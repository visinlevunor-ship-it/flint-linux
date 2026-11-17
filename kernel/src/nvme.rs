//! NVMe SSDs: one admin queue and one I/O queue pair, polled (no interrupts).

use crate::block::Disk;
use crate::mem;
use crate::pci::Dev;
use alloc::boxed::Box;
use alloc::string::String;
use core::ptr::{read_volatile, write_volatile};

const QSIZE: u16 = 64;
const BOUNCE: usize = 64 * 1024;

struct Queue {
    sq: u64,
    cq: u64,
    tail: u16,
    head: u16,
    phase: bool,
    sq_db: *mut u32,
    cq_db: *mut u32,
}

pub struct Nvme {
    regs: *mut u8,
    admin: Queue,
    io: Queue,
    cid: u16,
    lba_shift: u32,
    sectors: u64,
    bounce: u64,
    prp_list: u64,
    n: usize,
}

fn v(p: u64) -> *mut u8 {
    mem::phys_to_virt(p)
}

unsafe fn r32(b: *mut u8, off: usize) -> u32 {
    read_volatile(b.add(off) as *const u32)
}
unsafe fn w32(b: *mut u8, off: usize, val: u32) {
    write_volatile(b.add(off) as *mut u32, val)
}
unsafe fn r64(b: *mut u8, off: usize) -> u64 {
    read_volatile(b.add(off) as *const u32) as u64 | (read_volatile(b.add(off + 4) as *const u32) as u64) << 32
}
unsafe fn w64(b: *mut u8, off: usize, val: u64) {
    write_volatile(b.add(off) as *mut u32, val as u32);
    write_volatile(b.add(off + 4) as *mut u32, (val >> 32) as u32);
}

pub fn probe(d: &Dev, n: usize) -> Option<Box<dyn Disk>> {
    if !(d.class == 1 && d.subclass == 8 && d.prog_if == 2) {
        return None;
    }
    let (bar, io) = d.bar(0);
    if io || bar == 0 {
        return None;
    }
    d.enable();
    let regs = mem::map_mmio(bar, 0x4000);
    // SAFETY: the controller's registers, mapped uncached; our queue memory.
    unsafe {
        let cap = r64(regs, 0);
        let dstrd = ((cap >> 32) & 0xF) as usize;
        let timeout = ((cap >> 24) & 0xFF) * 500; // ms
        let wait_ready = |want: bool| -> bool {
            let start = crate::syscall::now_ns();
            while (r32(regs, 0x1C) & 1 != 0) != want {
                if crate::syscall::now_ns() - start > (timeout.max(1000) + 1000) * 1_000_000 {
                    return false;
                }
                core::hint::spin_loop();
            }
            true
        };
        w32(regs, 0x14, r32(regs, 0x14) & !1); // disable
        if !wait_ready(false) {
            return None;
        }
        let db = |q: usize, cq: bool| regs.add(0x1000 + (2 * q + cq as usize) * (4 << dstrd)) as *mut u32;
        let mk = |q: usize| Queue { sq: mem::alloc_frame(), cq: mem::alloc_frame(), tail: 0, head: 0, phase: true, sq_db: db(q, false), cq_db: db(q, true) };
        let admin = mk(0);
        w32(regs, 0x24, (QSIZE as u32 - 1) | (QSIZE as u32 - 1) << 16);
        w64(regs, 0x28, admin.sq);
        w64(regs, 0x30, admin.cq);
        // enable: NVM command set, 4 KiB pages, 64-byte entries, 16-byte completions
        w32(regs, 0x14, 1 | (6 << 16) | (4 << 20));
        if !wait_ready(true) {
            crate::kprintln!("[kernel] NVMe controller did not start");
            return None;
        }
        let mut nv = Nvme { regs, admin, io: mk(1), cid: 0, lba_shift: 9, sectors: 0, bounce: mem::alloc_contiguous((BOUNCE / 4096) as u64), prp_list: mem::alloc_frame(), n };
        // identify namespace 1: its size and sector size
        let id = mem::alloc_frame();
        if nv.admin_cmd(0x06, 1, id, [0, 0, 0, 0, 0, 0]).is_err() {
            return None;
        }
        let ns = v(id);
        nv.sectors = read_volatile(ns as *const u64);
        let flbas = read_volatile(ns.add(26)) & 0xF;
        let lbaf = read_volatile(ns.add(128 + 4 * flbas as usize) as *const u32);
        nv.lba_shift = (lbaf >> 16) & 0xFF;
        // the I/O queues: completion queue 1, then submission queue 1 on it
        let (iocq, iosq) = (nv.io.cq, nv.io.sq);
        if nv.admin_cmd(0x05, 0, iocq, [1 | ((QSIZE as u32 - 1) << 16), 1, 0, 0, 0, 0]).is_err()
            || nv.admin_cmd(0x01, 0, iosq, [1 | ((QSIZE as u32 - 1) << 16), 1 | (1 << 16), 0, 0, 0, 0]).is_err()
        {
            crate::kprintln!("[kernel] NVMe: cannot create the I/O queues");
            return None;
        }
        Some(Box::new(nv))
    }
}

