//! getaddrinfo, freeaddrinfo, gai_strerror: numeric addresses, "localhost", /etc/hosts,
//! then DNS (A and AAAA over UDP to the servers in /etc/resolv.conf).

use crate::imp::sys::{self, sc1, sc3, sc6};
use crate::imp::types::*;
use crate::imp::{malloc, mem};
use core::ptr;

const AF_UNSPEC: c_int = 0;
const AF_INET: c_int = 2;
const AF_INET6: c_int = 10;
const SOCK_STREAM: c_int = 1;
const SOCK_DGRAM: c_int = 2;
const AI_PASSIVE: c_int = 1;
const AI_CANONNAME: c_int = 2;
const AI_NUMERICHOST: c_int = 4;
const AI_NUMERICSERV: c_int = 0x400;

const EAI_NONAME: c_int = -2;
const EAI_AGAIN: c_int = -3;
const EAI_FAIL: c_int = -4;
const EAI_FAMILY: c_int = -6;
const EAI_SOCKTYPE: c_int = -7;
const EAI_SERVICE: c_int = -8;
const EAI_MEMORY: c_int = -10;

/// musl's struct addrinfo (48 bytes).
#[repr(C)]
pub struct AddrInfo {
    flags: c_int,
    family: c_int,
    socktype: c_int,
    protocol: c_int,
    addrlen: socklen_t,
    addr: *mut u8,
    canonname: *mut c_char,
    next: *mut AddrInfo,
}

#[derive(Clone, Copy)]
enum Ip {
    V4([u8; 4]),
    V6([u8; 16]),
}

