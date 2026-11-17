//! virtio-blk (QEMU's and other hypervisors' disks), through the legacy I/O-port interface
//! transitional devices offer. Polled: no interrupts.

use crate::block::Disk;
use crate::mem;
use crate::pci::{self, Dev};
use alloc::boxed::Box;
use alloc::string::String;
use core::ptr::{read_volatile, write_volatile};

const BOUNCE: usize = 64 * 1024;

pub struct VirtioBlk {
    io: u16,
    qsize: u16,
    desc: u64,  // physical address of the queue
    avail: u64, // offsets of the rings inside it
    used: u64,
    last_used: u16,
    req: u64,    // a page: header (16 bytes) and status byte
    bounce: u64, // 64 KiB, physically contiguous
    sectors: u64,
    n: usize,
}

fn v(p: u64) -> *mut u8 {
    mem::phys_to_virt(p)
}

pub fn probe(d: &Dev, n: usize) -> Option<Box<dyn Disk>> {
    if d.vendor != 0x1AF4 || !(d.device == 0x1001 || d.device == 0x1042) {
        return None;
    }
    let (bar, io) = d.bar(0);
    if !io {
        crate::kprintln!("[kernel] virtio-blk without the legacy interface: not supported yet");
        return None;
    }
    d.enable();
    let io = bar as u16;
    // SAFETY: the device's legacy register block at its I/O BAR.
    unsafe {
        pci::outbp(io + 0x12, 0); // reset
        pci::outbp(io + 0x12, 1 | 2); // ACKNOWLEDGE | DRIVER
        let _features = pci::ind(io);
        pci::outd(io + 4, 0); // no optional features
        pci::outw(io + 0x0E, 0); // queue 0
        let qsize = pci::inw(io + 0x0C);
        if qsize == 0 {
            return None;
        }
        let desc_bytes = 16 * qsize as u64;
        let avail_bytes = 6 + 2 * qsize as u64;
        let used_off = (desc_bytes + avail_bytes + 4095) & !4095;
        let total = used_off + ((6 + 8 * qsize as u64 + 4095) & !4095);
        let q = mem::alloc_contiguous(total / 4096);
        pci::outd(io + 0x08, (q / 4096) as u32);
        let req = mem::alloc_frame();
        let bounce = mem::alloc_contiguous((BOUNCE / 4096) as u64);
        pci::outbp(io + 0x12, 1 | 2 | 4); // DRIVER_OK
        let lo = pci::ind(io + 0x14) as u64;
        let hi = pci::ind(io + 0x18) as u64;
        Some(Box::new(VirtioBlk { io, qsize, desc: q, avail: q + desc_bytes, used: q + used_off, last_used: 0, req, bounce, sectors: lo | hi << 32, n }))
    }
}

impl VirtioBlk {
    /// One request: `write` or read `len` bytes at sector `lba` through the bounce buffer.
    fn request(&mut self, write: bool, lba: u64, len: usize) -> Result<(), ()> {
        // SAFETY: the queue and buffers are ours, physically contiguous, mapped at PHYS_OFF.
        unsafe {
            let hdr = v(self.req);
            write_volatile(hdr as *mut u32, if write { 1 } else { 0 });
            write_volatile(hdr.add(4) as *mut u32, 0);
            write_volatile(hdr.add(8) as *mut u64, lba);
            write_volatile(hdr.add(16), 0xFF); // status
            let d = v(self.desc) as *mut u64;
            // descriptor: addr u64, len u32, flags u16, next u16
            let set = |i: usize, addr: u64, len: u32, flags: u16, next: u16| {
                write_volatile(d.add(i * 2), addr);
                write_volatile(d.add(i * 2 + 1), len as u64 | (flags as u64) << 32 | (next as u64) << 48);
            };
            set(0, self.req, 16, 1, 1); // NEXT
            set(1, self.bounce, len as u32, 1 | if write { 0 } else { 2 }, 2); // NEXT (| WRITE: device writes)
            set(2, self.req + 16, 1, 2, 0); // WRITE
            let av = v(self.avail) as *mut u16;
            let idx = read_volatile(av.add(1));
            write_volatile(av.add(2 + (idx % self.qsize) as usize), 0);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            write_volatile(av.add(1), idx.wrapping_add(1));
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            pci::outw(self.io + 0x10, 0);
            let used = v(self.used) as *mut u16;
            let mut spins = 0u64;
            while read_volatile(used.add(1)) == self.last_used {
                core::hint::spin_loop();
                spins += 1;
                if spins > 2_000_000_000 {
                    return Err(());
                }
            }
            self.last_used = self.last_used.wrapping_add(1);
            if read_volatile(hdr.add(16)) != 0 {
                return Err(());
            }
        }
        Ok(())
    }
}

impl Disk for VirtioBlk {
    fn name(&self) -> String {
        alloc::format!("vd{}", (b'a' + self.n as u8) as char)
    }
    fn sector_size(&self) -> usize {
        512
    }
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        for (i, chunk) in buf.chunks_mut(BOUNCE).enumerate() {
            self.request(false, lba + (i * BOUNCE / 512) as u64, chunk.len())?;
            // SAFETY: the bounce buffer holds chunk.len() bytes from the device.
            unsafe { core::ptr::copy_nonoverlapping(v(self.bounce), chunk.as_mut_ptr(), chunk.len()) };
        }
        Ok(())
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        for (i, chunk) in buf.chunks(BOUNCE).enumerate() {
            // SAFETY: as above.
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), v(self.bounce), chunk.len()) };
            self.request(true, lba + (i * BOUNCE / 512) as u64, chunk.len())?;
        }
        Ok(())
    }
}
