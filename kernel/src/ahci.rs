//! AHCI: SATA disks (and SSDs) on the standard SATA controller. Polled: no interrupts.

use crate::block::Disk;
use crate::mem;
use crate::pci::Dev;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

const BOUNCE: usize = 64 * 1024;

pub struct AhciDisk {
    port: *mut u32, // the port's registers
    cl: u64,        // command list (1 KiB) and FIS area (256 B) in one page
    ct: u64,        // command table, one page
    bounce: u64,
    sectors: u64,
    n: usize,
}

fn v(p: u64) -> *mut u8 {
    mem::phys_to_virt(p)
}

unsafe fn rd(base: *mut u32, off: usize) -> u32 {
    read_volatile(base.add(off / 4))
}
unsafe fn wr(base: *mut u32, off: usize, val: u32) {
    write_volatile(base.add(off / 4), val)
}

/// Every SATA disk on this controller.
pub fn probe(d: &Dev, first: usize) -> Vec<Box<dyn Disk>> {
    let mut out: Vec<Box<dyn Disk>> = Vec::new();
    if !(d.class == 1 && d.subclass == 6 && d.prog_if == 1) {
        return out;
    }
    let (abar, io) = d.bar(5);
    if io || abar == 0 {
        return out;
    }
    d.enable();
    let hba = mem::map_mmio(abar, 0x1100) as *mut u32;
    // SAFETY: the controller's registers, mapped uncached.
    unsafe {
        wr(hba, 0x04, rd(hba, 0x04) | (1 << 31)); // AHCI mode
        let pi = rd(hba, 0x0C);
        for i in 0..32 {
            if pi & (1 << i) == 0 {
                continue;
            }
            let port = hba.add((0x100 + i * 0x80) / 4);
            let ssts = rd(port, 0x28);
            if ssts & 0xF != 3 || rd(port, 0x24) != 0x0000_0101 {
                continue; // no device, or not a disk (ATAPI, port multiplier...)
            }
            // stop the port, give it our memory, start it
            wr(port, 0x18, rd(port, 0x18) & !(1 | (1 << 4)));
            let mut spins = 0;
            while rd(port, 0x18) & ((1 << 15) | (1 << 14)) != 0 && spins < 10_000_000 {
                spins += 1;
            }
            let cl = mem::alloc_frame();
            let ct = mem::alloc_frame();
            wr(port, 0x00, cl as u32);
            wr(port, 0x04, (cl >> 32) as u32);
            wr(port, 0x08, (cl + 1024) as u32);
            wr(port, 0x0C, ((cl + 1024) >> 32) as u32);
            wr(port, 0x30, 0xFFFF_FFFF); // clear errors
            wr(port, 0x10, 0xFFFF_FFFF);
            wr(port, 0x18, rd(port, 0x18) | (1 << 4)); // FRE
            wr(port, 0x18, rd(port, 0x18) | 1); // ST
            let bounce = mem::alloc_contiguous((BOUNCE / 4096) as u64);
            let mut disk = AhciDisk { port, cl, ct, bounce, sectors: 0, n: first + out.len() };
            // IDENTIFY DEVICE: the size
            if disk.command(0xEC, 0, 512, false).is_err() {
                continue;
            }
            let id = v(bounce) as *const u16;
            let words = |i: usize| read_volatile(id.add(i)) as u64;
            disk.sectors = words(100) | words(101) << 16 | words(102) << 32 | words(103) << 48;
            if disk.sectors == 0 {
                disk.sectors = words(60) | words(61) << 16;
            }
            out.push(Box::new(disk));
        }
    }
    out
}

impl AhciDisk {
    /// An ATA command through slot 0: `bytes` of data in or out of the bounce buffer.
    fn command(&mut self, cmd: u8, lba: u64, bytes: usize, write: bool) -> Result<(), ()> {
        // SAFETY: our command list, table and buffer; the port's registers.
        unsafe {
            let hdr = v(self.cl) as *mut u32;
            // flags: FIS length 5 dwords, write, one PRD entry
            write_volatile(hdr, 5 | if write { 1 << 6 } else { 0 } | (1 << 16));
            write_volatile(hdr.add(1), 0);
            write_volatile(hdr.add(2), self.ct as u32);
            write_volatile(hdr.add(3), (self.ct >> 32) as u32);
            let t = v(self.ct);
            core::ptr::write_bytes(t, 0, 0x100);
            let count = (bytes / 512) as u16;
            let fis = [
                0x27, 0x80, cmd, 0, lba as u8, (lba >> 8) as u8, (lba >> 16) as u8, 0x40,
                (lba >> 24) as u8, (lba >> 32) as u8, (lba >> 40) as u8, 0,
                count as u8, (count >> 8) as u8, 0, 0,
            ];
            for (i, b) in fis.iter().enumerate() {
                write_volatile(t.add(i), *b);
            }
            let prd = t.add(0x80) as *mut u32;
            write_volatile(prd, self.bounce as u32);
            write_volatile(prd.add(1), (self.bounce >> 32) as u32);
            write_volatile(prd.add(2), 0);
            write_volatile(prd.add(3), (bytes as u32 - 1) & 0x3F_FFFF);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            let mut spins = 0u64;
            while rd(self.port, 0x20) & 0x88 != 0 && spins < 100_000_000 {
                spins += 1; // busy
            }
            wr(self.port, 0x10, 0xFFFF_FFFF);
            wr(self.port, 0x38, 1);
            spins = 0;
            while rd(self.port, 0x38) & 1 != 0 {
                if rd(self.port, 0x10) & (1 << 30) != 0 || spins > 2_000_000_000 {
                    return Err(()); // task file error
                }
                spins += 1;
                core::hint::spin_loop();
            }
            if rd(self.port, 0x20) & 1 != 0 {
                return Err(());
            }
        }
        Ok(())
    }
}

impl Disk for AhciDisk {
    fn name(&self) -> String {
        alloc::format!("sd{}", (b'a' + self.n as u8) as char)
    }
    fn sector_size(&self) -> usize {
        512
    }
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        for (i, chunk) in buf.chunks_mut(BOUNCE).enumerate() {
            self.command(0x25, lba + (i * BOUNCE / 512) as u64, chunk.len(), false)?; // READ DMA EXT
            // SAFETY: the bounce buffer holds the data.
            unsafe { core::ptr::copy_nonoverlapping(v(self.bounce), chunk.as_mut_ptr(), chunk.len()) };
        }
        Ok(())
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        for (i, chunk) in buf.chunks(BOUNCE).enumerate() {
            // SAFETY: as above.
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), v(self.bounce), chunk.len()) };
            self.command(0x35, lba + (i * BOUNCE / 512) as u64, chunk.len(), true)?; // WRITE DMA EXT
        }
        Ok(())
    }
}