fn parse_v4(s: &[u8]) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.split(|&c| c == b'.');
    for o in out.iter_mut() {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 3 || !p.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let v = p.iter().fold(0u32, |a, &c| a * 10 + (c - b'0') as u32);
        if v > 255 {
            return None;
        }
        *o = v as u8;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

fn parse_v6(s: &[u8]) -> Option<[u8; 16]> {
    // strip a zone ("%eth0"); it is not kept
    let s = match s.iter().position(|&c| c == b'%') {
        Some(i) => &s[..i],
        None => s,
    };
    let mut head = [0u16; 8];
    let mut tail = [0u16; 8];
    let (mut nh, mut nt) = (0usize, 0usize);
    let (h, t) = match s.windows(2).position(|w| w == b"::") {
        Some(i) => (&s[..i], Some(&s[i + 2..])),
        None => (s, None),
    };
    let group = |g: &[u8]| -> Option<u16> {
        if g.is_empty() || g.len() > 4 {
            return None;
        }
        let mut v = 0u16;
        for &c in g {
            v = v * 16 + (c as char).to_digit(16)? as u16;
        }
        Some(v)
    };
    let fill = |part: &[u8], out: &mut [u16; 8], n: &mut usize| -> Option<()> {
        if part.is_empty() {
            return Some(());
        }
        let groups: &[u8] = part;
        let mut it = groups.split(|&c| c == b':').peekable();
        while let Some(g) = it.next() {
            if it.peek().is_none() && g.contains(&b'.') {
                // trailing embedded IPv4
                let v4 = parse_v4(g)?;
                if *n + 2 > 8 {
                    return None;
                }
                out[*n] = u16::from_be_bytes([v4[0], v4[1]]);
                out[*n + 1] = u16::from_be_bytes([v4[2], v4[3]]);
                *n += 2;
                return Some(());
            }
            if *n == 8 {
                return None;
            }
            out[*n] = group(g)?;
            *n += 1;
        }
        Some(())
    };
    fill(h, &mut head, &mut nh)?;
    if let Some(t) = t {
        fill(t, &mut tail, &mut nt)?;
        if nh + nt > 7 {
            return None;
        }
    } else if nh != 8 {
        return None;
    }
    let mut words = [0u16; 8];
    words[..nh].copy_from_slice(&head[..nh]);
    words[8 - nt..].copy_from_slice(&tail[..nt]);
    let mut out = [0u8; 16];
    for (i, w) in words.iter().enumerate() {
        out[2 * i..2 * i + 2].copy_from_slice(&w.to_be_bytes());
    }
    Some(out)
}

fn parse_ip(s: &[u8]) -> Option<Ip> {
    parse_v4(s).map(Ip::V4).or_else(|| parse_v6(s).map(Ip::V6))
}

fn service_port(s: &[u8], numeric_only: bool) -> Option<u16> {
    if !s.is_empty() && s.iter().all(u8::is_ascii_digit) {
        let v = s.iter().try_fold(0u32, |a, &c| {
            let v = a * 10 + (c - b'0') as u32;
            (v <= 65535).then_some(v)
        })?;
        return Some(v as u16);
    }
    if numeric_only {
        return None;
    }
    Some(match s {
        b"http" => 80,
        b"https" => 443,
        b"ssh" => 22,
        b"ftp" => 21,
        b"domain" => 53,
        b"smtp" => 25,
        b"ntp" => 123,
        _ => return None,
    })
}

/// Read a small file into `buf`; returns the bytes read.
unsafe fn read_file(path: &[u8], buf: &mut [u8]) -> usize {
    let fd = sc3(sys::OPEN, path.as_ptr() as usize, 0o2000000, 0);
    if fd < 0 {
        return 0;
    }
    let mut n = 0;
    while n < buf.len() {
        let r = sc3(sys::READ, fd as usize, buf.as_mut_ptr().add(n) as usize, buf.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    sc1(sys::CLOSE, fd as usize);
    n
}

fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// Addresses for `name` from /etc/hosts.
unsafe fn from_hosts(name: &[u8], out: &mut [Ip; 16], n: &mut usize) {
    let mut buf = [0u8; 16384];
    let len = read_file(b"/etc/hosts\0", &mut buf);
    for line in buf[..len].split(|&c| c == b'\n') {
        let line = match line.iter().position(|&c| c == b'#') {
            Some(i) => &line[..i],
            None => line,
        };
        let mut words = line.split(|c| c.is_ascii_whitespace()).filter(|w| !w.is_empty());
        let Some(addr) = words.next() else { continue };
        if words.any(|w| eq_ignore_case(w, name)) {
            if let Some(ip) = parse_ip(addr) {
                if *n < out.len() {
                    out[*n] = ip;
                    *n += 1;
                }
            }
        }
    }
}

/// The nameservers in /etc/resolv.conf (127.0.0.1 if there are none).
unsafe fn nameservers(out: &mut [Ip; 3]) -> usize {
    let mut buf = [0u8; 4096];
    let len = read_file(b"/etc/resolv.conf\0", &mut buf);
    let mut n = 0;
    for line in buf[..len].split(|&c| c == b'\n') {
        let mut w = line.split(|c| c.is_ascii_whitespace()).filter(|w| !w.is_empty());
        if w.next() == Some(b"nameserver") {
            if let Some(ip) = w.next().and_then(parse_ip) {
                if n < out.len() {
                    out[n] = ip;
                    n += 1;
                }
            }
        }
    }
    if n == 0 {
        out[0] = Ip::V4([127, 0, 0, 1]);
        n = 1;
    }
    n
}

fn now_ms() -> i64 {
    let mut ts = timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: valid pointer; CLOCK_MONOTONIC
    unsafe { crate::imp::time::clock_gettime(1, &mut ts) };
    ts.tv_sec * 1000 + ts.tv_nsec / 1_000_000
}

/// Build a DNS query for `name`, type `qtype`; returns its length.
fn build_query(name: &[u8], qtype: u16, id: u16, q: &mut [u8; 512]) -> Option<usize> {
    q[..12].copy_from_slice(&[(id >> 8) as u8, id as u8, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]); // RD, 1 question
    let mut p = 12;
    for label in name.split(|&c| c == b'.').filter(|l| !l.is_empty()) {
        if label.len() > 63 || p + label.len() + 6 > q.len() {
            return None;
        }
        q[p] = label.len() as u8;
        q[p + 1..p + 1 + label.len()].copy_from_slice(label);
        p += 1 + label.len();
    }
    q[p] = 0;
    q[p + 1..p + 5].copy_from_slice(&[(qtype >> 8) as u8, qtype as u8, 0, 1]);
    Some(p + 5)
}

/// Skip a (possibly compressed) name in a DNS message.
fn skip_name(m: &[u8], mut p: usize) -> Option<usize> {
    loop {
        let l = *m.get(p)? as usize;
        if l == 0 {
            return Some(p + 1);
        }
        if l & 0xc0 == 0xc0 {
            return Some(p + 2);
        }
        p += 1 + l;
    }
}

/// Parse answers of type `qtype`. Returns false when the server answered "no such name".
fn parse_answers(m: &[u8], id: u16, qtype: u16, out: &mut [Ip; 16], n: &mut usize) -> Option<bool> {
    if m.len() < 12 || u16::from_be_bytes([m[0], m[1]]) != id || m[2] & 0x80 == 0 {
        return None;
    }
    let rcode = m[3] & 0x0f;
    if rcode == 3 {
        return Some(false); // NXDOMAIN
    }
    if rcode != 0 {
        return None;
    }
    let qd = u16::from_be_bytes([m[4], m[5]]) as usize;
    let an = u16::from_be_bytes([m[6], m[7]]) as usize;
    let mut p = 12;
    for _ in 0..qd {
        p = skip_name(m, p)? + 4;
    }
    for _ in 0..an {
        p = skip_name(m, p)?;
        let rr = m.get(p..p + 10)?;
        let ty = u16::from_be_bytes([rr[0], rr[1]]);
        let len = u16::from_be_bytes([rr[8], rr[9]]) as usize;
        let data = m.get(p + 10..p + 10 + len)?;
        p += 10 + len;
        if ty == qtype && *n < out.len() {
            match (ty, len) {
                (1, 4) => {
                    out[*n] = Ip::V4([data[0], data[1], data[2], data[3]]);
                    *n += 1;
                }
                (28, 16) => {
                    let mut a = [0u8; 16];
                    a.copy_from_slice(data);
                    out[*n] = Ip::V6(a);
                    *n += 1;
                }
                _ => {}
            }
        }
    }
    Some(true)
}

/// Ask the nameservers for `qtype` records. Err(EAI_*) when nothing could be learned.
unsafe fn dns(name: &[u8], qtype: u16, out: &mut [Ip; 16], n: &mut usize) -> Result<(), c_int> {
    let mut servers = [Ip::V4([0; 4]); 3];
    let ns = nameservers(&mut servers);
    let mut rnd = [0u8; 2];
    sc3(318 /* getrandom */, rnd.as_mut_ptr() as usize, 2, 0);
    let id = u16::from_le_bytes(rnd);
    let mut q = [0u8; 512];
    let Some(qlen) = build_query(name, qtype, id, &mut q) else { return Err(EAI_NONAME) };
    for _attempt in 0..2 {
        for s in servers.iter().take(ns) {
            let (fam, sa, salen) = sockaddr(*s, 53);
            let fd = sc3(sys::SOCKET, fam as usize, SOCK_DGRAM as usize | 0o2000000, 0);
            if fd < 0 {
                continue;
            }
            let fd = fd as usize;
            if sc3(sys::CONNECT, fd, sa.as_ptr() as usize, salen as usize) < 0
                || sc3(sys::WRITE, fd, q.as_ptr() as usize, qlen) < 0
            {
                sc1(sys::CLOSE, fd);
                continue;
            }
            let deadline = now_ms() + 2500;
            let mut buf = [0u8; 1500];
            let result = loop {
                let left = deadline - now_ms();
                if left <= 0 {
                    break None;
                }
                let mut pfd = [fd as i32, 1 /* POLLIN */, 0];
                let r = sc3(sys::POLL, pfd.as_mut_ptr() as usize, 1, left as usize);
                if r <= 0 {
                    if r == -4 {
                        continue; // EINTR
                    }
                    break None;
                }
                let got = sc6(sys::RECVFROM, fd, buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0);
                if got <= 0 {
                    break None;
                }
                if let Some(r) = parse_answers(&buf[..got as usize], id, qtype, out, n) {
                    break Some(r);
                }
                // not our answer: keep waiting
            };
            sc1(sys::CLOSE, fd);
            match result {
                Some(true) => return Ok(()),
                Some(false) => return Err(EAI_NONAME),
                None => {}
            }
        }
    }
    Err(EAI_AGAIN)
}

/// A sockaddr_in or sockaddr_in6 for `ip`:`port`.
fn sockaddr(ip: Ip, port: u16) -> (c_int, [u8; 28], socklen_t) {
    let mut sa = [0u8; 28];
    match ip {
        Ip::V4(a) => {
            sa[0..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
            sa[2..4].copy_from_slice(&port.to_be_bytes());
            sa[4..8].copy_from_slice(&a);
            (AF_INET, sa, 16)
        }
        Ip::V6(a) => {
            sa[0..2].copy_from_slice(&(AF_INET6 as u16).to_ne_bytes());
            sa[2..4].copy_from_slice(&port.to_be_bytes());
            sa[8..24].copy_from_slice(&a);
            (AF_INET6, sa, 28)
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn getaddrinfo(node: *const c_char, service: *const c_char, hints: *const AddrInfo, res: *mut *mut AddrInfo) -> c_int {
    let (flags, family, socktype, protocol) = match hints.as_ref() {
        Some(h) => (h.flags, h.family, h.socktype, h.protocol),
        None => (0, AF_UNSPEC, 0, 0),
    };
    if family != AF_UNSPEC && family != AF_INET && family != AF_INET6 {
        return EAI_FAMILY;
    }
    if socktype != 0 && socktype != SOCK_STREAM && socktype != SOCK_DGRAM {
        return EAI_SOCKTYPE;
    }
    if node.is_null() && service.is_null() {
        return EAI_NONAME;
    }
    let port = if service.is_null() {
        0
    } else {
        match service_port(mem::cstr(service as *const u8), flags & AI_NUMERICSERV != 0) {
            Some(p) => p,
            None => return EAI_SERVICE,
        }
    };
    let mut ips = [Ip::V4([0; 4]); 16];
    let mut n = 0;
    if node.is_null() {
        let passive = flags & AI_PASSIVE != 0;
        if family != AF_INET6 {
            ips[n] = Ip::V4(if passive { [0; 4] } else { [127, 0, 0, 1] });
            n += 1;
        }
        if family != AF_INET {
            let mut a = [0u8; 16];
            if !passive {
                a[15] = 1;
            }
            ips[n] = Ip::V6(a);
            n += 1;
        }
    } else {
        let name = mem::cstr(node as *const u8);
        if let Some(ip) = parse_ip(name) {
            ips[0] = ip;
            n = 1;
        } else if flags & AI_NUMERICHOST != 0 {
            return EAI_NONAME;
        } else if eq_ignore_case(name, b"localhost") || name.len() > 10 && eq_ignore_case(&name[name.len() - 10..], b".localhost") {
            ips[0] = Ip::V4([127, 0, 0, 1]);
            let mut v6 = [0u8; 16];
            v6[15] = 1;
            ips[1] = Ip::V6(v6);
            n = 2;
        } else {
            from_hosts(name, &mut ips, &mut n);
            if n == 0 {
                let mut err = EAI_NONAME;
                if family != AF_INET6 {
                    if let Err(e) = dns(name, 1, &mut ips, &mut n) {
                        err = e;
                    }
                }
                if family != AF_INET && n < ips.len() {
                    // IPv6 addresses too; a failure here keeps the A query's answer as the reason
                    let _ = dns(name, 28, &mut ips, &mut n);
                }
                if n == 0 {
                    return if err == EAI_NONAME { EAI_NONAME } else { EAI_AGAIN };
                }
            }
        }
    }
    // one entry per address and socket type
    let types: &[(c_int, c_int)] = match socktype {
        SOCK_STREAM => &[(SOCK_STREAM, 6)],
        SOCK_DGRAM => &[(SOCK_DGRAM, 17)],
        _ => &[(SOCK_STREAM, 6), (SOCK_DGRAM, 17)],
    };
    let mut head: *mut AddrInfo = ptr::null_mut();
    let mut tail: *mut *mut AddrInfo = &mut head;
    let canon = flags & AI_CANONNAME != 0 && !node.is_null();
    for ip in &ips[..n] {
        let fam = match ip {
            Ip::V4(_) => AF_INET,
            Ip::V6(_) => AF_INET6,
        };
        if family != AF_UNSPEC && family != fam {
            continue;
        }
        for &(st, proto) in types {
            let (_, sa, salen) = sockaddr(*ip, port);
            let cn_len = if canon { mem::cstrlen(node as *const u8) + 1 } else { 0 };
            let a = malloc::malloc(core::mem::size_of::<AddrInfo>() + 28 + cn_len) as *mut AddrInfo;
            if a.is_null() {
                freeaddrinfo(head);
                return EAI_MEMORY;
            }
            let addr = (a as *mut u8).add(core::mem::size_of::<AddrInfo>());
            ptr::copy_nonoverlapping(sa.as_ptr(), addr, 28);
            let cn = if canon {
                let p = addr.add(28);
                ptr::copy_nonoverlapping(node as *const u8, p, cn_len);
                p as *mut c_char
            } else {
                ptr::null_mut()
            };
            *a = AddrInfo {
                flags,
                family: fam,
                socktype: st,
                protocol: if protocol != 0 { protocol } else { proto },
                addrlen: salen,
                addr,
                canonname: cn,
                next: ptr::null_mut(),
            };
            *tail = a;
            tail = &mut (*a).next;
        }
    }
    if head.is_null() {
        return EAI_NONAME;
    }
    *res = head;
    0
}

#[no_mangle]
pub unsafe extern "C" fn freeaddrinfo(mut a: *mut AddrInfo) {
    while !a.is_null() {
        let next = (*a).next;
        malloc::free(a as *mut u8);
        a = next;
    }
}

#[no_mangle]
pub unsafe extern "C" fn gai_strerror(e: c_int) -> *const c_char {
    let m: &'static [u8] = match e {
        -1 => b"Invalid flags\0",
        EAI_NONAME => b"Name does not resolve\0",
        EAI_AGAIN => b"Try again\0",
        EAI_FAIL => b"Non-recoverable error\0",
        EAI_FAMILY => b"Unrecognized address family or invalid length\0",
        EAI_SOCKTYPE => b"Unrecognized socket type\0",
        EAI_SERVICE => b"Unrecognized service\0",
        EAI_MEMORY => b"Out of memory\0",
        -11 => b"System error\0",
        -12 => b"Overflow\0",
        _ => b"Unknown error\0",
    };
    m.as_ptr() as *const c_char
}
