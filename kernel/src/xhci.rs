//! USB through xHCI (USB 3 host controllers, which also run USB 1 and 2 devices), polled:
//! keyboards plugged straight into the computer's ports, in the HID boot protocol. Keyboards
//! behind a USB hub are not supported yet.

use crate::mem;
use crate::pci::Dev;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

const RING: usize = 256; // TRBs per ring (one page)

fn v(p: u64) -> *mut u8 {
    mem::phys_to_virt(p)
}

struct Ring {
    base: u64,
    idx: usize,
    cycle: bool,
}

impl Ring {
    fn new() -> Ring {
        let base = mem::alloc_frame();
        // the last TRB links back to the start, toggling the cycle bit
        // SAFETY: a fresh page of our own.
        unsafe {
            let link = v(base).add((RING - 1) * 16) as *mut u32;
            write_volatile(link, base as u32);
            write_volatile(link.add(1), (base >> 32) as u32);
            write_volatile(link.add(3), (6 << 10) | 2); // Link TRB, Toggle Cycle
        }
        Ring { base, idx: 0, cycle: true }
    }

    /// Put a TRB on the ring; returns its physical address.
    fn push(&mut self, t: [u32; 4]) -> u64 {
        // SAFETY: inside our ring page.
        unsafe {
            let p = v(self.base).add(self.idx * 16) as *mut u32;
            write_volatile(p, t[0]);
            write_volatile(p.add(1), t[1]);
            write_volatile(p.add(2), t[2]);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            write_volatile(p.add(3), (t[3] & !1) | self.cycle as u32);
            let addr = self.base + self.idx as u64 * 16;
            self.idx += 1;
            if self.idx == RING - 1 {
                // hand the link TRB over, then wrap
                let link = v(self.base).add((RING - 1) * 16) as *mut u32;
                let ctl = read_volatile(link.add(3));
                write_volatile(link.add(3), (ctl & !1) | self.cycle as u32);
                self.idx = 0;
                self.cycle = !self.cycle;
            }
            addr
        }
    }
}

struct Keyboard {
    slot: u8,
    dci: u8,
    ring: Ring,
    buf: u64,
    last: [u8; 8],
}

pub struct Xhci {
    op: *mut u8,
    db: *mut u32,
    rt: *mut u8,
    ctx64: bool,
    cmd: Ring,
    ev_base: u64,
    ev_idx: usize,
    ev_cycle: bool,
    dcbaa: u64,
    kbds: Vec<Keyboard>,
}

static mut HCS: Vec<Xhci> = Vec::new();

unsafe fn r32(b: *mut u8, off: usize) -> u32 {
    read_volatile(b.add(off) as *const u32)
}
unsafe fn w32(b: *mut u8, off: usize, val: u32) {
    write_volatile(b.add(off) as *mut u32, val)
}
unsafe fn w64(b: *mut u8, off: usize, val: u64) {
    write_volatile(b.add(off) as *mut u32, val as u32);
    write_volatile(b.add(off + 4) as *mut u32, (val >> 32) as u32);
}

