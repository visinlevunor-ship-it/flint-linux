//! The network stack: Ethernet, ARP, IPv4, ICMP, UDP and TCP, a DHCP client, and the
//! loopback interface. Driven by polling (the timer and the idle loop) and by the socket calls
//! themselves; there is one wired card at most.
//!
//! All state lives in one `Net`, reached through `with`, which marks it busy so a timer tick
//! that arrives in the middle of a socket call leaves it alone.

use crate::nic::Nic;
use crate::syscall::now_ns;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

pub const ECONNREFUSED: i64 = 111;
pub const ECONNRESET: i64 = 104;
pub const ETIMEDOUT: i64 = 110;
pub const ENETUNREACH: i64 = 101;
pub const EHOSTUNREACH: i64 = 113;

const MS: u64 = 1_000_000;
const SEC: u64 = 1_000_000_000;
const LOOPBACK: u32 = 0x7F00_0001;
const MSS: usize = 1460;
const RBUF: usize = 65535;
const SBUF: usize = 256 * 1024;
const DGRAM_QUEUE: usize = 128;

pub fn ip_text(ip: u32) -> alloc::string::String {
    alloc::format!("{}.{}.{}.{}", ip >> 24, (ip >> 16) & 255, (ip >> 8) & 255, ip & 255)
}

fn is_loop(ip: u32) -> bool {
    ip >> 24 == 127
}

pub fn checksum_add(mut sum: u32, data: &[u8]) -> u32 {
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        sum += u16::from_be_bytes([c[0], c[1]]) as u32;
    }
    if let [b] = chunks.remainder() {
        sum += (*b as u32) << 8;
    }
    sum
}

