//! Wired network cards, polled: Intel's e1000 family (8254x, 82574 and others that keep the
//! legacy descriptors) and virtio-net (legacy interface).

use crate::mem;
use crate::pci::{self, Dev};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

pub trait Nic {
    fn mac(&self) -> [u8; 6];
    fn name(&self) -> &'static str;
    fn send(&mut self, frame: &[u8]);
    fn recv(&mut self) -> Option<Vec<u8>>;
    fn link_up(&self) -> bool;
}

fn v(p: u64) -> *mut u8 {
    mem::phys_to_virt(p)
}

// ---------------------------------------------------------------------------------------
// Intel e1000 / e1000e

const N: usize = 64;

pub struct E1000 {
    regs: *mut u8,
    rx: u64,
    tx: u64,
    rx_bufs: u64, // N pages... 2 KiB each, 32 per 64 KiB
    tx_bufs: u64,
    rx_next: usize,
    tx_next: usize,
    mac: [u8; 6],
}

unsafe fn r32(b: *mut u8, off: usize) -> u32 {
    read_volatile(b.add(off) as *const u32)
}
unsafe fn w32(b: *mut u8, off: usize, val: u32) {
    write_volatile(b.add(off) as *mut u32, val)
}

const E1000_IDS: &[u16] = &[
    0x100E, 0x100F, 0x1004, 0x100C, 0x1010, 0x1011, 0x1015, 0x1019, 0x101E, 0x1026, 0x1075, 0x1076, 0x1079, 0x107C, 0x105E,
    0x10D3, 0x10F6, 0x150C, 0x1502, 0x1503, 0x153A, 0x153B, 0x15A0, 0x15A1, 0x15A2, 0x15A3, 0x156F, 0x1570, 0x15B7, 0x15B8, 0x15B9,
    0x15BB, 0x15BC, 0x15BD, 0x15BE, 0x15D6, 0x15D7, 0x15D8, 0x15E3, 0x0D4C, 0x0D4D, 0x0D4E, 0x0D4F, 0x0D53, 0x0D55, 0x0DC5, 0x0DC6,
    0x0DC7, 0x0DC8, 0x15F9, 0x15FA, 0x15FB, 0x15FC, 0x1A1C, 0x1A1D, 0x1A1E, 0x1A1F, 0x550A, 0x550B, 0x550C, 0x550D,
];

pub fn probe_e1000(d: &Dev) -> Option<Box<dyn Nic>> {
    if d.vendor != 0x8086 || !E1000_IDS.contains(&d.device) {
        return None;
    }
    let (bar, io) = d.bar(0);
    if io || bar == 0 {
        return None;
    }
    d.enable();
    let regs = mem::map_mmio(bar, 0x20000);
    // SAFETY: the card's registers, mapped uncached; our descriptor rings and buffers.
    unsafe {
        // reset, then link up
        w32(regs, 0xD8, 0xFFFF_FFFF); // interrupts off
        w32(regs, 0x0, r32(regs, 0x0) | (1 << 26));
        let start = crate::syscall::now_ns();
        while r32(regs, 0x0) & (1 << 26) != 0 && crate::syscall::now_ns() - start < 100_000_000 {}
        w32(regs, 0xD8, 0xFFFF_FFFF);
        w32(regs, 0x0, (r32(regs, 0x0) | (1 << 6)) & !(1 << 3)); // SLU, no link reset
        let lo = r32(regs, 0x5400);
        let hi = r32(regs, 0x5404);
        let mac = [lo as u8, (lo >> 8) as u8, (lo >> 16) as u8, (lo >> 24) as u8, hi as u8, (hi >> 8) as u8];
        w32(regs, 0x5404, hi | (1 << 31)); // address valid
        for i in 0..128 {
            w32(regs, 0x5200 + i * 4, 0); // multicast table
        }
        let rx = mem::alloc_frame();
        let tx = mem::alloc_frame();
        let rx_bufs = mem::alloc_contiguous((N * 2048 / 4096) as u64);
        let tx_bufs = mem::alloc_contiguous((N * 2048 / 4096) as u64);
        for i in 0..N {
            let d = v(rx).add(i * 16) as *mut u64;
            write_volatile(d, rx_bufs + (i * 2048) as u64);
            write_volatile(d.add(1), 0);
        }
        w32(regs, 0x2800, rx as u32);
        w32(regs, 0x2804, (rx >> 32) as u32);
        w32(regs, 0x2808, (N * 16) as u32);
        w32(regs, 0x2810, 0);
        w32(regs, 0x2818, (N - 1) as u32);
        // receive: on, broadcasts, 2 KiB buffers, strip the CRC
        w32(regs, 0x100, (1 << 1) | (1 << 15) | (1 << 26));
        w32(regs, 0x3800, tx as u32);
        w32(regs, 0x3804, (tx >> 32) as u32);
        w32(regs, 0x3808, (N * 16) as u32);
        w32(regs, 0x3810, 0);
        w32(regs, 0x3818, 0);
        w32(regs, 0x400, (1 << 1) | (1 << 3) | (0x0F << 4) | (0x40 << 12)); // on, pad short packets
        w32(regs, 0x410, 0x0060_200A);
        Some(Box::new(E1000 { regs, rx, tx, rx_bufs, tx_bufs, rx_next: 0, tx_next: 0, mac }))
    }
}