fn wait(mut f: impl FnMut() -> bool, ms: u64) -> bool {
    let start = crate::syscall::now_ns();
    while !f() {
        if crate::syscall::now_ns() - start > ms * 1_000_000 {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

fn sleep_ms(ms: u64) {
    let start = crate::syscall::now_ns();
    while crate::syscall::now_ns() - start < ms * 1_000_000 {
        core::hint::spin_loop();
    }
}

pub fn probe(d: &Dev) {
    if !(d.class == 0x0C && d.subclass == 0x03 && d.prog_if == 0x30) {
        return;
    }
    let (bar, io) = d.bar(0);
    if io || bar == 0 {
        return;
    }
    d.enable();
    let cap = mem::map_mmio(bar, 0x10000);
    // SAFETY: the controller's registers, mapped uncached; our own DMA memory.
    unsafe {
        let caplen = (r32(cap, 0) & 0xFF) as usize;
        let hcs1 = r32(cap, 4);
        let hcs2 = r32(cap, 8);
        let hcc1 = r32(cap, 0x10);
        let op = cap.add(caplen);
        let db = cap.add((r32(cap, 0x14) & !3) as usize) as *mut u32;
        let rt = cap.add((r32(cap, 0x18) & !0x1F) as usize);
        let max_slots = (hcs1 & 0xFF) as u32;
        let max_ports = (hcs1 >> 24) as usize;
        // take the controller from the firmware (USB legacy support capability)
        let mut ecp = ((hcc1 >> 16) & 0xFFFF) as usize * 4;
        while ecp != 0 {
            let c = r32(cap, ecp);
            if c & 0xFF == 1 {
                w32(cap, ecp, c | (1 << 24)); // OS owned
                wait(|| r32(cap, ecp) & (1 << 16) == 0, 1000);
                w32(cap, ecp + 4, r32(cap, ecp + 4) & 0x000E_1FEE); // its SMIs off
            }
            let next = ((c >> 8) & 0xFF) as usize;
            ecp = if next == 0 { 0 } else { ecp + next * 4 };
        }
        // stop and reset
        w32(op, 0, r32(op, 0) & !1);
        wait(|| r32(op, 4) & 1 != 0, 100);
        w32(op, 0, 2);
        if !wait(|| r32(op, 0) & 2 == 0 && r32(op, 4) & (1 << 11) == 0, 1000) {
            crate::kprintln!("[kernel] USB controller did not reset");
            return;
        }
        w32(op, 0x38, max_slots);
        let dcbaa = mem::alloc_frame();
        // scratchpad buffers the controller asks for
        let scratch = (((hcs2 >> 21) & 0x1F) << 5 | (hcs2 >> 27) & 0x1F) as usize;
        if scratch > 0 {
            let arr = mem::alloc_frame();
            for i in 0..scratch {
                write_volatile((v(arr) as *mut u64).add(i), mem::alloc_frame());
            }
            write_volatile(v(dcbaa) as *mut u64, arr);
        }
        w64(op, 0x30, dcbaa);
        let cmd = Ring::new();
        w64(op, 0x18, cmd.base | 1);
        // one event ring segment
        let ev = mem::alloc_frame();
        let erst = mem::alloc_frame();
        write_volatile(v(erst) as *mut u64, ev);
        write_volatile(v(erst).add(8) as *mut u32, RING as u32);
        let ir = rt.add(0x20);
        w32(ir, 0x08, 1); // ERSTSZ
        w64(ir, 0x18, ev); // ERDP
        w64(ir, 0x10, erst); // ERSTBA
        w32(op, 0, 1); // run
        wait(|| r32(op, 4) & 1 == 0, 100);
        let mut hc = Xhci { op, db, rt, ctx64: hcc1 & 4 != 0, cmd, ev_base: ev, ev_idx: 0, ev_cycle: true, dcbaa, kbds: Vec::new() };
        sleep_ms(50);
        for port in 0..max_ports {
            hc.port(port);
        }
        if !hc.kbds.is_empty() {
            crate::kprintln!("[kernel] USB: {} keyboard(s)", hc.kbds.len());
        }
        (*core::ptr::addr_of_mut!(HCS)).push(hc);
    }
}

impl Xhci {
    fn portsc(&self, p: usize) -> *mut u8 {
        // SAFETY: inside the operational registers.
        unsafe { self.op.add(0x400 + p * 0x10) }
    }

    /// The next event, if any: (TRB words).
    fn event(&mut self) -> Option<[u32; 4]> {
        // SAFETY: our event ring; the interrupter's registers.
        unsafe {
            let p = v(self.ev_base).add(self.ev_idx * 16) as *const u32;
            let ctl = read_volatile(p.add(3));
            if (ctl & 1 != 0) != self.ev_cycle {
                return None;
            }
            let t = [read_volatile(p), read_volatile(p.add(1)), read_volatile(p.add(2)), ctl];
            self.ev_idx += 1;
            if self.ev_idx == RING {
                self.ev_idx = 0;
                self.ev_cycle = !self.ev_cycle;
            }
            w64(self.rt.add(0x20), 0x18, (self.ev_base + self.ev_idx as u64 * 16) | 8);
            Some(t)
        }
    }

    /// Wait for the completion event of the TRB at `trb`: (completion code, slot / length).
    fn wait_for(&mut self, trb: u64, ms: u64) -> Option<(u32, u32, [u32; 4])> {
        let start = crate::syscall::now_ns();
        loop {
            while let Some(e) = self.event() {
                let ty = (e[3] >> 10) & 0x3F;
                let ptr = e[0] as u64 | (e[1] as u64) << 32;
                if (ty == 33 || ty == 32) && ptr == trb {
                    return Some((e[2] >> 24, e[3] >> 24, e));
                }
                if ty == 32 {
                    self.keyboard_event(&e);
                }
            }
            if crate::syscall::now_ns() - start > ms * 1_000_000 {
                return None;
            }
        }
    }

    fn command(&mut self, t: [u32; 4]) -> Option<(u32, u32)> {
        let a = self.cmd.push(t);
        // SAFETY: doorbell 0 is the command ring's.
        unsafe { write_volatile(self.db, 0) };
        self.wait_for(a, 1000).map(|(cc, slot, _)| (cc, slot))
    }

    fn ctx(&self, base: u64, i: usize) -> *mut u32 {
        v(base).wrapping_add(i * if self.ctx64 { 64 } else { 32 }) as *mut u32
    }

    /// A device on root port `p` (0-based): reset it, address it, and if it is a boot
    /// keyboard set it up.
    fn port(&mut self, p: usize) {
        // SAFETY: port registers; our contexts and rings.
        unsafe {
            let sc = self.portsc(p);
            let mut st = r32(sc, 0);
            if st & 1 == 0 {
                return; // nothing connected
            }
            // reset (USB 3 ports enable themselves; resetting them is harmless)
            w32(sc, 0, (st & 0x0E00_C3E0) | (1 << 4));
            if !wait(|| r32(sc, 0) & (1 << 21) != 0, 500) {
                return;
            }
            w32(sc, 0, (r32(sc, 0) & 0x0E00_C3E0) | (1 << 21)); // clear "reset changed"
            sleep_ms(20);
            st = r32(sc, 0);
            if st & 2 == 0 {
                return; // not enabled
            }
            let speed = (st >> 10) & 0xF;
            let Some((cc, slot)) = self.command([0, 0, 0, 9 << 10]) else { return }; // Enable Slot
            if cc != 1 || slot == 0 {
                return;
            }
            // device context, and an input context for Address Device
            let out = mem::alloc_frame();
            write_volatile((v(self.dcbaa) as *mut u64).add(slot as usize), out);
            let input = mem::alloc_frame();
            let ep0 = Ring::new();
            let mps: u32 = match speed {
                2 => 8,         // low speed
                1 | 3 => 64,    // full, high
                _ => 512,       // super speed
            };
            let icc = self.ctx(input, 0);
            write_volatile(icc.add(1), 0b11); // add slot and EP0
            let slotc = self.ctx(input, 1);
            write_volatile(slotc, (speed << 20) | (1 << 27));
            write_volatile(slotc.add(1), ((p + 1) as u32) << 16);
            let ep = self.ctx(input, 2);
            write_volatile(ep.add(1), (3 << 1) | (4 << 3) | (mps << 16)); // CErr 3, control
            write_volatile(ep.add(2), ep0.base as u32 | 1);
            write_volatile(ep.add(3), (ep0.base >> 32) as u32);
            write_volatile(ep.add(4), 8);
            let Some((cc, _)) = self.command([input as u32, (input >> 32) as u32, 0, (11 << 10) | (slot << 24)]) else { return };
            if cc != 1 {
                return;
            }
            let mut dev = Dev0 { slot: slot as u8, ring: ep0 };
            let buf = mem::alloc_frame();
            // the configuration descriptor (first 255 bytes)
            if self.control(&mut dev, 0x80, 6, 0x0200, 0, buf, 255).is_none() {
                return;
            }
            let cfg = core::slice::from_raw_parts(v(buf), 255).to_vec();
            let total = (u16::from_le_bytes([cfg[2], cfg[3]]) as usize).min(255);
            let config_value = cfg[5];
            // a boot keyboard interface and its interrupt IN endpoint
            let mut off = 0;
            let mut iface: Option<u8> = None;
            let mut endpoint = None;
            while off + 2 <= total {
                let len = cfg[off] as usize;
                if len < 2 {
                    break;
                }
                match cfg[off + 1] {
                    4 => {
                        iface = if cfg[off + 5] == 3 && cfg[off + 6] == 1 && cfg[off + 7] == 1 { Some(cfg[off + 2]) } else { None };
                    }
                    5 if iface.is_some() && endpoint.is_none() => {
                        let addr = cfg[off + 2];
                        if addr & 0x80 != 0 && cfg[off + 3] & 3 == 3 {
                            endpoint = Some((addr & 0xF, u16::from_le_bytes([cfg[off + 4], cfg[off + 5]]) as u32 & 0x7FF, cfg[off + 6], iface.unwrap()));
                        }
                    }
                    _ => {}
                }
                off += len;
            }
            let Some((epn, ep_mps, interval, ifn)) = endpoint else { return };
            // SET_CONFIGURATION, SET_PROTOCOL(boot), SET_IDLE(0)
            if self.control(&mut dev, 0x00, 9, config_value as u16, 0, 0, 0).is_none() {
                return;
            }
            let _ = self.control(&mut dev, 0x21, 0x0B, 0, ifn as u16, 0, 0);
            let _ = self.control(&mut dev, 0x21, 0x0A, 0, ifn as u16, 0, 0);
            // Configure Endpoint: the interrupt IN endpoint
            let dci = epn * 2 + 1;
            core::ptr::write_bytes(v(input), 0, 4096);
            let icc = self.ctx(input, 0);
            write_volatile(icc.add(1), 1 | (1 << dci));
            let slotc = self.ctx(input, 1);
            write_volatile(slotc, (speed << 20) | ((dci as u32) << 27));
            write_volatile(slotc.add(1), ((p + 1) as u32) << 16);
            let ring = Ring::new();
            let epc = self.ctx(input, 1 + dci as usize);
            // interval: high/super speed counts in 2^(n-1) microframes; full/low speed in ms
            let iv = if speed >= 3 { (interval as u32).clamp(1, 16) - 1 } else { 3 + (31 - (interval.max(1) as u32 * 8).leading_zeros()) };
            write_volatile(epc, iv << 16);
            write_volatile(epc.add(1), (3 << 1) | (7 << 3) | (ep_mps << 16)); // interrupt IN
            write_volatile(epc.add(2), ring.base as u32 | 1);
            write_volatile(epc.add(3), (ring.base >> 32) as u32);
            write_volatile(epc.add(4), 8 | (ep_mps << 16));
            let Some((cc, _)) = self.command([input as u32, (input >> 32) as u32, 0, (12 << 10) | (slot << 24)]) else { return };
            if cc != 1 {
                return;
            }
            let report = mem::alloc_frame();
            let mut kb = Keyboard { slot: slot as u8, dci, ring, buf: report, last: [0; 8] };
            self.queue_report(&mut kb);
            self.kbds.push(kb);
        }
    }

    fn queue_report(&self, kb: &mut Keyboard) {
        kb.ring.push([kb.buf as u32, (kb.buf >> 32) as u32, 8, (1 << 10) | (1 << 5) | (1 << 2)]); // Normal, IOC, ISP
        // SAFETY: the device's doorbell.
        unsafe { write_volatile(self.db.add(kb.slot as usize), kb.dci as u32) };
    }

    /// A control transfer on EP0. `len` bytes in (bmRequestType bit 7) or none.
    fn control(&mut self, d: &mut Dev0, rt: u8, req: u8, value: u16, index: u16, buf: u64, len: u16) -> Option<()> {
        let setup0 = rt as u32 | (req as u32) << 8 | (value as u32) << 16;
        let setup1 = index as u32 | (len as u32) << 16;
        let trt = if len == 0 { 0 } else if rt & 0x80 != 0 { 3 } else { 2 };
        d.ring.push([setup0, setup1, 8, (2 << 10) | (1 << 6) | (trt << 16)]); // Setup, IDT
        if len > 0 {
            d.ring.push([buf as u32, (buf >> 32) as u32, len as u32, (3 << 10) | (((rt & 0x80 != 0) as u32) << 16)]);
        }
        let dir_in = len == 0 || rt & 0x80 == 0;
        let status = d.ring.push([0, 0, 0, (4 << 10) | (1 << 5) | ((dir_in as u32) << 16)]); // Status, IOC
        // SAFETY: the device's doorbell, EP0.
        unsafe { write_volatile(self.db.add(d.slot as usize), 1) };
        let (cc, _, _) = self.wait_for(status, 1000)?;
        if cc == 1 || cc == 13 { Some(()) } else { None }
    }

    fn keyboard_event(&mut self, e: &[u32; 4]) {
        let slot = (e[3] >> 24) as u8;
        let ep = ((e[3] >> 16) & 0x1F) as u8;
        let Some(i) = self.kbds.iter().position(|k| k.slot == slot && k.dci == ep) else { return };
        // SAFETY: the report buffer the controller just filled.
        let report: [u8; 8] = unsafe { core::ptr::read_volatile(v(self.kbds[i].buf) as *const [u8; 8]) };
        let last = self.kbds[i].last;
        for &k in &report[2..8] {
            if k > 3 && !last[2..8].contains(&k) {
                hid_key(report[0], k);
            }
        }
        self.kbds[i].last = report;
        let mut kb = core::mem::replace(&mut self.kbds[i], Keyboard { slot: 0, dci: 0, ring: Ring { base: 0, idx: 0, cycle: true }, buf: 0, last: [0; 8] });
        self.queue_report(&mut kb);
        self.kbds[i] = kb;
    }
}

struct Dev0 {
    slot: u8,
    ring: Ring,
}

/// Keyboards' reports, while idle and on every timer tick.
pub fn poll() {
    // SAFETY: one CPU.
    let hcs = unsafe { &mut *core::ptr::addr_of_mut!(HCS) };
    for hc in hcs.iter_mut() {
        while let Some(e) = hc.event() {
            if (e[3] >> 10) & 0x3F == 32 {
                hc.keyboard_event(&e);
            }
        }
    }
}

/// A key pressed: HID usage `k` with modifier bits `m`, as the terminal's bytes (US layout).
fn hid_key(m: u8, k: u8) {
    let shift = m & 0x22 != 0;
    let ctrl = m & 0x11 != 0;
    let alt = m & 0x44 != 0;
    let send = |b: &[u8]| {
        for &c in b {
            crate::tty::input(c);
        }
    };
    const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz1234567890\n\x1b\x7f\t -=[]\\#;'`,./";
    const UPPER: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()\n\x1b\x7f\t _+{}|~:\"~<>?";
    match k {
        0x04..=0x38 => {
            let i = (k - 0x04) as usize;
            let mut c = if shift { UPPER[i] } else { LOWER[i] };
            if ctrl && c.is_ascii_alphabetic() {
                c = c.to_ascii_lowercase() & 0x1F;
            }
            if alt {
                send(&[0x1B, c]);
            } else {
                send(&[c]);
            }
        }
        0x4F => send(b"\x1b[C"),
        0x50 => send(b"\x1b[D"),
        0x51 => send(b"\x1b[B"),
        0x52 => send(b"\x1b[A"),
        0x4A => send(b"\x1b[H"),
        0x4D => send(b"\x1b[F"),
        0x4C => send(b"\x1b[3~"),
        0x4B => send(b"\x1b[5~"),
        0x4E => send(b"\x1b[6~"),
        0x58 => send(b"\n"),
        _ => {}
    }
}
