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
    nets: Vec<UsbNet>,
}

/// A USB network adapter or phone (CDC ECM or NCM): bulk endpoints for the frames.
struct UsbNet {
    slot: u8,
    kind: u8, // 1 ECM, 2 NCM, 3 RNDIS
    mac: [u8; 6],
    out_dci: u8,
    in_dci: u8,
    out_ring: Ring,
    in_ring: Ring,
    out_mps: u32,
    rx_bufs: u64,
    rx_next: usize,
    tx_bufs: u64,
    tx_next: usize,
    tx_busy: usize,
    rxq: alloc::collections::VecDeque<Vec<u8>>,
    seq: u16,
}

const NET_RX: usize = 16; // receive transfers kept queued, 16 KiB each
const NET_RX_SIZE: usize = 16384;
const NET_TX: usize = 32; // send buffers, 2 KiB each


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
        let mut hc = Xhci { op, db, rt, ctx64: hcc1 & 4 != 0, cmd, ev_base: ev, ev_idx: 0, ev_cycle: true, dcbaa, kbds: Vec::new(), nets: Vec::new() };
        sleep_ms(50);
        for port in 0..max_ports {
            hc.port(port);
        }
        if !hc.kbds.is_empty() {
            crate::kprintln!("[kernel] USB: {} keyboard(s)", hc.kbds.len());
        }
        for n in &hc.nets {
            crate::kprintln!("[kernel] USB: network adapter ({})", kind_name(n.kind));
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
                    self.transfer_event(&e);
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
            if self.try_network(&mut dev, p, speed, buf, input) {
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

    /// A CDC ECM or NCM network function in any of the device's configurations: set it up
    /// and keep it. False when the device is something else.
    fn try_network(&mut self, dev: &mut Dev0, p: usize, speed: u32, buf: u64, input: u64) -> bool {
        // SAFETY: our descriptor buffer, contexts and rings.
        unsafe {
            if self.control(dev, 0x80, 6, 0x0100, 0, buf, 18).is_none() {
                return false;
            }
            let configs = *v(buf).add(17);
            // every configuration's network function: (kind, config descriptor, comm interface,
            // data interface, alternate setting, IN, OUT, MAC string); ECM and NCM before RNDIS
            let mut found: Vec<(u8, Vec<u8>, u8, u8, u8, (u8, u32), (u8, u32), u8)> = Vec::new();
            for ci in 0..configs.min(4) {
                if self.control(dev, 0x80, 6, 0x0200 | ci as u16, 0, buf, 9).is_none() {
                    continue;
                }
                let total = (u16::from_le_bytes([*v(buf).add(2), *v(buf).add(3)]) as usize).min(4096);
                if self.control(dev, 0x80, 6, 0x0200 | ci as u16, 0, buf, total as u16).is_none() {
                    continue;
                }
                let cfg = core::slice::from_raw_parts(v(buf), total).to_vec();
                let (mut kind, mut comm, mut mac_str, mut union) = (0u8, None, 0u8, None);
                let mut cur: (u8, u8, u8, u8) = (0, 0, 0, 0); // number, alt, class, subclass
                let mut eps: Vec<(u8, u8, u8, u8, u32)> = Vec::new(); // interface, alt, class, address, size
                let mut off = 0;
                while off + 2 <= total {
                    let len = cfg[off] as usize;
                    if len < 2 || off + len > total {
                        break;
                    }
                    let d = &cfg[off..off + len];
                    match d[1] {
                        4 if len >= 9 => {
                            cur = (d[2], d[3], d[5], d[6]);
                            if comm.is_none() {
                                let k = match (d[5], d[6], d[7]) {
                                    (2, 6, _) => 1,
                                    (2, 0x0D, _) => 2,
                                    (0xE0, 1, 3) | (2, 2, 0xFF) | (0xEF, 4, 1) => 3,
                                    _ => 0,
                                };
                                if k != 0 {
                                    comm = Some(d[2]);
                                    kind = k;
                                }
                            }
                        }
                        0x24 if comm == Some(cur.0) && len >= 4 => match d[2] {
                            0x06 if len >= 5 => union = Some(d[4]),
                            0x0F => mac_str = d[3],
                            _ => {}
                        },
                        5 if len >= 7 && d[3] & 3 == 2 => eps.push((cur.0, cur.1, cur.2, d[2], u16::from_le_bytes([d[4], d[5]]) as u32 & 0x7FF)),
                        _ => {}
                    }
                    off += len;
                }
                let Some(comm) = comm else { continue };
                let data_if = union.unwrap_or(comm + 1);
                let want_alt = |alt: u8| if kind == 3 { alt == 0 } else { alt != 0 };
                let ep_in = eps.iter().find(|e| e.0 == data_if && e.2 == 0x0A && want_alt(e.1) && e.3 & 0x80 != 0);
                let ep_out = eps.iter().find(|e| e.0 == data_if && e.2 == 0x0A && want_alt(e.1) && e.3 & 0x80 == 0);
                if let (Some(i), Some(o)) = (ep_in, ep_out) {
                    found.push((kind, cfg.clone(), comm, data_if, i.1, (i.3 & 0xF, i.4), (o.3 & 0xF, o.4), mac_str));
                }
            }
            found.sort_by_key(|f| f.0 == 3);
            for (kind, cfg, comm, data_if, data_alt, (in_ep, in_mps), (out_ep, out_mps), mac_str) in found {
                let mut mac = [0x02, 0, 0, 0, 0, 1];
                if mac_str != 0 && self.control(dev, 0x80, 6, 0x0300 | mac_str as u16, 0x0409, buf, 255).is_some() {
                    let sd = core::slice::from_raw_parts(v(buf), 255);
                    let digits: Vec<u8> = sd[2..(sd[0] as usize).min(255)].chunks(2).map(|c| c[0]).collect();
                    if digits.len() >= 12 {
                        for (i, m) in mac.iter_mut().enumerate() {
                            let h = core::str::from_utf8(&digits[i * 2..i * 2 + 2]).ok().and_then(|t| u8::from_str_radix(t, 16).ok());
                            *m = h.unwrap_or(0);
                        }
                    }
                }
                let config_value = cfg[5];
                if self.control(dev, 0x00, 9, config_value as u16, 0, 0, 0).is_none() {
                    continue;
                }
                // Configure Endpoint: bulk OUT and IN
                let out_dci = out_ep * 2;
                let in_dci = in_ep * 2 + 1;
                core::ptr::write_bytes(v(input), 0, 4096);
                let icc = self.ctx(input, 0);
                write_volatile(icc.add(1), 1 | (1 << out_dci) | (1 << in_dci));
                let slotc = self.ctx(input, 1);
                write_volatile(slotc, (speed << 20) | ((out_dci.max(in_dci) as u32) << 27));
                write_volatile(slotc.add(1), ((p + 1) as u32) << 16);
                let out_ring = Ring::new();
                let in_ring = Ring::new();
                for (dci, ring, ty, mps) in [(out_dci, &out_ring, 2u32, out_mps), (in_dci, &in_ring, 6u32, in_mps)] {
                    let epc = self.ctx(input, 1 + dci as usize);
                    write_volatile(epc.add(1), (3 << 1) | (ty << 3) | (mps << 16));
                    write_volatile(epc.add(2), ring.base as u32 | 1);
                    write_volatile(epc.add(3), (ring.base >> 32) as u32);
                    write_volatile(epc.add(4), 1500);
                }
                let slot = dev.slot as u32;
                let Some((cc, _)) = self.command([input as u32, (input >> 32) as u32, 0, (12 << 10) | (slot << 24)]) else { return true };
                if cc != 1 {
                    crate::kprintln!("[kernel] USB: network adapter not configured (code {})", cc);
                    return true;
                }
                if kind == 3 {
                    // RNDIS: initialise, ask for the address, then switch the packet filter on
                    match self.rndis_setup(dev, comm, buf) {
                        Some(m) => mac = m,
                        None => {
                            crate::kprintln!("[kernel] USB: RNDIS device did not answer");
                            return true;
                        }
                    }
                } else {
                    // the data interface's working alternate setting; frames to us and broadcasts
                    let _ = self.control(dev, 0x01, 0x0B, data_alt as u16, data_if as u16, 0, 0);
                    let _ = self.control(dev, 0x21, 0x43, 0x0E, comm as u16, 0, 0);
                }
                let rx_bufs = mem::alloc_contiguous((NET_RX * NET_RX_SIZE / 4096) as u64);
                let tx_bufs = mem::alloc_contiguous((NET_TX * 2048 / 4096) as u64);
                let mut n = UsbNet {
                    slot: dev.slot,
                    kind,
                    mac,
                    out_dci,
                    in_dci,
                    out_ring,
                    in_ring,
                    out_mps,
                    rx_bufs,
                    rx_next: 0,
                    tx_bufs,
                    tx_next: 0,
                    tx_busy: 0,
                    rxq: alloc::collections::VecDeque::new(),
                    seq: 0,
                };
                for _ in 0..NET_RX {
                    self.queue_rx(&mut n);
                }
                self.nets.push(n);
                return true;
            }
            false
        }
    }

    /// An RNDIS control message and its reply (in `buf`).
    fn rndis_msg(&mut self, dev: &mut Dev0, comm: u8, buf: u64, msg: &[u32]) -> Option<Vec<u8>> {
        // SAFETY: our buffer page.
        unsafe { core::ptr::copy_nonoverlapping(msg.as_ptr() as *const u8, v(buf), msg.len() * 4) };
        self.control(dev, 0x21, 0x00, 0, comm as u16, buf, (msg.len() * 4) as u16)?;
        for _ in 0..50 {
            // SAFETY: as above.
            unsafe { core::ptr::write_bytes(v(buf), 0, 1024) };
            if self.control(dev, 0xA1, 0x01, 0, comm as u16, buf, 1024).is_some() {
                // SAFETY: as above.
                let r = unsafe { core::slice::from_raw_parts(v(buf), 1024) }.to_vec();
                let ty = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
                if ty == msg[0] | 0x8000_0000 {
                    return Some(r);
                }
            }
            sleep_ms(10);
        }
        None
    }

    fn rndis_setup(&mut self, dev: &mut Dev0, comm: u8, buf: u64) -> Option<[u8; 6]> {
        let le = |r: &[u8], at: usize| u32::from_le_bytes([r[at], r[at + 1], r[at + 2], r[at + 3]]);
        let r = self.rndis_msg(dev, comm, buf, &[2, 24, 1, 1, 0, 0x4000])?;
        if le(&r, 12) != 0 {
            return None;
        }
        // OID_802_3_PERMANENT_ADDRESS
        let r = self.rndis_msg(dev, comm, buf, &[4, 28, 2, 0x0101_0101, 0, 0, 0])?;
        let (blen, boff) = (le(&r, 16) as usize, le(&r, 20) as usize);
        let mut mac = [0u8; 6];
        if blen >= 6 && 8 + boff + 6 <= r.len() {
            mac.copy_from_slice(&r[8 + boff..8 + boff + 6]);
        }
        // OID_GEN_CURRENT_PACKET_FILTER: directed, multicast, broadcast
        self.rndis_msg(dev, comm, buf, &[5, 32, 3, 0x0001_010E, 4, 20, 0, 0x0B])?;
        Some(mac)
    }

    fn queue_rx(&self, n: &mut UsbNet) {
        let b = n.rx_bufs + (n.rx_next * NET_RX_SIZE) as u64;
        n.rx_next = (n.rx_next + 1) % NET_RX;
        n.in_ring.push([b as u32, (b >> 32) as u32, NET_RX_SIZE as u32, (1 << 10) | (1 << 5) | (1 << 2)]); // Normal, IOC, ISP
        // SAFETY: the device's doorbell.
        unsafe { write_volatile(self.db.add(n.slot as usize), n.in_dci as u32) };
    }

    /// A transfer finished: a keyboard report, a received frame, or a sent one.
    fn transfer_event(&mut self, e: &[u32; 4]) {
        let slot = (e[3] >> 24) as u8;
        let ep = ((e[3] >> 16) & 0x1F) as u8;
        if self.kbds.iter().any(|k| k.slot == slot && k.dci == ep) {
            self.keyboard_event(e);
            return;
        }
        let Some(i) = self.nets.iter().position(|n| n.slot == slot && (n.in_dci == ep || n.out_dci == ep)) else { return };
        let mut n = core::mem::replace(&mut self.nets[i], UsbNet::empty());
        if ep == n.out_dci {
            n.tx_busy = n.tx_busy.saturating_sub(1);
        } else {
            let cc = e[2] >> 24;
            let ptr = e[0] as u64 | (e[1] as u64) << 32;
            // the event points at the TRB; find which buffer it carried
            // SAFETY: a TRB of our own ring.
            let buf = unsafe { read_volatile(v(ptr) as *const u64) };
            let len = NET_RX_SIZE - (e[2] & 0xFF_FFFF) as usize;
            if (cc == 1 || cc == 13) && buf >= n.rx_bufs && buf < n.rx_bufs + (NET_RX * NET_RX_SIZE) as u64 && len > 0 {
                // SAFETY: the buffer the controller just filled.
                let data = unsafe { core::slice::from_raw_parts(v(buf), len.min(NET_RX_SIZE)) };
                if n.kind == 2 {
                    ncm_unpack(data, &mut n.rxq);
                } else if n.kind == 3 {
                    rndis_unpack(data, &mut n.rxq);
                } else if n.rxq.len() < 256 {
                    n.rxq.push_back(data.to_vec());
                }
            }
            // put the same buffer back in the queue
            n.in_ring.push([buf as u32, (buf >> 32) as u32, NET_RX_SIZE as u32, (1 << 10) | (1 << 5) | (1 << 2)]);
            // SAFETY: the device's doorbell.
            unsafe { write_volatile(self.db.add(n.slot as usize), n.in_dci as u32) };
        }
        self.nets[i] = n;
    }

    fn net_send(&mut self, i: usize, frame: &[u8]) {
        let n = &mut self.nets[i];
        if n.tx_busy >= NET_TX - 1 {
            return; // the adapter is behind: drop, TCP will send again
        }
        let b = n.tx_bufs + (n.tx_next * 2048) as u64;
        n.tx_next = (n.tx_next + 1) % NET_TX;
        let len = if n.kind == 2 {
            n.seq = n.seq.wrapping_add(1);
            ncm_pack(frame, n.seq, v(b))
        } else if n.kind == 3 {
            let l = frame.len().min(2048 - 44);
            let mut h = [0u32; 11];
            h[0] = 1;
            h[1] = (44 + l) as u32;
            h[2] = 36;
            h[3] = l as u32;
            // SAFETY: our send buffer.
            unsafe {
                core::ptr::copy_nonoverlapping(h.as_ptr() as *const u8, v(b), 44);
                core::ptr::copy_nonoverlapping(frame.as_ptr(), v(b).add(44), l);
            }
            44 + l
        } else {
            let l = frame.len().min(2048);
            // SAFETY: our send buffer.
            unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), v(b), l) };
            l
        };
        n.tx_busy += 1;
        let zlp = len % n.out_mps as usize == 0;
        n.out_ring.push([b as u32, (b >> 32) as u32, len as u32, (1 << 10) | if zlp { 0 } else { 1 << 5 }]);
        if zlp {
            // a transfer that fills whole packets ends with an empty one
            n.out_ring.push([0, 0, 0, (1 << 10) | (1 << 5)]);
        }
        // SAFETY: the device's doorbell.
        unsafe { write_volatile(self.db.add(n.slot as usize), n.out_dci as u32) };
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