impl Nic for E1000 {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn name(&self) -> &'static str {
        "Intel e1000"
    }
    fn link_up(&self) -> bool {
        // SAFETY: the status register.
        unsafe { r32(self.regs, 0x8) & 2 != 0 }
    }
    fn send(&mut self, frame: &[u8]) {
        let len = frame.len().min(2048);
        // SAFETY: our ring and buffers.
        unsafe {
            let i = self.tx_next;
            let d = v(self.tx).add(i * 16);
            // wait for the slot if the card still owns it
            let start = crate::syscall::now_ns();
            while read_volatile(d.add(11)) & 1 == 0 && read_volatile(d.add(10)) & 8 != 0 && crate::syscall::now_ns() - start < 10_000_000 {}
            let buf = self.tx_bufs + (i * 2048) as u64;
            core::ptr::copy_nonoverlapping(frame.as_ptr(), v(buf), len);
            write_volatile(d as *mut u64, buf);
            write_volatile(d.add(8) as *mut u16, len as u16);
            write_volatile(d.add(10), 0);
            write_volatile(d.add(11), 1 | 2 | 8); // EOP, insert CRC, report status
            write_volatile(d.add(12), 0);
            self.tx_next = (i + 1) % N;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            w32(self.regs, 0x3818, self.tx_next as u32);
        }
    }
    fn recv(&mut self) -> Option<Vec<u8>> {
        // SAFETY: our ring and buffers.
        unsafe {
            let i = self.rx_next;
            let d = v(self.rx).add(i * 16);
            let status = read_volatile(d.add(12));
            if status & 1 == 0 {
                return None;
            }
            let len = read_volatile(d.add(8) as *const u16) as usize;
            let buf = self.rx_bufs + (i * 2048) as u64;
            let out = core::slice::from_raw_parts(v(buf), len.min(2048)).to_vec();
            write_volatile(d.add(12), 0);
            self.rx_next = (i + 1) % N;
            w32(self.regs, 0x2818, i as u32);
            Some(out)
        }
    }
}

// ---------------------------------------------------------------------------------------
// virtio-net (legacy interface)

struct Vq {
    base: u64,
    size: u16,
    avail: u64,
    used: u64,
    last_used: u16,
}

pub struct VirtioNet {
    io: u16,
    rx: Vq,
    tx: Vq,
    rx_bufs: u64,
    tx_bufs: u64,
    tx_next: u16,
    mac: [u8; 6],
}

const VBUF: usize = 2048;

unsafe fn setup_vq(io: u16, q: u16) -> Option<Vq> {
    pci::outw(io + 0x0E, q);
    let size = pci::inw(io + 0x0C);
    if size == 0 {
        return None;
    }
    let desc = 16 * size as u64;
    let avail = 6 + 2 * size as u64;
    let used_off = (desc + avail + 4095) & !4095;
    let total = used_off + ((6 + 8 * size as u64 + 4095) & !4095);
    let base = mem::alloc_contiguous(total / 4096);
    pci::outd(io + 0x08, (base / 4096) as u32);
    Some(Vq { base, size, avail: base + desc, used: base + used_off, last_used: 0 })
}