pub fn checksum_end(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn checksum(data: &[u8]) -> u16 {
    checksum_end(checksum_add(0, data))
}

fn pseudo(src: u32, dst: u32, proto: u8, len: usize) -> u32 {
    let mut s = checksum_add(0, &src.to_be_bytes());
    s = checksum_add(s, &dst.to_be_bytes());
    s + proto as u32 + len as u32
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

// ---------------------------------------------------------------------------------------
// Sockets

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum St {
    Closed,
    Listen,
    SynSent,
    SynRcvd,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

pub struct Tcp {
    pub st: St,
    iss: u32,
    snd_una: u32,
    snd_nxt: u32,
    snd_wnd: u32,
    rcv_nxt: u32,
    sbuf: VecDeque<u8>, // from snd_una: sent but unacknowledged, then unsent
    pub rbuf: VecDeque<u8>,
    fin_wanted: bool,
    fin_sent: bool,
    pub rcv_fin: bool,
    rto: u64,
    rtx_at: u64,
    retries: u32,
    mss: usize,
    pub backlog: VecDeque<usize>,
    max_backlog: usize,
    parent: Option<usize>,
    linger_until: u64,
    last_wnd: u32,
    dupacks: u32,
}

impl Tcp {
    pub fn set_backlog(&mut self, n: usize) {
        self.max_backlog = n;
    }
    pub fn parent_is(&self, l: usize) -> bool {
        self.parent == Some(l)
    }
    pub fn fin_wanted(&self) -> bool {
        self.fin_wanted
    }
    fn new() -> Tcp {
        Tcp {
            st: St::Closed,
            iss: 0,
            snd_una: 0,
            snd_nxt: 0,
            snd_wnd: 0,
            rcv_nxt: 0,
            sbuf: VecDeque::new(),
            rbuf: VecDeque::new(),
            fin_wanted: false,
            fin_sent: false,
            rcv_fin: false,
            rto: SEC,
            rtx_at: 0,
            retries: 0,
            mss: 536,
            backlog: VecDeque::new(),
            max_backlog: 0,
            parent: None,
            linger_until: 0,
            last_wnd: 0,
            dupacks: 0,
        }
    }
    fn window(&self) -> u32 {
        (RBUF - self.rbuf.len()) as u32
    }
    /// Bytes sent and not yet acknowledged (data only).
    fn in_flight(&self) -> usize {
        let mut n = self.snd_nxt.wrapping_sub(self.snd_una) as usize;
        if self.fin_sent && n > 0 {
            n -= 1;
        }
        if matches!(self.st, St::SynSent | St::SynRcvd) {
            n = n.saturating_sub(1);
        }
        n.min(self.sbuf.len())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Udp,
    Icmp,
    Raw(u8),
    Tcp,
}

pub struct Sock {
    pub proto: Proto,
    pub lip: u32,
    pub lport: u16,
    pub rip: u32,
    pub rport: u16,
    pub connected: bool,
    pub dgrams: VecDeque<(u32, u16, Vec<u8>)>,
    pub tcp: Tcp,
    pub err: i64,
    pub orphan: bool,
    pub shut_rd: bool,
    pub reuse: bool,
    pub rcvtimeo: u64,
    pub sndtimeo: u64,
}

impl Sock {
    fn new(proto: Proto) -> Sock {
        Sock {
            proto,
            lip: 0,
            lport: 0,
            rip: 0,
            rport: 0,
            connected: false,
            dgrams: VecDeque::new(),
            tcp: Tcp::new(),
            err: 0,
            orphan: false,
            shut_rd: false,
            reuse: false,
            rcvtimeo: 0,
            sndtimeo: 0,
        }
    }
}

// ---------------------------------------------------------------------------------------
// The interface and its state

#[derive(PartialEq, Eq, Clone, Copy)]
enum Dhcp {
    Off,
    Discover,
    Request,
    Bound,
}

pub struct Net {
    nic: Option<Box<dyn Nic>>,
    mac: [u8; 6],
    pub ip: u32,
    pub mask: u32,
    pub gw: u32,
    pub dns: Vec<u32>,
    arp: BTreeMap<u32, [u8; 6]>,
    arp_wait: Vec<(u32, Vec<u8>, u64)>,
    arp_asked: BTreeMap<u32, u64>,
    loopq: VecDeque<Vec<u8>>,
    pub socks: Vec<Option<Sock>>,
    next_port: u16,
    ip_id: u16,
    dhcp: Dhcp,
    dhcp_xid: u32,
    dhcp_at: u64,
    dhcp_server: u32,
    dhcp_offer: u32,
    dhcp_tries: u32,
    lease_renew: u64,
    resolv_pending: bool,
    link_was: bool,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

static mut NET: Option<Net> = None;
static mut BUSY: bool = false;

/// Run `f` on the network state (always present after `init`).
pub fn with<R>(f: impl FnOnce(&mut Net) -> R) -> R {
    // SAFETY: one CPU; BUSY keeps the timer's poll out while we are in here.
    unsafe {
        let was = BUSY;
        BUSY = true;
        let n = (*core::ptr::addr_of_mut!(NET)).as_mut().expect("network not set up");
        let r = f(n);
        BUSY = was;
        r
    }
}

/// Set up the stack with the card found (if any); `cmdline` may say `ip=ADDR/BITS,GATEWAY,DNS`
/// or `ip=off`, otherwise DHCP configures it.
pub fn init(nic: Option<Box<dyn Nic>>, cmdline: &str) {
    let mac = nic.as_ref().map(|n| n.mac()).unwrap_or([0; 6]);
    let mut n = Net {
        nic,
        mac,
        ip: 0,
        mask: 0,
        gw: 0,
        dns: Vec::new(),
        arp: BTreeMap::new(),
        arp_wait: Vec::new(),
        arp_asked: BTreeMap::new(),
        loopq: VecDeque::new(),
        socks: Vec::new(),
        next_port: 49152 + (crate::syscall::random_u64() % 8000) as u16,
        ip_id: crate::syscall::random_u64() as u16,
        dhcp: Dhcp::Off,
        dhcp_xid: crate::syscall::random_u64() as u32,
        dhcp_at: 0,
        dhcp_server: 0,
        dhcp_offer: 0,
        dhcp_tries: 0,
        lease_renew: u64::MAX,
        resolv_pending: false,
        link_was: false,
        rx_packets: 0,
        tx_packets: 0,
    };
    let arg = cmdline.split_whitespace().find_map(|w| w.strip_prefix("ip="));
    if let Some(card) = &n.nic {
        let m = n.mac;
        crate::kprintln!("[kernel] network: {} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", card.name(), m[0], m[1], m[2], m[3], m[4], m[5]);
        match arg {
            Some("off") => {}
            Some(spec) if spec != "dhcp" => {
                let parts: Vec<&str> = spec.split(',').collect();
                let (addr, bits) = parts[0].split_once('/').unwrap_or((parts[0], "24"));
                n.ip = parse_ip(addr).unwrap_or(0);
                let bits: u32 = bits.parse().unwrap_or(24).min(32);
                n.mask = if bits == 0 { 0 } else { !0u32 << (32 - bits) };
                n.gw = parts.get(1).and_then(|s| parse_ip(s)).unwrap_or(0);
                n.dns = parts.get(2).and_then(|s| parse_ip(s)).into_iter().collect();
                n.resolv_pending = !n.dns.is_empty();
                crate::kprintln!("[kernel] network: address {} (set on the command line)", ip_text(n.ip));
            }
            _ => n.dhcp = Dhcp::Discover,
        }
    }
    // SAFETY: before any task runs.
    unsafe { *core::ptr::addr_of_mut!(NET) = Some(n) };
}

pub fn parse_ip(s: &str) -> Option<u32> {
    let mut out = 0u32;
    let mut count = 0;
    for p in s.split('.') {
        out = (out << 8) | p.parse::<u8>().ok()? as u32;
        count += 1;
    }
    (count == 4).then_some(out)
}

/// Process what arrived and what timed out. From the timer, the idle loop and socket calls.
pub fn poll() {
    // SAFETY: as `with`.
    unsafe {
        if BUSY || (*core::ptr::addr_of!(NET)).is_none() {
            return;
        }
    }
    let woke = with(|n| n.poll());
    if woke {
        crate::proc::wake_all();
    }
}

/// Work that touches files: from the idle loop only, where no system call is half done.
pub fn idle() {
    poll();
    let pending = with(|n| core::mem::replace(&mut n.resolv_pending, false).then(|| n.dns.clone()));
    if let Some(dns) = pending {
        let mut text = alloc::string::String::from("# written by the Flint kernel from DHCP\n");
        for d in dns {
            text.push_str(&alloc::format!("nameserver {}\n", ip_text(d)));
        }
        put_file("run/resolv.conf", text);
    }
    // the card's state for `net` (the same key=value form netd writes on Linux)
    let status = with(|n| {
        if !n.has_card() {
            return None;
        }
        let m = n.mac;
        let state = if !n.link_up() {
            "no-carrier"
        } else if n.ip != 0 {
            "up"
        } else if n.dhcp == Dhcp::Discover || n.dhcp == Dhcp::Request {
            if n.dhcp_tries > 3 { "no-dhcp" } else { "configuring" }
        } else {
            "down"
        };
        let mut t = alloc::format!("state={state}\ndriver={}\nmac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n", n.card_name(), m[0], m[1], m[2], m[3], m[4], m[5]);
        if n.ip != 0 {
            t.push_str(&alloc::format!("address={}/{}\n", ip_text(n.ip), n.mask.count_ones()));
        }
        if n.gw != 0 {
            t.push_str(&alloc::format!("gateway={}\n", ip_text(n.gw)));
        }
        let dns: Vec<_> = n.dns.iter().map(|d| ip_text(*d)).collect();
        t.push_str(&alloc::format!("dns={}\n", dns.join(" ")));
        Some(t)
    });
    if let Some(t) = status {
        // SAFETY: only the idle loop touches this.
        let last = unsafe { &mut *core::ptr::addr_of_mut!(LAST_STATUS) };
        if last.as_deref() != Some(t.as_str()) {
            put_file("run/net/status/eth0", t.clone());
            *last = Some(t);
        }
    }
}

static mut LAST_STATUS: Option<alloc::string::String> = None;

/// Write a file of the RAM layer, making its directories.
fn put_file(path: &str, text: alloc::string::String) {
    let mut dir = alloc::string::String::new();
    let parts: Vec<&str> = path.split('/').collect();
    for p in &parts[..parts.len() - 1] {
        if !dir.is_empty() {
            dir.push('/');
        }
        dir.push_str(p);
        if crate::vfs::lookup(&dir).is_none() {
            let _ = crate::vfs::mkdir(&dir, 0o755);
        }
    }
    if let Ok(m) = crate::vfs::writable(path, true, 0o644) {
        let mut m = m.borrow_mut();
        if m.data != text.as_bytes() {
            m.data = text.into_bytes();
            m.mtime = crate::vfs::now();
            drop(m);
            crate::vfs::mark_dirty();
        }
    }
}

impl Net {
    fn poll(&mut self) -> bool {
        let mut woke = false;
        let mut budget = 256;
        while budget > 0 {
            budget -= 1;
            if let Some(p) = self.loopq.pop_front() {
                self.ip_in(&p);
                woke = true;
                continue;
            }
            let Some(frame) = self.nic.as_mut().and_then(|n| n.recv()) else { break };
            self.rx_packets += 1;
            self.eth_in(&frame);
            woke = true;
        }
        if self.timers() {
            woke = true;
        }
        woke
    }

    // -------------------------------------------------------------------------------
    // Ethernet and ARP

    fn eth_send(&mut self, dst: [u8; 6], ty: u16, payload: &[u8]) {
        let Some(nic) = self.nic.as_mut() else { return };
        let mut f = Vec::with_capacity(14 + payload.len().max(46));
        f.extend_from_slice(&dst);
        f.extend_from_slice(&self.mac);
        f.extend_from_slice(&ty.to_be_bytes());
        f.extend_from_slice(payload);
        while f.len() < 60 {
            f.push(0);
        }
        nic.send(&f);
        self.tx_packets += 1;
    }

    fn arp_send(&mut self, op: u16, tmac: [u8; 6], tip: u32) {
        let mut p = Vec::with_capacity(28);
        p.extend_from_slice(&[0, 1, 8, 0, 6, 4]);
        p.extend_from_slice(&op.to_be_bytes());
        p.extend_from_slice(&self.mac);
        p.extend_from_slice(&self.ip.to_be_bytes());
        p.extend_from_slice(&tmac);
        p.extend_from_slice(&tip.to_be_bytes());
        let dst = if op == 1 { [0xFF; 6] } else { tmac };
        self.eth_send(dst, 0x0806, &p);
    }

    fn eth_in(&mut self, f: &[u8]) {
        if f.len() < 14 {
            return;
        }
        let ty = be16(f, 12);
        let p = &f[14..];
        match ty {
            0x0806 if p.len() >= 28 => {
                let op = be16(p, 6);
                let smac: [u8; 6] = p[8..14].try_into().unwrap();
                let sip = be32(p, 14);
                let tip = be32(p, 24);
                if self.ip != 0 && tip == self.ip {
                    self.arp.insert(sip, smac);
                    if op == 1 {
                        self.arp_send(2, smac, sip);
                    }
                } else if self.arp.contains_key(&sip) {
                    self.arp.insert(sip, smac);
                }
                self.flush_arp(sip);
            }
            0x0800 => self.ip_in(p),
            _ => {}
        }
    }

    fn flush_arp(&mut self, ip: u32) {
        let Some(&mac) = self.arp.get(&ip) else { return };
        let mut i = 0;
        while i < self.arp_wait.len() {
            if self.arp_wait[i].0 == ip {
                let (_, pkt, _) = self.arp_wait.remove(i);
                self.eth_send(mac, 0x0800, &pkt);
            } else {
                i += 1;
            }
        }
    }

    // -------------------------------------------------------------------------------
    // IPv4

    /// The source address for packets to `dst`, or None when there is no way there.
    pub fn src_for(&self, dst: u32) -> Option<u32> {
        if is_loop(dst) {
            return Some(LOOPBACK);
        }
        if dst == 0xFFFF_FFFF && self.nic.is_some() {
            return Some(self.ip);
        }
        if self.ip == 0 {
            return None;
        }
        if dst == self.ip || dst & self.mask == self.ip & self.mask || self.gw != 0 {
            return Some(self.ip);
        }
        None
    }

    fn ip_send(&mut self, proto: u8, src: u32, dst: u32, payload: &[u8]) {
        let mut p = Vec::with_capacity(20 + payload.len());
        let len = 20 + payload.len();
        self.ip_id = self.ip_id.wrapping_add(1);
        p.extend_from_slice(&[0x45, 0]);
        p.extend_from_slice(&(len as u16).to_be_bytes());
        p.extend_from_slice(&self.ip_id.to_be_bytes());
        p.extend_from_slice(&[0x40, 0, 64, proto, 0, 0]); // don't fragment, TTL 64
        p.extend_from_slice(&src.to_be_bytes());
        p.extend_from_slice(&dst.to_be_bytes());
        let c = checksum(&p[..20]);
        p[10..12].copy_from_slice(&c.to_be_bytes());
        p.extend_from_slice(payload);
        if is_loop(dst) || (dst == self.ip && self.ip != 0) {
            self.loopq.push_back(p);
            return;
        }
        if self.nic.is_none() {
            return;
        }
        if dst == 0xFFFF_FFFF || (self.mask != 0 && dst == self.ip | !self.mask) {
            self.eth_send([0xFF; 6], 0x0800, &p);
            return;
        }
        let hop = if dst & self.mask == self.ip & self.mask || self.gw == 0 { dst } else { self.gw };
        if let Some(&mac) = self.arp.get(&hop) {
            self.eth_send(mac, 0x0800, &p);
            return;
        }
        let now = now_ns();
        if self.arp_wait.len() < 256 {
            self.arp_wait.push((hop, p, now + 3 * SEC));
        }
        if self.arp_asked.get(&hop).is_none_or(|&t| now - t > 500 * MS) {
            self.arp_asked.insert(hop, now);
            self.arp_send(1, [0; 6], hop);
        }
    }

    fn ip_in(&mut self, p: &[u8]) {
        if p.len() < 20 || p[0] >> 4 != 4 {
            return;
        }
        let ihl = (p[0] & 15) as usize * 4;
        let total = be16(p, 2) as usize;
        if ihl < 20 || total < ihl || total > p.len() {
            return;
        }
        if be16(p, 6) & 0x3FFF != 0 {
            return; // fragments are not reassembled
        }
        let proto = p[9];
        let src = be32(p, 12);
        let dst = be32(p, 16);
        let ours = dst == self.ip || is_loop(dst) || dst == 0xFFFF_FFFF || (self.mask != 0 && dst == self.ip | !self.mask) || self.ip == 0;
        if !ours {
            return;
        }
        let body = &p[ihl..total];
        // raw sockets see everything of their protocol
        for s in self.socks.iter_mut().flatten() {
            if s.proto == Proto::Raw(proto) && s.dgrams.len() < DGRAM_QUEUE && (!s.connected || s.rip == src) {
                s.dgrams.push_back((src, 0, p[..total].to_vec()));
            }
        }
        match proto {
            1 => self.icmp_in(src, dst, body),
            6 => self.tcp_in(src, dst, body),
            17 => self.udp_in(src, dst, body, p),
            _ => {}
        }
    }

    // -------------------------------------------------------------------------------
    // ICMP

    fn icmp_in(&mut self, src: u32, dst: u32, b: &[u8]) {
        if b.len() < 8 || checksum(b) != 0 {
            return;
        }
        match b[0] {
            8 => {
                // echo request: answer it
                let mut r = b.to_vec();
                r[0] = 0;
                r[2] = 0;
                r[3] = 0;
                let c = checksum(&r);
                r[2..4].copy_from_slice(&c.to_be_bytes());
                let from = if dst == 0xFFFF_FFFF { self.ip } else { dst };
                self.ip_send(1, from, src, &r);
            }
            0 => {
                let id = be16(b, 4);
                for s in self.socks.iter_mut().flatten() {
                    if s.proto == Proto::Icmp && s.lport == id && s.dgrams.len() < DGRAM_QUEUE {
                        s.dgrams.push_back((src, 0, b.to_vec()));
                    }
                }
            }
            3 if b.len() >= 8 + 28 => {
                // unreachable: tell the UDP socket that sent it
                let inner = &b[8..];
                let ihl = (inner[0] & 15) as usize * 4;
                if inner[9] == 17 && inner.len() >= ihl + 4 {
                    let sport = be16(inner, ihl);
                    let dport = be16(inner, ihl + 2);
                    let idst = be32(inner, 16);
                    for s in self.socks.iter_mut().flatten() {
                        if s.proto == Proto::Udp && s.connected && s.lport == sport && s.rport == dport && s.rip == idst {
                            s.err = ECONNREFUSED;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn unreachable(&mut self, src: u32, dst: u32, ip_packet: &[u8]) {
        if dst == 0xFFFF_FFFF || src == 0 {
            return;
        }
        let mut r = alloc::vec![3u8, 3, 0, 0, 0, 0, 0, 0];
        r.extend_from_slice(&ip_packet[..ip_packet.len().min(28)]);
        let c = checksum(&r);
        r[2..4].copy_from_slice(&c.to_be_bytes());
        self.ip_send(1, dst, src, &r);
    }

    // -------------------------------------------------------------------------------
    // UDP

    pub fn udp_send(&mut self, src: u32, sport: u16, dst: u32, dport: u16, data: &[u8]) {
        let len = 8 + data.len();
        let mut u = Vec::with_capacity(len);
        u.extend_from_slice(&sport.to_be_bytes());
        u.extend_from_slice(&dport.to_be_bytes());
        u.extend_from_slice(&(len as u16).to_be_bytes());
        u.extend_from_slice(&[0, 0]);
        u.extend_from_slice(data);
        let mut c = checksum_end(checksum_add(pseudo(src, dst, 17, len), &u));
        if c == 0 {
            c = 0xFFFF;
        }
        u[6..8].copy_from_slice(&c.to_be_bytes());
        self.ip_send(17, src, dst, &u);
    }

    fn udp_in(&mut self, src: u32, dst: u32, b: &[u8], whole: &[u8]) {
        if b.len() < 8 {
            return;
        }
        let sport = be16(b, 0);
        let dport = be16(b, 2);
        let len = (be16(b, 4) as usize).clamp(8, b.len());
        let data = &b[8..len];
        if dport == 68 && self.dhcp != Dhcp::Off {
            self.dhcp_in(data);
            return;
        }
        let mut best: Option<usize> = None;
        for (i, s) in self.socks.iter().enumerate() {
            let Some(s) = s else { continue };
            if s.proto != Proto::Udp || s.lport != dport {
                continue;
            }
            if s.lip != 0 && s.lip != dst && dst != 0xFFFF_FFFF {
                continue;
            }
            if s.connected && (s.rip != src || s.rport != sport) {
                continue;
            }
            if best.is_none() || s.connected {
                best = Some(i);
            }
        }
        match best {
            Some(i) => {
                let s = self.socks[i].as_mut().unwrap();
                if s.dgrams.len() < DGRAM_QUEUE && !s.shut_rd {
                    s.dgrams.push_back((src, sport, data.to_vec()));
                }
            }
            None => self.unreachable(src, dst, whole),
        }
    }

    // -------------------------------------------------------------------------------
    // DHCP

    fn dhcp_send(&mut self, ty: u8) {
        let mut m = alloc::vec![0u8; 240];
        m[0] = 1;
        m[1] = 1;
        m[2] = 6;
        m[4..8].copy_from_slice(&self.dhcp_xid.to_be_bytes());
        m[10] = 0x80; // answer by broadcast
        m[28..34].copy_from_slice(&self.mac);
        m[236..240].copy_from_slice(&[99, 130, 83, 99]);
        m.extend_from_slice(&[53, 1, ty]);
        m.extend_from_slice(&[61, 7, 1]);
        m.extend_from_slice(&self.mac);
        if ty == 3 {
            m.extend_from_slice(&[50, 4]);
            m.extend_from_slice(&self.dhcp_offer.to_be_bytes());
            m.extend_from_slice(&[54, 4]);
            m.extend_from_slice(&self.dhcp_server.to_be_bytes());
        }
        m.extend_from_slice(b"\x0c\x05flint");
        m.extend_from_slice(&[55, 4, 1, 3, 6, 15, 255]);
        let src = self.ip;
        self.udp_send(src, 68, 0xFFFF_FFFF, 67, &m);
    }

    fn dhcp_in(&mut self, d: &[u8]) {
        if d.len() < 240 || d[0] != 2 || be32(d, 4) != self.dhcp_xid || d[236..240] != [99, 130, 83, 99] {
            return;
        }
        let yiaddr = be32(d, 16);
        let (mut ty, mut mask, mut router, mut dns, mut server, mut lease) = (0u8, 0u32, 0u32, Vec::new(), 0u32, 3600u32);
        let mut i = 240;
        while i < d.len() && d[i] != 255 {
            if d[i] == 0 {
                i += 1;
                continue;
            }
            if i + 1 >= d.len() {
                break;
            }
            let (code, len) = (d[i], d[i + 1] as usize);
            let Some(v) = d.get(i + 2..i + 2 + len) else { break };
            match code {
                53 if len >= 1 => ty = v[0],
                1 if len >= 4 => mask = be32(v, 0),
                3 if len >= 4 => router = be32(v, 0),
                6 => dns = v.chunks_exact(4).map(|c| be32(c, 0)).collect(),
                54 if len >= 4 => server = be32(v, 0),
                51 if len >= 4 => lease = be32(v, 0),
                _ => {}
            }
            i += 2 + len;
        }
        match (ty, self.dhcp) {
            (2, Dhcp::Discover) => {
                self.dhcp_offer = yiaddr;
                self.dhcp_server = server;
                self.dhcp = Dhcp::Request;
                self.dhcp_tries = 0;
                self.dhcp_send(3);
                self.dhcp_at = now_ns() + 2 * SEC;
            }
            (5, Dhcp::Request | Dhcp::Bound) => {
                let first = self.ip != yiaddr;
                self.ip = yiaddr;
                self.mask = if mask != 0 { mask } else { 0xFFFF_FF00 };
                self.gw = router;
                if !dns.is_empty() && dns != self.dns {
                    self.dns = dns;
                    self.resolv_pending = true;
                }
                self.dhcp = Dhcp::Bound;
                self.lease_renew = now_ns() + (lease.max(60) as u64 / 2) * SEC;
                if first {
                    let bits = self.mask.count_ones();
                    crate::kprintln!("[kernel] network: address {}/{} from DHCP, gateway {}", ip_text(self.ip), bits, ip_text(self.gw));
                }
            }
            (6, _) => {
                self.ip = 0;
                self.dhcp = Dhcp::Discover;
                self.dhcp_at = 0;
            }
            _ => {}
        }
    }

    fn dhcp_timer(&mut self, now: u64) {
        let Some(nic) = &self.nic else { return };
        let up = nic.link_up();
        if up && !self.link_was && self.dhcp == Dhcp::Bound {
            // cable plugged in again: ask whether the address still holds
            self.dhcp = Dhcp::Request;
            self.dhcp_offer = self.ip;
            self.dhcp_at = 0;
        }
        self.link_was = up;
        if !up {
            return;
        }
        match self.dhcp {
            Dhcp::Discover if now >= self.dhcp_at => {
                self.dhcp_xid = self.dhcp_xid.wrapping_add(1);
                self.dhcp_send(1);
                self.dhcp_tries += 1;
                self.dhcp_at = now + (2 * SEC << self.dhcp_tries.min(4));
            }
            Dhcp::Request if now >= self.dhcp_at => {
                self.dhcp_tries += 1;
                if self.dhcp_tries > 4 {
                    self.dhcp = Dhcp::Discover;
                    self.dhcp_tries = 0;
                    self.dhcp_at = 0;
                } else {
                    self.dhcp_send(3);
                    self.dhcp_at = now + 2 * SEC;
                }
            }
            Dhcp::Bound if now >= self.lease_renew => {
                self.dhcp = Dhcp::Request;
                self.dhcp_offer = self.ip;
                self.dhcp_tries = 0;
                self.dhcp_at = 0;
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------------------------
    // Timers

    fn timers(&mut self) -> bool {
        let now = now_ns();
        self.dhcp_timer(now);
        self.arp_wait.retain(|(_, _, until)| *until > now);
        let mut woke = false;
        for i in 0..self.socks.len() {
            let Some(s) = &self.socks[i] else { continue };
            if s.proto != Proto::Tcp {
                continue;
            }
            let t = &s.tcp;
            if s.orphan && t.st == St::FinWait2 && t.linger_until == 0 {
                self.socks[i].as_mut().unwrap().tcp.linger_until = now + 30 * SEC;
                continue;
            }
            if s.orphan && t.st == St::FinWait2 && now >= t.linger_until {
                self.socks[i] = None;
                continue;
            }
            if t.st == St::TimeWait && now >= t.linger_until {
                self.socks[i].as_mut().unwrap().tcp.st = St::Closed;
                self.reap(i);
                woke = true;
                continue;
            }
            if t.rtx_at != 0 && now >= t.rtx_at {
                self.retransmit(i, now);
                woke = true;
            }
        }
        woke
    }

    fn reap(&mut self, i: usize) {
        if let Some(s) = &self.socks[i] {
            if s.orphan && matches!(s.tcp.st, St::Closed) || s.orphan && s.proto != Proto::Tcp {
                self.socks[i] = None;
            }
        }
    }

    // -------------------------------------------------------------------------------
    // TCP

    pub fn port_free(&self, proto: Proto, port: u16) -> bool {
        !self.socks.iter().flatten().any(|s| s.proto == proto && s.lport == port && !(s.proto == Proto::Tcp && s.tcp.st == St::TimeWait))
    }

    pub fn ephemeral(&mut self, proto: Proto) -> u16 {
        for _ in 0..16384 {
            self.next_port = if self.next_port >= 65535 { 49152 } else { self.next_port + 1 };
            if self.port_free(proto, self.next_port) {
                return self.next_port;
            }
        }
        0
    }

    pub fn alloc(&mut self, proto: Proto) -> usize {
        let s = Sock::new(proto);
        if let Some(i) = self.socks.iter().position(|s| s.is_none()) {
            self.socks[i] = Some(s);
            i
        } else {
            self.socks.push(Some(s));
            self.socks.len() - 1
        }
    }

    pub fn sock(&mut self, i: usize) -> &mut Sock {
        self.socks[i].as_mut().expect("socket gone")
    }

    fn tcp_segment(&mut self, i: usize, seq: u32, flags: u8, data: &[u8]) {
        let s = self.socks[i].as_mut().unwrap();
        let (src, dst, sport, dport) = (s.lip, s.rip, s.lport, s.rport);
        let ack = s.tcp.rcv_nxt;
        let wnd = s.tcp.window();
        s.tcp.last_wnd = wnd;
        let syn = flags & 2 != 0;
        let hlen = if syn { 24 } else { 20 };
        let mut t = Vec::with_capacity(hlen + data.len());
        t.extend_from_slice(&sport.to_be_bytes());
        t.extend_from_slice(&dport.to_be_bytes());
        t.extend_from_slice(&seq.to_be_bytes());
        t.extend_from_slice(&(if flags & 0x10 != 0 { ack } else { 0 }).to_be_bytes());
        t.push(((hlen / 4) as u8) << 4);
        t.push(flags);
        // window scale is not offered, so the field is the window itself
        t.extend_from_slice(&(wnd.min(0xFFFF) as u16).to_be_bytes());
        t.extend_from_slice(&[0, 0, 0, 0]);
        if syn {
            t.extend_from_slice(&[2, 4]);
            t.extend_from_slice(&(MSS as u16).to_be_bytes());
        }
        t.extend_from_slice(data);
        let c = checksum_end(checksum_add(pseudo(src, dst, 6, t.len()), &t));
        t[16..18].copy_from_slice(&c.to_be_bytes());
        self.ip_send(6, src, dst, &t);
    }

    fn tcp_rst(&mut self, src: u32, dst: u32, sport: u16, dport: u16, seq: u32, ack: u32, with_ack: bool) {
        let mut t = Vec::with_capacity(20);
        t.extend_from_slice(&sport.to_be_bytes());
        t.extend_from_slice(&dport.to_be_bytes());
        t.extend_from_slice(&seq.to_be_bytes());
        t.extend_from_slice(&ack.to_be_bytes());
        t.push(5 << 4);
        t.push(if with_ack { 0x14 } else { 0x04 });
        t.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let c = checksum_end(checksum_add(pseudo(src, dst, 6, 20), &t));
        t[16..18].copy_from_slice(&c.to_be_bytes());
        self.ip_send(6, src, dst, &t);
    }

    fn ack_now(&mut self, i: usize) {
        let seq = self.socks[i].as_ref().unwrap().tcp.snd_nxt;
        self.tcp_segment(i, seq, 0x10, &[]);
    }

    /// Start connecting socket `i` (local address and port already set).
    pub fn tcp_connect(&mut self, i: usize) {
        let iss = crate::syscall::random_u64() as u32;
        let s = self.sock(i);
        s.tcp.iss = iss;
        s.tcp.snd_una = iss;
        s.tcp.snd_nxt = iss.wrapping_add(1);
        s.tcp.st = St::SynSent;
        s.tcp.rto = SEC;
        s.tcp.rtx_at = now_ns() + SEC;
        s.connected = true;
        self.tcp_segment(i, iss, 0x02, &[]);
    }

    /// Send what the window allows.
    pub fn tcp_output(&mut self, i: usize) {
        loop {
            let s = self.socks[i].as_mut().unwrap();
            let t = &mut s.tcp;
            if !matches!(t.st, St::Established | St::CloseWait | St::FinWait1 | St::Closing | St::LastAck) {
                return;
            }
            let flight = t.in_flight();
            let unsent = t.sbuf.len() - flight;
            let room = (t.snd_wnd as usize).saturating_sub(flight);
            if unsent > 0 && t.snd_wnd == 0 && flight == 0 && t.rtx_at == 0 {
                // the peer has no room: probe it later
                t.rtx_at = now_ns() + t.rto;
            }
            if unsent > 0 && room > 0 && !t.fin_sent {
                let n = unsent.min(room).min(t.mss);
                let data: Vec<u8> = t.sbuf.range(flight..flight + n).copied().collect();
                let seq = t.snd_nxt;
                t.snd_nxt = t.snd_nxt.wrapping_add(n as u32);
                if t.rtx_at == 0 {
                    t.rtx_at = now_ns() + t.rto;
                }
                let flags = if flight + n == t.sbuf.len() { 0x18 } else { 0x10 };
                self.tcp_segment(i, seq, flags, &data);
                continue;
            }
            if t.fin_wanted && !t.fin_sent && unsent == 0 {
                t.fin_sent = true;
                let seq = t.snd_nxt;
                t.snd_nxt = t.snd_nxt.wrapping_add(1);
                t.st = match t.st {
                    St::CloseWait => St::LastAck,
                    _ => St::FinWait1,
                };
                if t.rtx_at == 0 {
                    t.rtx_at = now_ns() + t.rto;
                }
                self.tcp_segment(i, seq, 0x11, &[]);
            }
            return;
        }
    }

    fn retransmit(&mut self, i: usize, now: u64) {
        let s = self.socks[i].as_mut().unwrap();
        let t = &mut s.tcp;
        if t.snd_una == t.snd_nxt {
            if t.snd_wnd == 0 && !t.sbuf.is_empty() && matches!(t.st, St::Established | St::CloseWait) {
                // window probe: one byte past the closed window
                t.rto = (t.rto * 2).min(30 * SEC);
                t.rtx_at = now + t.rto;
                let (seq, b) = (t.snd_nxt, [t.sbuf[0]]);
                t.snd_nxt = t.snd_nxt.wrapping_add(1);
                self.tcp_segment(i, seq, 0x18, &b);
            } else {
                t.rtx_at = 0;
            }
            return;
        }
        t.retries += 1;
        let limit = if matches!(t.st, St::SynSent) { 6 } else { 12 };
        if t.retries > limit {
            s.err = ETIMEDOUT;
            s.tcp.st = St::Closed;
            s.tcp.rtx_at = 0;
            self.reap(i);
            return;
        }
        t.rto = (t.rto * 2).min(30 * SEC);
        t.rtx_at = now + t.rto;
        let una = t.snd_una;
        match t.st {
            St::SynSent => self.tcp_segment(i, una, 0x02, &[]),
            St::SynRcvd => self.tcp_segment(i, una, 0x12, &[]),
            _ => self.go_back(i),
        }
    }

    /// Send everything again from the first unacknowledged byte (after a timeout, three
    /// duplicate acknowledgements, or a window probe the peer had no room for).
    fn go_back(&mut self, i: usize) {
        let t = &mut self.sock(i).tcp;
        t.snd_nxt = t.snd_una;
        if t.fin_sent {
            t.fin_sent = false;
            t.st = match t.st {
                St::FinWait1 => St::Established,
                St::Closing | St::LastAck => St::CloseWait,
                other => other,
            };
        }
        let saved = t.rtx_at;
        self.tcp_output(i);
        let t = &mut self.sock(i).tcp;
        if saved != 0 {
            t.rtx_at = saved;
        }
    }

    fn tcp_in(&mut self, src: u32, dst: u32, b: &[u8]) {
        if b.len() < 20 {
            return;
        }
        if checksum_end(checksum_add(pseudo(src, dst, 6, b.len()), b)) != 0 {
            return;
        }
        let sport = be16(b, 0);
        let dport = be16(b, 2);
        let seq = be32(b, 4);
        let ack = be32(b, 8);
        let off = ((b[12] >> 4) as usize * 4).clamp(20, b.len());
        let flags = b[13];
        let wnd = be16(b, 14) as u32;
        let data = &b[off..];
        let (fin, syn, rst, has_ack) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0, flags & 0x10 != 0);
        let mut peer_mss = 536usize;
        let mut o = 20;
        while o < off {
            match b[o] {
                0 => break,
                1 => o += 1,
                k => {
                    let l = b.get(o + 1).copied().unwrap_or(0) as usize;
                    if l < 2 {
                        break;
                    }
                    if k == 2 && l == 4 && o + 4 <= off {
                        peer_mss = be16(b, o + 2) as usize;
                    }
                    o += l;
                }
            }
        }

        // the connection this belongs to, or a listener
        let mut found = None;
        let mut listener = None;
        for (i, s) in self.socks.iter().enumerate() {
            let Some(s) = s else { continue };
            if s.proto != Proto::Tcp || s.lport != dport {
                continue;
            }
            if s.tcp.st == St::Listen {
                if s.lip == 0 || s.lip == dst {
                    listener = Some(i);
                }
            } else if s.rip == src && s.rport == sport && (s.lip == dst || s.lip == 0) && s.tcp.st != St::Closed {
                found = Some(i);
                break;
            }
        }
        let Some(i) = found else {
            if rst {
                return;
            }
            if let (Some(l), true, false) = (listener, syn, has_ack) {
                // a new connection for a listener
                let pending = self.socks.iter().flatten().filter(|s| s.tcp.parent == Some(l) && s.tcp.st == St::SynRcvd).count();
                let lst = self.sock(l);
                if lst.tcp.backlog.len() + pending >= lst.tcp.max_backlog.max(1) {
                    return;
                }
                let c = self.alloc(Proto::Tcp);
                let iss = crate::syscall::random_u64() as u32;
                let s = self.sock(c);
                s.lip = dst;
                s.lport = dport;
                s.rip = src;
                s.rport = sport;
                s.connected = true;
                s.orphan = true; // nobody holds it until accept
                let t = &mut s.tcp;
                t.st = St::SynRcvd;
                t.parent = Some(l);
                t.iss = iss;
                t.snd_una = iss;
                t.snd_nxt = iss.wrapping_add(1);
                t.rcv_nxt = seq.wrapping_add(1);
                t.snd_wnd = wnd;
                t.mss = peer_mss.clamp(64, MSS);
                t.rtx_at = now_ns() + SEC;
                self.tcp_segment(c, iss, 0x12, &[]);
                return;
            }
            let (rseq, rack, with_ack) = if has_ack { (ack, 0, false) } else { (0, seq.wrapping_add(data.len() as u32 + syn as u32 + fin as u32), true) };
            self.tcp_rst(dst, src, dport, sport, rseq, rack, with_ack);
            return;
        };

        let st = self.sock(i).tcp.st;
        if rst {
            let s = self.sock(i);
            s.err = if st == St::SynSent { ECONNREFUSED } else { ECONNRESET };
            s.tcp.st = St::Closed;
            s.tcp.rtx_at = 0;
            s.tcp.sbuf.clear();
            if st == St::SynRcvd {
                self.socks[i] = None;
            } else {
                self.reap(i);
            }
            return;
        }
        if st == St::SynSent {
            let s = self.sock(i);
            if syn && has_ack && ack == s.tcp.snd_nxt {
                let t = &mut s.tcp;
                t.rcv_nxt = seq.wrapping_add(1);
                t.snd_una = ack;
                t.snd_wnd = wnd;
                t.mss = peer_mss.clamp(64, MSS);
                t.st = St::Established;
                t.rtx_at = 0;
                t.retries = 0;
                t.rto = SEC;
                self.ack_now(i);
                self.tcp_output(i);
            } else if has_ack {
                self.tcp_rst(dst, src, dport, sport, ack, 0, false);
            }
            return;
        }
        if syn {
            // a repeated SYN (our SYN-ACK or ACK got lost): answer again
            let s = self.sock(i);
            if st == St::SynRcvd {
                let iss = s.tcp.iss;
                self.tcp_segment(i, iss, 0x12, &[]);
            } else {
                self.ack_now(i);
            }
            return;
        }
        if !has_ack {
            return;
        }
        // acknowledgements
        {
            let s = self.sock(i);
            let t = &mut s.tcp;
            let acked = ack.wrapping_sub(t.snd_una);
            let outstanding = t.snd_nxt.wrapping_sub(t.snd_una);
            if acked > 0 && acked <= outstanding {
                let mut n = acked as usize;
                if t.st == St::SynRcvd {
                    n -= 1;
                }
                let fin_acked = t.fin_sent && ack == t.snd_nxt;
                if fin_acked {
                    n = n.saturating_sub(1);
                }
                let n = n.min(t.sbuf.len());
                t.sbuf.drain(..n);
                t.snd_una = ack;
                t.retries = 0;
                t.dupacks = 0;
                t.rto = SEC;
                t.rtx_at = if t.snd_una == t.snd_nxt { 0 } else { now_ns() + t.rto };
                let mut joined = None;
                if t.st == St::SynRcvd {
                    t.st = St::Established;
                    joined = t.parent;
                }
                if let Some(p) = joined {
                    match self.socks.get_mut(p) {
                        Some(Some(l)) if l.tcp.st == St::Listen => l.tcp.backlog.push_back(i),
                        _ => {
                            self.abort(i);
                            return;
                        }
                    }
                }
                let s = self.sock(i);
                let t = &mut s.tcp;
                if fin_acked {
                    t.st = match t.st {
                        St::FinWait1 => St::FinWait2,
                        St::Closing => {
                            t.linger_until = now_ns() + 2 * SEC;
                            St::TimeWait
                        }
                        St::LastAck => St::Closed,
                        other => other,
                    };
                    if t.st == St::Closed {
                        self.reap(i);
                        return;
                    }
                }
            } else if acked == 0 && outstanding > 0 && data.is_empty() && !fin && wnd == t.snd_wnd {
                t.dupacks += 1;
                if t.dupacks == 3 {
                    self.go_back(i);
                }
            }
            let s = self.sock(i);
            if s.tcp.snd_una == ack {
                s.tcp.snd_wnd = wnd;
            }
        }
        if self.sock(i).tcp.st == St::SynRcvd {
            return;
        }
        // data
        let mut ack_needed = false;
        let mut fin_now = false;
        {
            let s = self.sock(i);
            let t = &mut s.tcp;
            let accepts = matches!(t.st, St::Established | St::FinWait1 | St::FinWait2);
            if !data.is_empty() {
                ack_needed = true;
                let skip = t.rcv_nxt.wrapping_sub(seq) as usize;
                if accepts && skip < data.len() {
                    let take = &data[skip..];
                    let room = RBUF - t.rbuf.len();
                    let take = &take[..take.len().min(room)];
                    if !s.shut_rd {
                        t.rbuf.extend(take);
                    }
                    t.rcv_nxt = t.rcv_nxt.wrapping_add(take.len() as u32);
                }
            }
            if fin && seq.wrapping_add(data.len() as u32) == t.rcv_nxt && !t.rcv_fin {
                t.rcv_fin = true;
                t.rcv_nxt = t.rcv_nxt.wrapping_add(1);
                ack_needed = true;
                fin_now = true;
            } else if fin {
                ack_needed = true;
            }
            if fin_now {
                t.st = match t.st {
                    St::Established => St::CloseWait,
                    St::FinWait1 => St::Closing,
                    St::FinWait2 => {
                        t.linger_until = now_ns() + 2 * SEC;
                        St::TimeWait
                    }
                    other => other,
                };
            }
        }
        if ack_needed {
            self.ack_now(i);
        }
        self.tcp_output(i);
    }

    /// The program closed its last descriptor for socket `i`.
    pub fn close(&mut self, i: usize) {
        let Some(s) = self.socks[i].as_mut() else { return };
        s.orphan = true;
        if s.proto != Proto::Tcp {
            self.socks[i] = None;
            return;
        }
        match s.tcp.st {
            St::Listen => {
                let pending: Vec<usize> = s.tcp.backlog.drain(..).collect();
                self.socks[i] = None;
                for c in pending {
                    self.abort(c);
                }
                for c in 0..self.socks.len() {
                    if self.socks[c].as_ref().is_some_and(|s| s.tcp.parent == Some(i) && s.tcp.st == St::SynRcvd) {
                        self.abort(c);
                    }
                }
            }
            St::Closed | St::SynSent => self.socks[i] = None,
            St::Established | St::CloseWait | St::SynRcvd => {
                if !s.tcp.rbuf.is_empty() {
                    // unread data: reset, as Linux does
                    self.abort(i);
                    return;
                }
                s.tcp.fin_wanted = true;
                self.tcp_output(i);
            }
            _ => {}
        }
    }

    pub fn abort(&mut self, i: usize) {
        let Some(s) = self.socks[i].as_ref() else { return };
        if !matches!(s.tcp.st, St::Closed | St::Listen | St::SynSent | St::TimeWait) {
            let (src, dst, sp, dp, seq) = (s.lip, s.rip, s.lport, s.rport, s.tcp.snd_nxt);
            self.tcp_rst(src, dst, sp, dp, seq, 0, false);
        }
        self.socks[i] = None;
    }

    /// The program read from the receive buffer: tell the peer when the window opened.
    pub fn window_update(&mut self, i: usize) {
        let t = &self.sock(i).tcp;
        if matches!(t.st, St::Established | St::FinWait1 | St::FinWait2) && (t.last_wnd as usize) < RBUF / 2 && t.window() as usize >= RBUF / 2 {
            self.ack_now(i);
        }
    }

    pub fn shutdown_write(&mut self, i: usize) {
        let s = self.sock(i);
        if matches!(s.tcp.st, St::Established | St::CloseWait) {
            s.tcp.fin_wanted = true;
            self.tcp_output(i);
        }
    }

    pub fn send_room(&self, i: usize) -> usize {
        SBUF.saturating_sub(self.socks[i].as_ref().unwrap().tcp.sbuf.len())
    }

    pub fn queue_send(&mut self, i: usize, data: &[u8]) {
        self.sock(i).tcp.sbuf.extend(data);
        self.tcp_output(i);
    }

    pub fn icmp_send(&mut self, src: u32, dst: u32, data: &[u8]) {
        self.ip_send(1, src, dst, data);
    }

    pub fn raw_send(&mut self, proto: u8, src: u32, dst: u32, data: &[u8]) {
        self.ip_send(proto, src, dst, data);
    }

    pub fn has_card(&self) -> bool {
        self.nic.is_some()
    }

    pub fn card_name(&self) -> &'static str {
        self.nic.as_ref().map(|n| n.name()).unwrap_or("none")
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn link_up(&self) -> bool {
        self.nic.as_ref().is_some_and(|n| n.link_up())
    }
}