impl UsbNet {
    fn empty() -> UsbNet {
        UsbNet {
            slot: 0,
            kind: 0,
            mac: [0; 6],
            out_dci: 0,
            in_dci: 0,
            out_ring: Ring { base: 0, idx: 0, cycle: true },
            in_ring: Ring { base: 0, idx: 0, cycle: true },
            out_mps: 512,
            rx_bufs: 0,
            rx_next: 0,
            tx_bufs: 0,
            tx_next: 0,
            tx_busy: 0,
            rxq: alloc::collections::VecDeque::new(),
            seq: 0,
        }
    }
}

fn kind_name(k: u8) -> &'static str {
    match k {
        2 => "CDC NCM",
        3 => "RNDIS",
        _ => "CDC ECM",
    }
}

/// The Ethernet frames in an RNDIS transfer (one or more data messages).
fn rndis_unpack(d: &[u8], q: &mut alloc::collections::VecDeque<Vec<u8>>) {
    let le = |at: usize| -> usize { d.get(at..at + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]) as usize).unwrap_or(0) };
    let mut at = 0;
    while at + 44 <= d.len() {
        let (ty, len, off, dlen) = (le(at), le(at + 4), le(at + 8), le(at + 12));
        if ty != 1 || len < 44 {
            return;
        }
        let start = at + 8 + off;
        if start + dlen <= d.len() && q.len() < 256 {
            q.push_back(d[start..start + dlen].to_vec());
        }
        at += len;
    }
}