impl Nvme {
    unsafe fn submit(q: &mut Queue, cmd: &[u32; 16]) -> Result<(), ()> {
        let slot = v(q.sq).add(q.tail as usize * 64) as *mut u32;
        for (i, w) in cmd.iter().enumerate() {
            write_volatile(slot.add(i), *w);
        }
        q.tail = (q.tail + 1) % QSIZE;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write_volatile(q.sq_db, q.tail as u32);
        let start = crate::syscall::now_ns();
        loop {
            let e = v(q.cq).add(q.head as usize * 16) as *const u32;
            let dw3 = read_volatile(e.add(3));
            if (dw3 >> 16) & 1 == q.phase as u32 {
                q.head = (q.head + 1) % QSIZE;
                if q.head == 0 {
                    q.phase = !q.phase;
                }
                write_volatile(q.cq_db, q.head as u32);
                return if (dw3 >> 17) & 0x7FFF == 0 { Ok(()) } else { Err(()) };
            }
            if crate::syscall::now_ns() - start > 10_000_000_000 {
                return Err(());
            }
            core::hint::spin_loop();
        }
    }

    fn admin_cmd(&mut self, op: u8, nsid: u32, prp1: u64, cdw: [u32; 6]) -> Result<(), ()> {
        self.cid = self.cid.wrapping_add(1);
        let mut c = [0u32; 16];
        c[0] = op as u32 | (self.cid as u32) << 16;
        c[1] = nsid;
        c[6] = prp1 as u32;
        c[7] = (prp1 >> 32) as u32;
        c[10..16].copy_from_slice(&cdw);
        // SAFETY: the admin queue is ours.
        unsafe { Self::submit(&mut self.admin, &c) }
    }

    /// Read or write `len` bytes at sector `lba` through the bounce buffer.
    fn io(&mut self, write: bool, lba: u64, len: usize) -> Result<(), ()> {
        let ss = 1usize << self.lba_shift;
        let nlb = (len / ss) as u32;
        let pages = len.div_ceil(4096);
        let prp2 = if pages <= 1 {
            0
        } else if pages == 2 {
            self.bounce + 4096
        } else {
            // a list of the pages after the first
            let l = v(self.prp_list) as *mut u64;
            for i in 1..pages {
                // SAFETY: the list page is ours; at most 15 entries.
                unsafe { write_volatile(l.add(i - 1), self.bounce + i as u64 * 4096) };
            }
            self.prp_list
        };
        self.cid = self.cid.wrapping_add(1);
        let mut c = [0u32; 16];
        c[0] = if write { 0x01 } else { 0x02 } | (self.cid as u32) << 16;
        c[1] = 1;
        c[6] = self.bounce as u32;
        c[7] = (self.bounce >> 32) as u32;
        c[8] = prp2 as u32;
        c[9] = (prp2 >> 32) as u32;
        c[10] = lba as u32;
        c[11] = (lba >> 32) as u32;
        c[12] = nlb - 1;
        // SAFETY: the I/O queue is ours.
        unsafe { Self::submit(&mut self.io, &c) }
    }
}

impl Disk for Nvme {
    fn name(&self) -> String {
        alloc::format!("nvme{}n1", self.n)
    }
    fn sector_size(&self) -> usize {
        1 << self.lba_shift
    }
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        let ss = self.sector_size();
        for (i, chunk) in buf.chunks_mut(BOUNCE).enumerate() {
            self.io(false, lba + (i * BOUNCE / ss) as u64, chunk.len())?;
            // SAFETY: the bounce buffer holds the data.
            unsafe { core::ptr::copy_nonoverlapping(v(self.bounce), chunk.as_mut_ptr(), chunk.len()) };
        }
        Ok(())
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        let ss = self.sector_size();
        for (i, chunk) in buf.chunks(BOUNCE).enumerate() {
            // SAFETY: as above.
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), v(self.bounce), chunk.len()) };
            self.io(true, lba + (i * BOUNCE / ss) as u64, chunk.len())?;
        }
        Ok(())
    }
}