pub fn probe_virtio(d: &Dev) -> Option<Box<dyn Nic>> {
    if d.vendor != 0x1AF4 || !(d.device == 0x1000 || d.device == 0x1041) {
        return None;
    }
    let (bar, io) = d.bar(0);
    if !io {
        return None;
    }
    d.enable();
    let io = bar as u16;
    // SAFETY: the device's legacy registers; our queues and buffers.
    unsafe {
        pci::outbp(io + 0x12, 0);
        pci::outbp(io + 0x12, 1 | 2);
        let feat = pci::ind(io);
        pci::outd(io + 4, feat & (1 << 5)); // MAC only
        let rx = setup_vq(io, 0)?;
        let tx = setup_vq(io, 1)?;
        let n = rx.size.min(64) as usize;
        let rx_bufs = mem::alloc_contiguous((n * VBUF / 4096) as u64);
        let tx_bufs = mem::alloc_contiguous((tx.size.min(64) as usize * VBUF / 4096) as u64);
        let mut mac = [0u8; 6];
        for (i, m) in mac.iter_mut().enumerate() {
            *m = pci::inbp(io + 0x14 + i as u16);
        }
        let mut nic = VirtioNet { io, rx, tx, rx_bufs, tx_bufs, tx_next: 0, mac };
        for i in 0..n {
            nic.give_rx(i as u16);
        }
        pci::outbp(io + 0x12, 1 | 2 | 4);
        pci::outw(io + 0x10, 0);
        Some(Box::new(nic))
    }
}

impl VirtioNet {
    unsafe fn give_rx(&mut self, i: u16) {
        let d = v(self.rx.base).add(i as usize * 16) as *mut u64;
        write_volatile(d, self.rx_bufs + i as u64 * VBUF as u64);
        write_volatile(d.add(1), VBUF as u64 | (2u64 << 32)); // device writes
        let av = v(self.rx.avail) as *mut u16;
        let idx = read_volatile(av.add(1));
        write_volatile(av.add(2 + (idx % self.rx.size) as usize), i);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write_volatile(av.add(1), idx.wrapping_add(1));
    }
}

impl Nic for VirtioNet {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn name(&self) -> &'static str {
        "virtio-net"
    }
    fn link_up(&self) -> bool {
        true
    }
    fn send(&mut self, frame: &[u8]) {
        let n = self.tx.size.min(64);
        let i = self.tx_next % n;
        self.tx_next = self.tx_next.wrapping_add(1);
        let len = frame.len().min(VBUF - 10);
        // SAFETY: our queue and buffers.
        unsafe {
            let buf = self.tx_bufs + i as u64 * VBUF as u64;
            core::ptr::write_bytes(v(buf), 0, 10); // virtio_net_hdr
            core::ptr::copy_nonoverlapping(frame.as_ptr(), v(buf).add(10), len);
            let d = v(self.tx.base).add(i as usize * 16) as *mut u64;
            write_volatile(d, buf);
            write_volatile(d.add(1), (len + 10) as u64);
            let av = v(self.tx.avail) as *mut u16;
            let idx = read_volatile(av.add(1));
            write_volatile(av.add(2 + (idx % self.tx.size) as usize), i);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            write_volatile(av.add(1), idx.wrapping_add(1));
            pci::outw(self.io + 0x10, 1);
        }
    }
    fn recv(&mut self) -> Option<Vec<u8>> {
        // SAFETY: our queue and buffers.
        unsafe {
            let used = v(self.rx.used) as *mut u16;
            if read_volatile(used.add(1)) == self.rx.last_used {
                return None;
            }
            let e = (v(self.rx.used).add(4) as *const u32).add((self.rx.last_used % self.rx.size) as usize * 2);
            let id = read_volatile(e) as u16;
            let len = read_volatile(e.add(1)) as usize;
            self.rx.last_used = self.rx.last_used.wrapping_add(1);
            let buf = self.rx_bufs + id as u64 * VBUF as u64;
            let out = if len > 10 { core::slice::from_raw_parts(v(buf).add(10), (len - 10).min(VBUF - 10)).to_vec() } else { Vec::new() };
            self.give_rx(id);
            pci::outw(self.io + 0x10, 0);
            Some(out)
        }
    }
}