/// One Ethernet frame in an NCM transfer block (16-bit format): NTH16, the frame, NDP16.
fn ncm_pack(frame: &[u8], seq: u16, out: *mut u8) -> usize {
    let flen = frame.len().min(1514);
    let data_at = 12usize;
    let ndp_at = (data_at + flen + 3) & !3;
    let total = ndp_at + 16;
    let mut b = [0u8; 2048];
    b[0..4].copy_from_slice(b"NCMH");
    b[4..6].copy_from_slice(&12u16.to_le_bytes());
    b[6..8].copy_from_slice(&seq.to_le_bytes());
    b[8..10].copy_from_slice(&(total as u16).to_le_bytes());
    b[10..12].copy_from_slice(&(ndp_at as u16).to_le_bytes());
    b[data_at..data_at + flen].copy_from_slice(&frame[..flen]);
    let n = &mut b[ndp_at..ndp_at + 16];
    n[0..4].copy_from_slice(b"NCM0");
    n[4..6].copy_from_slice(&16u16.to_le_bytes());
    n[8..10].copy_from_slice(&(data_at as u16).to_le_bytes());
    n[10..12].copy_from_slice(&(flen as u16).to_le_bytes());
    // SAFETY: our 2 KiB send buffer.
    unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), out, total) };
    total
}

/// The frames in a received NCM transfer block.
fn ncm_unpack(d: &[u8], q: &mut alloc::collections::VecDeque<Vec<u8>>) {
    let le = |at: usize| -> usize { d.get(at..at + 2).map(|x| u16::from_le_bytes([x[0], x[1]]) as usize).unwrap_or(0) };
    if d.len() < 12 || &d[0..4] != b"NCMH" {
        return;
    }
    let mut ndp = le(10);
    let mut guard = 0;
    while ndp != 0 && ndp + 8 <= d.len() && guard < 16 {
        guard += 1;
        if &d[ndp..ndp + 3] != b"NCM" {
            return;
        }
        let len = le(ndp + 4);
        let mut e = ndp + 8;
        while e + 4 <= (ndp + len).min(d.len()) {
            let (at, l) = (le(e), le(e + 2));
            if at == 0 || l == 0 {
                break;
            }
            if at + l <= d.len() && q.len() < 256 {
                q.push_back(d[at..at + l].to_vec());
            }
            e += 4;
        }
        ndp = le(ndp + 6);
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
                hc.transfer_event(&e);
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

// ---------------------------------------------------------------------------------------
// The network adapters as network cards for the stack

pub struct UsbNic {
    hc: usize,
    n: usize,
    mac: [u8; 6],
}

fn hcs() -> &'static mut Vec<Xhci> {
    // SAFETY: one CPU.
    unsafe { &mut *core::ptr::addr_of_mut!(HCS) }
}

/// The first USB network adapter found, if any.
pub fn network_card() -> Option<alloc::boxed::Box<dyn crate::nic::Nic>> {
    for (h, hc) in hcs().iter().enumerate() {
        if let Some(n) = hc.nets.first() {
            // the address the device gives (iMACAddress) is the one this end uses, as on Linux
            return Some(alloc::boxed::Box::new(UsbNic { hc: h, n: 0, mac: n.mac }));
        }
    }
    None
}

impl crate::nic::Nic for UsbNic {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn name(&self) -> &'static str {
        match hcs()[self.hc].nets[self.n].kind {
            2 => "USB network (CDC NCM)",
            3 => "USB network (RNDIS)",
            _ => "USB network (CDC ECM)",
        }
    }
    fn link_up(&self) -> bool {
        true
    }
    fn send(&mut self, frame: &[u8]) {
        let hc = &mut hcs()[self.hc];
        while let Some(e) = hc.event() {
            if (e[3] >> 10) & 0x3F == 32 {
                hc.transfer_event(&e);
            }
        }
        hc.net_send(self.n, frame);
    }
    fn recv(&mut self) -> Option<Vec<u8>> {
        let hc = &mut hcs()[self.hc];
        if hc.nets[self.n].rxq.is_empty() {
            while let Some(e) = hc.event() {
                if (e[3] >> 10) & 0x3F == 32 {
                    hc.transfer_event(&e);
                }
            }
        }
        hc.nets[self.n].rxq.pop_front()
    }
}
