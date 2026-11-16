//! Networking names and addresses: inet_* conversions, interfaces (if_*, getifaddrs over
//! netlink), the hostent/servent/protoent lookups, getnameinfo, h_errno, and the resolver
//! interface (res_*query, ns_* message parsing, dn_expand) for programs that read DNS
//! records themselves.

use crate::fl;
use crate::fl::resolve::{parse_ip, Ip};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;
type size_t = usize;

const AF_INET: c_int = 2;
const AF_INET6: c_int = 10;
const AF_PACKET: c_int = 17;
const AF_NETLINK: usize = 16;

const HOST_NOT_FOUND: c_int = 1;
const TRY_AGAIN: c_int = 2;
const NO_RECOVERY: c_int = 3;
const NO_DATA: c_int = 4;

#[no_mangle]
pub static in6addr_any: [u8; 16] = [0; 16];
#[no_mangle]
pub static in6addr_loopback: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

#[no_mangle]
pub unsafe extern "C" fn __h_errno_location() -> *mut c_int {
    ptr::addr_of_mut!((*fl::tls::current()).h_errno)
}
fn set_h(e: c_int) {
    // SAFETY: the current thread's block.
    unsafe { *__h_errno_location() = e }
}

#[no_mangle]
pub unsafe extern "C" fn hstrerror(e: c_int) -> *const c_char {
    match e {
        0 => b"Resolver Error 0 (no error)\0".as_ptr(),
        HOST_NOT_FOUND => b"Unknown host\0".as_ptr(),
        TRY_AGAIN => b"Host name lookup failure\0".as_ptr(),
        NO_RECOVERY => b"Unknown server error\0".as_ptr(),
        NO_DATA => b"No address associated with name\0".as_ptr(),
        _ => b"Unknown resolver error\0".as_ptr(),
    }
}
#[no_mangle]
pub unsafe extern "C" fn herror(s: *const c_char) {
    let e = *__h_errno_location();
    let mut v: Vec<u8> = Vec::new();
    if !s.is_null() && *s != 0 {
        v.extend_from_slice(fl::mem::cstr(s));
        v.extend_from_slice(b": ");
    }
    v.extend_from_slice(fl::mem::cstr(hstrerror(e)));
    v.push(b'\n');
    fl::file::write(2, v.as_ptr() as *const c_void, v.len());
}

// --- address text ------------------------------------------------------------------------------

/// inet_aton's forms: a.b.c.d, a.b.c (c 16 bits), a.b (b 24 bits), a (32 bits); each part
/// decimal, octal (leading 0) or hexadecimal (0x).
fn aton(s: &[u8]) -> Option<u32> {
    let mut parts = [0u32; 4];
    let mut n = 0;
    let mut i = 0;
    loop {
        if n == 4 || i >= s.len() || !s[i].is_ascii_digit() {
            return None;
        }
        let (base, mut j) = if s[i] == b'0' && i + 1 < s.len() && (s[i + 1] | 0x20) == b'x' {
            (16, i + 2)
        } else if s[i] == b'0' {
            (8, i)
        } else {
            (10, i)
        };
        let st = j;
        let mut v: u64 = 0;
        while j < s.len() {
            let d = match s[j] {
                c @ b'0'..=b'9' => (c - b'0') as u64,
                c @ b'a'..=b'f' if base == 16 => (c - b'a' + 10) as u64,
                c @ b'A'..=b'F' if base == 16 => (c - b'A' + 10) as u64,
                _ => break,
            };
            if d >= base {
                return None;
            }
            v = v * base + d;
            if v > u32::MAX as u64 {
                return None;
            }
            j += 1;
        }
        if j == st && base == 16 {
            return None;
        }
        parts[n] = v as u32;
        n += 1;
        if j >= s.len() || s[j] == b' ' || s[j] == b'\t' || s[j] == b'\n' {
            break;
        }
        if s[j] != b'.' {
            return None;
        }
        i = j + 1;
    }
    let lim = |k: usize, max: u32| parts[k] <= max;
    let v = match n {
        1 => parts[0],
        2 if lim(0, 255) && lim(1, 0xff_ffff) => parts[0] << 24 | parts[1],
        3 if lim(0, 255) && lim(1, 255) && lim(2, 0xffff) => parts[0] << 24 | parts[1] << 16 | parts[2],
        4 if (0..4).all(|k| lim(k, 255)) => parts[0] << 24 | parts[1] << 16 | parts[2] << 8 | parts[3],
        _ => return None,
    };
    Some(v)
}

#[no_mangle]
pub unsafe extern "C" fn inet_aton(s: *const c_char, out: *mut u32) -> c_int {
    match aton(fl::mem::cstr(s)) {
        Some(v) => {
            if !out.is_null() {
                *out = v.to_be();
            }
            1
        }
        None => 0,
    }
}
#[no_mangle]
pub unsafe extern "C" fn inet_addr(s: *const c_char) -> u32 {
    aton(fl::mem::cstr(s)).map_or(u32::MAX, |v| v.to_be())
}
#[no_mangle]
pub unsafe extern "C" fn inet_network(s: *const c_char) -> u32 {
    aton(fl::mem::cstr(s)).unwrap_or(u32::MAX)
}

fn fmt_v4(a: [u8; 4], out: &mut Vec<u8>) {
    for (i, b) in a.iter().enumerate() {
        if i > 0 {
            out.push(b'.');
        }
        let mut t = [0u8; 3];
        let mut n = 0;
        let mut x = *b;
        loop {
            t[n] = b'0' + x % 10;
            n += 1;
            x /= 10;
            if x == 0 {
                break;
            }
        }
        for k in (0..n).rev() {
            out.push(t[k]);
        }
    }
}

/// RFC 5952 text: lowercase, the longest run of two or more zero groups as "::", and an
/// IPv4 tail for v4-mapped and v4-compatible addresses.
fn fmt_v6(a: [u8; 16], out: &mut Vec<u8>) {
    let g: Vec<u16> = (0..8).map(|i| u16::from_be_bytes([a[2 * i], a[2 * i + 1]])).collect();
    let v4tail = (g[..5].iter().all(|&x| x == 0) && g[5] == 0xffff) || (g[..6].iter().all(|&x| x == 0) && (g[6] != 0 || g[7] > 1));
    let ngroups = if v4tail { 6 } else { 8 };
    let (mut best, mut best_len) = (usize::MAX, 0);
    let mut i = 0;
    while i < ngroups {
        if g[i] == 0 {
            let st = i;
            while i < ngroups && g[i] == 0 {
                i += 1;
            }
            if i - st > best_len && i - st >= 2 {
                best = st;
                best_len = i - st;
            }
        } else {
            i += 1;
        }
    }
    let hex = |out: &mut Vec<u8>, x: u16| {
        let mut started = false;
        for k in (0..4).rev() {
            let d = ((x >> (k * 4)) & 0xf) as u8;
            if d != 0 || started || k == 0 {
                started = true;
                out.push(if d < 10 { b'0' + d } else { b'a' + d - 10 });
            }
        }
    };
    let mut i = 0;
    while i < ngroups {
        if i == best {
            out.extend_from_slice(b"::");
            i += best_len;
            continue;
        }
        if i > 0 && out.last() != Some(&b':') {
            out.push(b':');
        }
        hex(out, g[i]);
        i += 1;
    }
    if v4tail {
        if out.last() != Some(&b':') {
            out.push(b':');
        }
        fmt_v4([a[12], a[13], a[14], a[15]], out);
    }
}

static mut NTOA: [u8; 16] = [0; 16];

#[no_mangle]
pub unsafe extern "C" fn inet_ntoa(a: u32) -> *mut c_char {
    let mut v = Vec::new();
    fmt_v4(a.to_ne_bytes(), &mut v);
    let buf = &mut *ptr::addr_of_mut!(NTOA);
    buf[..v.len()].copy_from_slice(&v);
    buf[v.len()] = 0;
    buf.as_mut_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn inet_ntop(af: c_int, src: *const c_void, dst: *mut c_char, size: u32) -> *const c_char {
    let mut v = Vec::new();
    match af {
        AF_INET => fmt_v4(*(src as *const [u8; 4]), &mut v),
        AF_INET6 => fmt_v6(*(src as *const [u8; 16]), &mut v),
        _ => {
            fl::errno::set(97); // EAFNOSUPPORT
            return ptr::null();
        }
    }
    if v.len() + 1 > size as usize {
        fl::errno::set(28); // ENOSPC
        return ptr::null();
    }
    ptr::copy_nonoverlapping(v.as_ptr(), dst, v.len());
    *dst.add(v.len()) = 0;
    dst
}

#[no_mangle]
pub unsafe extern "C" fn inet_pton(af: c_int, src: *const c_char, dst: *mut c_void) -> c_int {
    let s = fl::mem::cstr(src);
    match (af, parse_ip(s)) {
        (AF_INET, Some(Ip::V4(a))) => {
            *(dst as *mut [u8; 4]) = a;
            1
        }
        (AF_INET6, Some(Ip::V6(a))) => {
            *(dst as *mut [u8; 16]) = a;
            1
        }
        (AF_INET, _) | (AF_INET6, _) => 0,
        _ => {
            fl::errno::set(97);
            -1
        }
    }
}

// --- interfaces ----------------------------------------------------------------------------------

/// struct ifreq: the name, then a union (the index for SIOCGIFINDEX).
#[repr(C)]
struct IfReq {
    name: [u8; 16],
    u: [u8; 24],
}

unsafe fn ioctl_sock() -> isize {
    fl::sys::sc3(fl::sys::SOCKET, AF_INET as usize, 2 /* SOCK_DGRAM */ | 0o2000000, 0)
}

#[no_mangle]
pub unsafe extern "C" fn if_nametoindex(name: *const c_char) -> u32 {
    let n = fl::mem::cstr(name);
    if n.len() >= 16 {
        return 0;
    }
    let mut r = IfReq { name: [0; 16], u: [0; 24] };
    r.name[..n.len()].copy_from_slice(n);
    let fd = ioctl_sock();
    if fd < 0 {
        return 0;
    }
    let e = fl::sys::sc3(fl::sys::IOCTL, fd as usize, 0x8933 /* SIOCGIFINDEX */, &mut r as *mut _ as usize);
    fl::sys::sc1(fl::sys::CLOSE, fd as usize);
    if e < 0 {
        return 0;
    }
    i32::from_ne_bytes([r.u[0], r.u[1], r.u[2], r.u[3]]) as u32
}

#[no_mangle]
pub unsafe extern "C" fn if_indextoname(index: u32, name: *mut c_char) -> *mut c_char {
    let mut r = IfReq { name: [0; 16], u: [0; 24] };
    r.u[..4].copy_from_slice(&(index as i32).to_ne_bytes());
    let fd = ioctl_sock();
    if fd < 0 {
        return ptr::null_mut();
    }
    let e = fl::sys::sc3(fl::sys::IOCTL, fd as usize, 0x8910 /* SIOCGIFNAME */, &mut r as *mut _ as usize);
    fl::sys::sc1(fl::sys::CLOSE, fd as usize);
    if e < 0 {
        fl::errno::set(if e == -19 { 6 /* ENXIO */ } else { (-e) as i32 });
        return ptr::null_mut();
    }
    ptr::copy_nonoverlapping(r.name.as_ptr(), name, 16);
    *name.add(15) = 0;
    name
}

#[repr(C)]
pub struct IfNameIndex {
    index: u32,
    name: *mut c_char,
}

#[no_mangle]
pub unsafe extern "C" fn if_nameindex() -> *mut IfNameIndex {
    let links = netlink_links();
    let total = (links.len() + 1) * 16 + links.iter().map(|l| l.name.len() + 1).sum::<usize>();
    let base = fl::malloc::imalloc(total);
    if base.is_null() {
        return ptr::null_mut();
    }
    let arr = base as *mut IfNameIndex;
    let mut p = base.add((links.len() + 1) * 16);
    for (i, l) in links.iter().enumerate() {
        ptr::copy_nonoverlapping(l.name.as_ptr(), p, l.name.len());
        *p.add(l.name.len()) = 0;
        *arr.add(i) = IfNameIndex { index: l.index, name: p };
        p = p.add(l.name.len() + 1);
    }
    *arr.add(links.len()) = IfNameIndex { index: 0, name: ptr::null_mut() };
    arr
}
#[no_mangle]
pub unsafe extern "C" fn if_freenameindex(p: *mut IfNameIndex) {
    fl::malloc::ifree(p as *mut u8);
}

struct Link {
    index: u32,
    flags: u32,
    name: Vec<u8>,
    hw: Vec<u8>,
    hwtype: u16,
}
struct Addr {
    index: u32,
    family: u8,
    prefix: u8,
    addr: Vec<u8>,
    peer: Option<Vec<u8>>,
    brd: Option<Vec<u8>>,
}

/// One rtnetlink dump: call `f` with (message type, payload) for each message.
unsafe fn netlink_dump(req_type: u16, family: u8, mut f: impl FnMut(u16, &[u8])) -> bool {
    let fd = fl::sys::sc3(fl::sys::SOCKET, AF_NETLINK, 3 /* SOCK_RAW */ | 0o2000000, 0 /* NETLINK_ROUTE */);
    if fd < 0 {
        return false;
    }
    let fd = fd as usize;
    // nlmsghdr (16) + rtgenmsg padded to ifinfomsg size (16)
    let mut req = [0u8; 32];
    req[0..4].copy_from_slice(&32u32.to_ne_bytes());
    req[4..6].copy_from_slice(&req_type.to_ne_bytes());
    req[6..8].copy_from_slice(&(1u16 | 0x300).to_ne_bytes()); // NLM_F_REQUEST | NLM_F_DUMP
    req[8..12].copy_from_slice(&1u32.to_ne_bytes());
    req[16] = family;
    let mut sa = [0u8; 12];
    sa[0] = AF_NETLINK as u8;
    if fl::sys::sc6(fl::sys::SENDTO, fd, req.as_ptr() as usize, 32, 0, sa.as_ptr() as usize, 12) < 0 {
        fl::sys::sc1(fl::sys::CLOSE, fd);
        return false;
    }
    let mut buf = alloc::vec![0u8; 32768];
    let mut ok = false;
    'outer: loop {
        let n = fl::sys::sc6(fl::sys::RECVFROM, fd, buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0);
        if n <= 0 {
            break;
        }
        let mut p = 0usize;
        let n = n as usize;
        while p + 16 <= n {
            let len = u32::from_ne_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
            let ty = u16::from_ne_bytes([buf[p + 4], buf[p + 5]]);
            if len < 16 || p + len > n {
                break 'outer;
            }
            match ty {
                3 => {
                    ok = true; // NLMSG_DONE
                    break 'outer;
                }
                2 => break 'outer, // NLMSG_ERROR
                _ => f(ty, &buf[p + 16..p + len]),
            }
            p += (len + 3) & !3;
        }
    }
    fl::sys::sc1(fl::sys::CLOSE, fd);
    ok
}

/// rtattrs in `b`: (type, data).
fn attrs(mut b: &[u8]) -> Vec<(u16, &[u8])> {
    let mut v = Vec::new();
    while b.len() >= 4 {
        let len = u16::from_ne_bytes([b[0], b[1]]) as usize;
        let ty = u16::from_ne_bytes([b[2], b[3]]) & 0x3fff;
        if len < 4 || len > b.len() {
            break;
        }
        v.push((ty, &b[4..len]));
        let adv = (len + 3) & !3;
        if adv >= b.len() {
            break;
        }
        b = &b[adv..];
    }
    v
}

unsafe fn netlink_links() -> Vec<Link> {
    let mut links = Vec::new();
    netlink_dump(18 /* RTM_GETLINK */, 0, |ty, m| {
        if ty != 16 || m.len() < 16 {
            return;
        }
        let hwtype = u16::from_ne_bytes([m[2], m[3]]);
        let index = i32::from_ne_bytes([m[4], m[5], m[6], m[7]]) as u32;
        let flags = u32::from_ne_bytes([m[8], m[9], m[10], m[11]]);
        let mut l = Link { index, flags, name: Vec::new(), hw: Vec::new(), hwtype };
        for (t, d) in attrs(&m[16..]) {
            match t {
                3 => l.name = d.split(|&c| c == 0).next().unwrap_or(b"").to_vec(),
                1 => l.hw = d.to_vec(),
                _ => {}
            }
        }
        links.push(l);
    });
    links
}

unsafe fn netlink_addrs() -> Vec<Addr> {
    let mut addrs = Vec::new();
    netlink_dump(22 /* RTM_GETADDR */, 0, |ty, m| {
        if ty != 20 || m.len() < 8 {
            return;
        }
        let family = m[0];
        let prefix = m[1];
        let index = u32::from_ne_bytes([m[4], m[5], m[6], m[7]]);
        let (mut address, mut local, mut brd) = (None, None, None);
        for (t, d) in attrs(&m[8..]) {
            match t {
                1 => address = Some(d.to_vec()),
                2 => local = Some(d.to_vec()),
                4 => brd = Some(d.to_vec()),
                _ => {}
            }
        }
        let (addr, peer) = match (local, address) {
            (Some(l), Some(a)) if l != a => (l, Some(a)),
            (Some(l), _) => (l, None),
            (None, Some(a)) => (a, None),
            (None, None) => return,
        };
        addrs.push(Addr { index, family, prefix, addr, peer, brd });
    });
    addrs
}

/// glibc's struct ifaddrs.
#[repr(C)]
pub struct IfAddrs {
    next: *mut IfAddrs,
    name: *mut c_char,
    flags: u32,
    addr: *mut u8,
    netmask: *mut u8,
    ifu: *mut u8,
    data: *mut c_void,
}

const IFF_BROADCAST: u32 = 2;
const IFF_POINTOPOINT: u32 = 0x10;

fn sockaddr_ip(family: u8, a: &[u8], scope: u32) -> [u8; 28] {
    let mut s = [0u8; 28];
    s[0..2].copy_from_slice(&(family as u16).to_ne_bytes());
    if family as c_int == AF_INET && a.len() >= 4 {
        s[4..8].copy_from_slice(&a[..4]);
    } else if a.len() >= 16 {
        s[8..24].copy_from_slice(&a[..16]);
        // link-local: the scope is the interface
        if a[0] == 0xfe && a[1] & 0xc0 == 0x80 {
            s[24..28].copy_from_slice(&scope.to_ne_bytes());
        }
    }
    s
}

fn netmask(family: u8, prefix: u8) -> [u8; 28] {
    let n = if family as c_int == AF_INET { 4 } else { 16 };
    let mut m = [0u8; 16];
    for i in 0..n {
        let bits = (prefix as i32 - 8 * i as i32).clamp(0, 8);
        m[i] = (0xff00u16 >> bits) as u8;
    }
    sockaddr_ip(family, &m[..n], 0)
}

#[no_mangle]
pub unsafe extern "C" fn getifaddrs(out: *mut *mut IfAddrs) -> c_int {
    let links = netlink_links();
    let addrs = netlink_addrs();
    // one AF_PACKET entry per link, then one per address (as glibc lists them)
    struct E {
        name: Vec<u8>,
        flags: u32,
        addr: Option<[u8; 28]>,
        mask: Option<[u8; 28]>,
        ifu: Option<[u8; 28]>,
    }
    let mut es: Vec<E> = Vec::new();
    for l in &links {
        // sockaddr_ll: family, protocol, ifindex, hatype, pkttype, halen, addr[8]
        let mut s = [0u8; 28];
        s[0..2].copy_from_slice(&(AF_PACKET as u16).to_ne_bytes());
        s[4..8].copy_from_slice(&(l.index as i32).to_ne_bytes());
        s[8..10].copy_from_slice(&l.hwtype.to_ne_bytes());
        let hl = l.hw.len().min(8);
        s[11] = hl as u8;
        s[12..12 + hl].copy_from_slice(&l.hw[..hl]);
        es.push(E { name: l.name.clone(), flags: l.flags, addr: Some(s), mask: None, ifu: None });
    }
    for a in &addrs {
        let Some(l) = links.iter().find(|l| l.index == a.index) else { continue };
        let ifu = if l.flags & IFF_POINTOPOINT != 0 {
            a.peer.as_ref().map(|p| sockaddr_ip(a.family, p, a.index))
        } else if l.flags & IFF_BROADCAST != 0 {
            a.brd.as_ref().map(|b| sockaddr_ip(a.family, b, a.index))
        } else {
            None
        };
        es.push(E {
            name: l.name.clone(),
            flags: l.flags,
            addr: Some(sockaddr_ip(a.family, &a.addr, a.index)),
            mask: Some(netmask(a.family, a.prefix)),
            ifu,
        });
    }
    if es.is_empty() {
        *out = ptr::null_mut();
        return 0;
    }
    // one block: entries, then the sockaddrs (32 bytes each), then the names
    let ent = core::mem::size_of::<IfAddrs>();
    let total = es.len() * (ent + 3 * 32) + es.iter().map(|e| e.name.len() + 1).sum::<usize>();
    let base = fl::malloc::imalloc(total);
    if base.is_null() {
        fl::errno::set(12);
        return -1;
    }
    ptr::write_bytes(base, 0, total);
    let mut sa = base.add(es.len() * ent);
    let mut names = sa.add(es.len() * 3 * 32);
    for (i, e) in es.iter().enumerate() {
        let p = base.add(i * ent) as *mut IfAddrs;
        let mut put = |s: Option<[u8; 28]>| -> *mut u8 {
            match s {
                Some(s) => {
                    let d = sa;
                    ptr::copy_nonoverlapping(s.as_ptr(), d, 28);
                    sa = sa.add(32);
                    d
                }
                None => ptr::null_mut(),
            }
        };
        (*p).addr = put(e.addr);
        (*p).netmask = put(e.mask);
        (*p).ifu = put(e.ifu);
        ptr::copy_nonoverlapping(e.name.as_ptr(), names, e.name.len());
        (*p).name = names;
        names = names.add(e.name.len() + 1);
        (*p).flags = e.flags;
        (*p).next = if i + 1 < es.len() { base.add((i + 1) * ent) as *mut IfAddrs } else { ptr::null_mut() };
    }
    *out = base as *mut IfAddrs;
    0
}
#[no_mangle]
pub unsafe extern "C" fn freeifaddrs(p: *mut IfAddrs) {
    fl::malloc::ifree(p as *mut u8);
}

// --- services and protocols ------------------------------------------------------------------------

const SERVICES: &[(&[u8], u16, &[u8])] = &[
    (b"ftp", 21, b"tcp"),
    (b"ssh", 22, b"tcp"),
    (b"telnet", 23, b"tcp"),
    (b"smtp", 25, b"tcp"),
    (b"domain", 53, b"tcp"),
    (b"domain", 53, b"udp"),
    (b"http", 80, b"tcp"),
    (b"pop3", 110, b"tcp"),
    (b"ntp", 123, b"udp"),
    (b"imap", 143, b"tcp"),
    (b"https", 443, b"tcp"),
    (b"https", 443, b"udp"),
    (b"submissions", 465, b"tcp"),
    (b"submission", 587, b"tcp"),
    (b"imaps", 993, b"tcp"),
    (b"pop3s", 995, b"tcp"),
    (b"socks", 1080, b"tcp"),
    (b"mdns", 5353, b"udp"),
];
const PROTOCOLS: &[(&[u8], c_int)] = &[
    (b"ip", 0),
    (b"icmp", 1),
    (b"igmp", 2),
    (b"tcp", 6),
    (b"udp", 17),
    (b"ipv6", 41),
    (b"ipv6-icmp", 58),
    (b"sctp", 132),
    (b"udplite", 136),
    (b"raw", 255),
];

/// (name, port, proto) entries from /etc/services, or the built-in table.
unsafe fn services() -> Vec<(Vec<u8>, u16, Vec<u8>)> {
    let mut buf = alloc::vec![0u8; 1 << 18];
    let n = fl::resolve::read_file(b"/etc/services\0", &mut buf);
    let mut v = Vec::new();
    for line in buf[..n].split(|&c| c == b'\n') {
        let line = line.split(|&c| c == b'#').next().unwrap_or(b"");
        let mut w = line.split(|c| c.is_ascii_whitespace()).filter(|w| !w.is_empty());
        let (Some(name), Some(pp)) = (w.next(), w.next()) else { continue };
        let mut it = pp.splitn(2, |&c| c == b'/');
        let (Some(port), Some(proto)) = (it.next(), it.next()) else { continue };
        let Ok(port) = core::str::from_utf8(port).unwrap_or("x").parse::<u16>() else { continue };
        v.push((name.to_vec(), port, proto.to_vec()));
        for alias in w {
            v.push((alias.to_vec(), port, proto.to_vec()));
        }
    }
    if v.is_empty() {
        v = SERVICES.iter().map(|&(a, b, c)| (a.to_vec(), b, c.to_vec())).collect();
    }
    v
}

#[repr(C)]
pub struct Servent {
    name: *mut c_char,
    aliases: *mut *mut c_char,
    port: c_int,
    proto: *mut c_char,
}

unsafe fn fill_servent(e: &(Vec<u8>, u16, Vec<u8>), s: *mut Servent, buf: *mut c_char, len: size_t) -> c_int {
    let need = 8 + e.0.len() + 1 + e.2.len() + 1 + 8;
    if need > len {
        return 34;
    }
    let al = ((buf as usize + 7) & !7) as *mut *mut c_char;
    *al = ptr::null_mut();
    let mut p = (al as *mut u8).add(8);
    if p.add(e.0.len() + e.2.len() + 2) > buf.add(len) {
        return 34;
    }
    (*s).name = p;
    ptr::copy_nonoverlapping(e.0.as_ptr(), p, e.0.len());
    *p.add(e.0.len()) = 0;
    p = p.add(e.0.len() + 1);
    (*s).proto = p;
    ptr::copy_nonoverlapping(e.2.as_ptr(), p, e.2.len());
    *p.add(e.2.len()) = 0;
    (*s).aliases = al;
    (*s).port = (e.1.to_be()) as c_int;
    0
}

#[no_mangle]
pub unsafe extern "C" fn getservbyname_r(name: *const c_char, proto: *const c_char, s: *mut Servent, buf: *mut c_char, len: size_t, r: *mut *mut Servent) -> c_int {
    *r = ptr::null_mut();
    let n = fl::mem::cstr(name);
    let pr: Option<&[u8]> = if proto.is_null() { None } else { Some(fl::mem::cstr(proto)) };
    for e in services() {
        if e.0 == n && pr.map_or(true, |p| p == &e.2[..]) {
            let x = fill_servent(&e, s, buf, len);
            if x == 0 {
                *r = s;
            }
            return x;
        }
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn getservbyport_r(port: c_int, proto: *const c_char, s: *mut Servent, buf: *mut c_char, len: size_t, r: *mut *mut Servent) -> c_int {
    *r = ptr::null_mut();
    let port = u16::from_be(port as u16);
    let pr: Option<&[u8]> = if proto.is_null() { None } else { Some(fl::mem::cstr(proto)) };
    for e in services() {
        if e.1 == port && pr.map_or(true, |p| p == &e.2[..]) {
            let x = fill_servent(&e, s, buf, len);
            if x == 0 {
                *r = s;
            }
            return x;
        }
    }
    0
}

static mut SERV: Servent = Servent { name: ptr::null_mut(), aliases: ptr::null_mut(), port: 0, proto: ptr::null_mut() };
static mut SERV_BUF: [u8; 512] = [0; 512];

#[no_mangle]
pub unsafe extern "C" fn getservbyname(name: *const c_char, proto: *const c_char) -> *mut Servent {
    let mut r = ptr::null_mut();
    getservbyname_r(name, proto, ptr::addr_of_mut!(SERV), ptr::addr_of_mut!(SERV_BUF) as *mut c_char, 512, &mut r);
    r
}
#[no_mangle]
pub unsafe extern "C" fn getservbyport(port: c_int, proto: *const c_char) -> *mut Servent {
    let mut r = ptr::null_mut();
    getservbyport_r(port, proto, ptr::addr_of_mut!(SERV), ptr::addr_of_mut!(SERV_BUF) as *mut c_char, 512, &mut r);
    r
}
#[no_mangle]
pub extern "C" fn setservent(_stay: c_int) {}
#[no_mangle]
pub extern "C" fn endservent() {}

#[repr(C)]
pub struct Protoent {
    name: *mut c_char,
    aliases: *mut *mut c_char,
    proto: c_int,
}

unsafe fn protocols() -> Vec<(Vec<u8>, c_int)> {
    let mut buf = alloc::vec![0u8; 1 << 14];
    let n = fl::resolve::read_file(b"/etc/protocols\0", &mut buf);
    let mut v = Vec::new();
    for line in buf[..n].split(|&c| c == b'\n') {
        let line = line.split(|&c| c == b'#').next().unwrap_or(b"");
        let mut w = line.split(|c| c.is_ascii_whitespace()).filter(|w| !w.is_empty());
        let (Some(name), Some(num)) = (w.next(), w.next()) else { continue };
        let Ok(num) = core::str::from_utf8(num).unwrap_or("x").parse::<c_int>() else { continue };
        v.push((name.to_vec(), num));
        for a in w {
            v.push((a.to_vec(), num));
        }
    }
    if v.is_empty() {
        v = PROTOCOLS.iter().map(|&(a, b)| (a.to_vec(), b)).collect();
    }
    v
}

unsafe fn fill_proto(e: &(Vec<u8>, c_int), p: *mut Protoent, buf: *mut c_char, len: size_t, r: *mut *mut Protoent) -> c_int {
    let al = ((buf as usize + 7) & !7) as *mut *mut c_char;
    let s = (al as *mut u8).add(8);
    if s.add(e.0.len() + 1) > buf.add(len) {
        return 34;
    }
    *al = ptr::null_mut();
    ptr::copy_nonoverlapping(e.0.as_ptr(), s, e.0.len());
    *s.add(e.0.len()) = 0;
    (*p).name = s;
    (*p).aliases = al;
    (*p).proto = e.1;
    *r = p;
    0
}

#[no_mangle]
pub unsafe extern "C" fn getprotobyname_r(name: *const c_char, p: *mut Protoent, buf: *mut c_char, len: size_t, r: *mut *mut Protoent) -> c_int {
    *r = ptr::null_mut();
    let n = fl::mem::cstr(name);
    for e in protocols() {
        if e.0 == n {
            return fill_proto(&e, p, buf, len, r);
        }
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn getprotobynumber_r(num: c_int, p: *mut Protoent, buf: *mut c_char, len: size_t, r: *mut *mut Protoent) -> c_int {
    *r = ptr::null_mut();
    for e in protocols() {
        if e.1 == num {
            return fill_proto(&e, p, buf, len, r);
        }
    }
    0
}
static mut PROTO: Protoent = Protoent { name: ptr::null_mut(), aliases: ptr::null_mut(), proto: 0 };
static mut PROTO_BUF: [u8; 256] = [0; 256];
#[no_mangle]
pub unsafe extern "C" fn getprotobyname(name: *const c_char) -> *mut Protoent {
    let mut r = ptr::null_mut();
    getprotobyname_r(name, ptr::addr_of_mut!(PROTO), ptr::addr_of_mut!(PROTO_BUF) as *mut c_char, 256, &mut r);
    r
}
#[no_mangle]
pub unsafe extern "C" fn getprotobynumber(num: c_int) -> *mut Protoent {
    let mut r = ptr::null_mut();
    getprotobynumber_r(num, ptr::addr_of_mut!(PROTO), ptr::addr_of_mut!(PROTO_BUF) as *mut c_char, 256, &mut r);
    r
}

// --- hostent --------------------------------------------------------------------------------------

#[repr(C)]
pub struct Hostent {
    name: *mut c_char,
    aliases: *mut *mut c_char,
    addrtype: c_int,
    length: c_int,
    addr_list: *mut *mut c_char,
}

#[repr(C)]
struct AddrInfo {
    flags: c_int,
    family: c_int,
    socktype: c_int,
    protocol: c_int,
    addrlen: u32,
    addr: *mut u8,
    canonname: *mut c_char,
    next: *mut AddrInfo,
}
extern "C" {
    fn getaddrinfo(node: *const c_char, service: *const c_char, hints: *const AddrInfo, res: *mut *mut AddrInfo) -> c_int;
    fn freeaddrinfo(a: *mut AddrInfo);
}

/// Look `name` up as `af` and lay the hostent out in `buf`.
unsafe fn host_lookup(name: *const c_char, af: c_int, h: *mut Hostent, buf: *mut c_char, len: size_t, r: *mut *mut Hostent, herr: *mut c_int) -> c_int {
    *r = ptr::null_mut();
    let hints = AddrInfo { flags: 2 /* AI_CANONNAME */, family: af, socktype: 1, protocol: 0, addrlen: 0, addr: ptr::null_mut(), canonname: ptr::null_mut(), next: ptr::null_mut() };
    let mut res = ptr::null_mut();
    let e = getaddrinfo(name, ptr::null(), &hints, &mut res);
    if e != 0 {
        let he = if e == -3 { TRY_AGAIN } else if e == -2 || e == -5 { HOST_NOT_FOUND } else { NO_RECOVERY };
        if !herr.is_null() {
            *herr = he;
        }
        set_h(he);
        return if e == -3 { 11 /* EAGAIN */ } else { 2 /* ENOENT */ };
    }
    let alen = if af == AF_INET6 { 16 } else { 4 };
    let mut addrs: Vec<Vec<u8>> = Vec::new();
    let canon: Vec<u8> = if (*res).canonname.is_null() { fl::mem::cstr(name).to_vec() } else { fl::mem::cstr((*res).canonname).to_vec() };
    let mut a = res;
    while !a.is_null() {
        if (*a).family == af {
            let off = if af == AF_INET6 { 8 } else { 4 };
            let v = core::slice::from_raw_parts((*a).addr.add(off), alen).to_vec();
            if !addrs.contains(&v) {
                addrs.push(v);
            }
        }
        a = (*a).next;
    }
    freeaddrinfo(res);
    if addrs.is_empty() {
        if !herr.is_null() {
            *herr = NO_DATA;
        }
        set_h(NO_DATA);
        return 2;
    }
    // aliases[1], addr_list[n+1], addresses, name
    let base = (buf as usize + 7) & !7;
    let need = base - buf as usize + 8 + (addrs.len() + 1) * 8 + addrs.len() * alen + canon.len() + 1;
    if need > len {
        if !herr.is_null() {
            *herr = NETDB_INTERNAL;
        }
        return 34;
    }
    let aliases = base as *mut *mut c_char;
    *aliases = ptr::null_mut();
    let list = aliases.add(1);
    let mut p = list.add(addrs.len() + 1) as *mut u8;
    for (i, v) in addrs.iter().enumerate() {
        ptr::copy_nonoverlapping(v.as_ptr(), p, alen);
        *list.add(i) = p;
        p = p.add(alen);
    }
    *list.add(addrs.len()) = ptr::null_mut();
    ptr::copy_nonoverlapping(canon.as_ptr(), p, canon.len());
    *p.add(canon.len()) = 0;
    (*h).name = p;
    (*h).aliases = aliases;
    (*h).addrtype = af;
    (*h).length = alen as c_int;
    (*h).addr_list = list;
    *r = h;
    if !herr.is_null() {
        *herr = 0;
    }
    0
}
const NETDB_INTERNAL: c_int = -1;

#[no_mangle]
pub unsafe extern "C" fn gethostbyname2_r(name: *const c_char, af: c_int, h: *mut Hostent, buf: *mut c_char, len: size_t, r: *mut *mut Hostent, herr: *mut c_int) -> c_int {
    if af != AF_INET && af != AF_INET6 {
        *r = ptr::null_mut();
        return 97;
    }
    host_lookup(name, af, h, buf, len, r, herr)
}
#[no_mangle]
pub unsafe extern "C" fn gethostbyname_r(name: *const c_char, h: *mut Hostent, buf: *mut c_char, len: size_t, r: *mut *mut Hostent, herr: *mut c_int) -> c_int {
    host_lookup(name, AF_INET, h, buf, len, r, herr)
}

static mut HOST: Hostent = Hostent { name: ptr::null_mut(), aliases: ptr::null_mut(), addrtype: 0, length: 0, addr_list: ptr::null_mut() };
static mut HOST_BUF: [u8; 2048] = [0; 2048];

#[no_mangle]
pub unsafe extern "C" fn gethostbyname2(name: *const c_char, af: c_int) -> *mut Hostent {
    let mut r = ptr::null_mut();
    let mut he = 0;
    gethostbyname2_r(name, af, ptr::addr_of_mut!(HOST), ptr::addr_of_mut!(HOST_BUF) as *mut c_char, 2048, &mut r, &mut he);
    set_h(he);
    r
}
#[no_mangle]
pub unsafe extern "C" fn gethostbyname(name: *const c_char) -> *mut Hostent {
    gethostbyname2(name, AF_INET)
}

/// The /etc/hosts name for an address, if any.
unsafe fn hosts_name(ip: Ip) -> Option<Vec<u8>> {
    let mut buf = alloc::vec![0u8; 1 << 16];
    let n = fl::resolve::read_file(b"/etc/hosts\0", &mut buf);
    for line in buf[..n].split(|&c| c == b'\n') {
        let line = line.split(|&c| c == b'#').next().unwrap_or(b"");
        let mut w = line.split(|c| c.is_ascii_whitespace()).filter(|w| !w.is_empty());
        let (Some(a), Some(name)) = (w.next(), w.next()) else { continue };
        match (parse_ip(a), ip) {
            (Some(Ip::V4(x)), Ip::V4(y)) if x == y => return Some(name.to_vec()),
            (Some(Ip::V6(x)), Ip::V6(y)) if x == y => return Some(name.to_vec()),
            _ => {}
        }
    }
    None
}

#[no_mangle]
pub unsafe extern "C" fn gethostbyaddr_r(addr: *const c_void, alen: u32, af: c_int, h: *mut Hostent, buf: *mut c_char, len: size_t, r: *mut *mut Hostent, herr: *mut c_int) -> c_int {
    *r = ptr::null_mut();
    let ip = match (af, alen) {
        (AF_INET, 4) => Ip::V4(*(addr as *const [u8; 4])),
        (AF_INET6, 16) => Ip::V6(*(addr as *const [u8; 16])),
        _ => {
            if !herr.is_null() {
                *herr = NETDB_INTERNAL;
            }
            return 97;
        }
    };
    let Some(name) = hosts_name(ip) else {
        if !herr.is_null() {
            *herr = HOST_NOT_FOUND;
        }
        set_h(HOST_NOT_FOUND);
        return 2;
    };
    let base = (buf as usize + 7) & !7;
    let need = base - buf as usize + 8 + 16 + alen as usize + name.len() + 1;
    if need > len {
        return 34;
    }
    let aliases = base as *mut *mut c_char;
    *aliases = ptr::null_mut();
    let list = aliases.add(1);
    let p = list.add(2) as *mut u8;
    ptr::copy_nonoverlapping(addr as *const u8, p, alen as usize);
    *list = p;
    *list.add(1) = ptr::null_mut();
    let np = p.add(alen as usize);
    ptr::copy_nonoverlapping(name.as_ptr(), np, name.len());
    *np.add(name.len()) = 0;
    (*h).name = np;
    (*h).aliases = aliases;
    (*h).addrtype = af;
    (*h).length = alen as c_int;
    (*h).addr_list = list;
    *r = h;
    if !herr.is_null() {
        *herr = 0;
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn gethostbyaddr(addr: *const c_void, alen: u32, af: c_int) -> *mut Hostent {
    let mut r = ptr::null_mut();
    let mut he = 0;
    gethostbyaddr_r(addr, alen, af, ptr::addr_of_mut!(HOST), ptr::addr_of_mut!(HOST_BUF) as *mut c_char, 2048, &mut r, &mut he);
    set_h(he);
    r
}

// --- getnameinfo ------------------------------------------------------------------------------------

const NI_NUMERICHOST: c_int = 1;
const NI_NUMERICSERV: c_int = 2;
const NI_NAMEREQD: c_int = 8;
const NI_DGRAM: c_int = 16;

#[no_mangle]
pub unsafe extern "C" fn getnameinfo(sa: *const u8, salen: u32, host: *mut c_char, hostlen: u32, serv: *mut c_char, servlen: u32, flags: c_int) -> c_int {
    const EAI_NONAME: c_int = -2;
    const EAI_FAMILY: c_int = -6;
    const EAI_OVERFLOW: c_int = -12;
    let fam = u16::from_ne_bytes([*sa, *sa.add(1)]) as c_int;
    let (ip, port, scope) = match fam {
        AF_INET if salen >= 16 => (Ip::V4(*(sa.add(4) as *const [u8; 4])), u16::from_be_bytes([*sa.add(2), *sa.add(3)]), 0u32),
        AF_INET6 if salen >= 28 => (
            Ip::V6(*(sa.add(8) as *const [u8; 16])),
            u16::from_be_bytes([*sa.add(2), *sa.add(3)]),
            u32::from_ne_bytes([*sa.add(24), *sa.add(25), *sa.add(26), *sa.add(27)]),
        ),
        _ => return EAI_FAMILY,
    };
    let copy = |dst: *mut c_char, cap: u32, s: &[u8]| -> bool {
        if s.len() + 1 > cap as usize {
            return false;
        }
        ptr::copy_nonoverlapping(s.as_ptr(), dst, s.len());
        *dst.add(s.len()) = 0;
        true
    };
    if !host.is_null() && hostlen > 0 {
        let mut name = if flags & NI_NUMERICHOST == 0 { hosts_name(ip) } else { None };
        if name.is_none() {
            if flags & NI_NAMEREQD != 0 {
                return EAI_NONAME;
            }
            let mut v = Vec::new();
            match ip {
                Ip::V4(a) => fmt_v4(a, &mut v),
                Ip::V6(a) => {
                    fmt_v6(a, &mut v);
                    if scope != 0 {
                        v.push(b'%');
                        let mut nb = [0u8; 16];
                        if !if_indextoname(scope, nb.as_mut_ptr()).is_null() {
                            v.extend_from_slice(fl::mem::cstr(nb.as_ptr()));
                        } else {
                            v.extend_from_slice(alloc::format!("{}", scope).as_bytes());
                        }
                    }
                }
            }
            name = Some(v);
        }
        if !copy(host, hostlen, name.as_deref().unwrap_or(b"")) {
            return EAI_OVERFLOW;
        }
    }
    if !serv.is_null() && servlen > 0 {
        let mut s: Option<Vec<u8>> = None;
        if flags & NI_NUMERICSERV == 0 {
            let proto: &[u8] = if flags & NI_DGRAM != 0 { b"udp" } else { b"tcp" };
            s = services().into_iter().find(|e| e.1 == port && e.2 == proto).map(|e| e.0);
        }
        let s = s.unwrap_or_else(|| alloc::format!("{}", port).into_bytes());
        if !copy(serv, servlen, &s) {
            return EAI_OVERFLOW;
        }
    }
    0
}

// --- the resolver interface -----------------------------------------------------------------------

/// glibc's struct __res_state is 568 bytes; programs read a few fields (options, nscount,
/// nsaddr_list) and pass it back.
#[repr(C, align(8))]
pub struct ResState([u8; 568]);

static mut RES: ResState = ResState([0; 568]);

#[no_mangle]
pub unsafe extern "C" fn __res_state() -> *mut ResState {
    ptr::addr_of_mut!(RES)
}

#[no_mangle]
pub unsafe extern "C" fn __res_ninit(st: *mut ResState) -> c_int {
    let b = &mut (*st).0;
    b.fill(0);
    b[0..4].copy_from_slice(&5i32.to_ne_bytes()); // retrans
    b[4..8].copy_from_slice(&2i32.to_ne_bytes()); // retry
    // RES_INIT | RES_RECURSE | RES_DEFNAMES | RES_DNSRCH
    b[8..16].copy_from_slice(&0x2c1u64.to_ne_bytes());
    // nameservers: up to three IPv4 sockaddr_ins at 20
    let mut conf = [0u8; 4096];
    let n = fl::resolve::read_file(b"/etc/resolv.conf\0", &mut conf);
    let mut count = 0;
    for line in conf[..n].split(|&c| c == b'\n') {
        let mut w = line.split(|c| c.is_ascii_whitespace()).filter(|w| !w.is_empty());
        if w.next() == Some(b"nameserver") {
            if let Some(Ip::V4(a)) = w.next().and_then(parse_ip) {
                if count < 3 {
                    let o = 20 + count * 16;
                    b[o..o + 2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
                    b[o + 2..o + 4].copy_from_slice(&53u16.to_be_bytes());
                    b[o + 4..o + 8].copy_from_slice(&a);
                    count += 1;
                }
            }
        }
    }
    if count == 0 {
        b[20..22].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
        b[22..24].copy_from_slice(&53u16.to_be_bytes());
        b[24..28].copy_from_slice(&[127, 0, 0, 1]);
        count = 1;
    }
    b[16..20].copy_from_slice(&(count as i32).to_ne_bytes());
    // ndots = 1 (the bitfield after pfcode at 392)
    b[392] = 1;
    0
}
#[no_mangle]
pub unsafe extern "C" fn __res_init() -> c_int {
    __res_ninit(__res_state())
}
#[no_mangle]
pub unsafe extern "C" fn __res_nclose(st: *mut ResState) {
    // RES_INIT off
    (*st).0[8] &= !1;
}
#[no_mangle]
pub unsafe extern "C" fn __res_close() {
    __res_nclose(__res_state())
}

/// Encode `name` and send a query; returns the answer length (or -1 with h_errno set).
unsafe fn query(name: &[u8], class: c_int, ty: c_int, ans: *mut u8, anslen: c_int) -> c_int {
    let mut q: Vec<u8> = Vec::with_capacity(300);
    let mut id = [0u8; 2];
    fl::sys::sc3(318 /* getrandom */, id.as_mut_ptr() as usize, 2, 0);
    q.extend_from_slice(&[id[0], id[1], 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split(|&c| c == b'.').filter(|l| !l.is_empty()) {
        if label.len() > 63 {
            set_h(NO_RECOVERY);
            return -1;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label);
    }
    q.push(0);
    q.extend_from_slice(&(ty as u16).to_be_bytes());
    q.extend_from_slice(&(class as u16).to_be_bytes());
    if q.len() > 512 || anslen < 12 {
        set_h(NO_RECOVERY);
        return -1;
    }
    let out = core::slice::from_raw_parts_mut(ans, anslen as usize);
    let n = fl::resolve::exchange(&q, out);
    if n < 12 {
        set_h(TRY_AGAIN);
        return -1;
    }
    let rcode = out[3] & 0xf;
    let ancount = u16::from_be_bytes([out[6], out[7]]);
    match rcode {
        0 if ancount > 0 => n as c_int,
        0 => {
            set_h(NO_DATA);
            -1
        }
        3 => {
            set_h(HOST_NOT_FOUND);
            -1
        }
        2 => {
            set_h(TRY_AGAIN);
            -1
        }
        _ => {
            set_h(NO_RECOVERY);
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn __res_nquery(_st: *mut ResState, name: *const c_char, class: c_int, ty: c_int, ans: *mut u8, anslen: c_int) -> c_int {
    query(fl::mem::cstr(name), class, ty, ans, anslen)
}
#[no_mangle]
pub unsafe extern "C" fn __res_nsearch(st: *mut ResState, name: *const c_char, class: c_int, ty: c_int, ans: *mut u8, anslen: c_int) -> c_int {
    let n = fl::mem::cstr(name);
    let r = query(n, class, ty, ans, anslen);
    if r >= 0 || n.contains(&b'.') {
        return r;
    }
    // a single label: try the search domains from resolv.conf
    let mut conf = [0u8; 4096];
    let len = fl::resolve::read_file(b"/etc/resolv.conf\0", &mut conf);
    for line in conf[..len].split(|&c| c == b'\n') {
        let mut w = line.split(|c| c.is_ascii_whitespace()).filter(|w| !w.is_empty());
        let k = w.next();
        if k == Some(b"search") || k == Some(b"domain") {
            for d in w {
                let mut full = n.to_vec();
                full.push(b'.');
                full.extend_from_slice(d);
                let r = query(&full, class, ty, ans, anslen);
                if r >= 0 {
                    return r;
                }
            }
        }
    }
    let _ = st;
    r
}
#[no_mangle]
pub unsafe extern "C" fn __res_query(name: *const c_char, class: c_int, ty: c_int, ans: *mut u8, anslen: c_int) -> c_int {
    __res_nquery(__res_state(), name, class, ty, ans, anslen)
}
#[no_mangle]
pub unsafe extern "C" fn __res_search(name: *const c_char, class: c_int, ty: c_int, ans: *mut u8, anslen: c_int) -> c_int {
    __res_nsearch(__res_state(), name, class, ty, ans, anslen)
}

// --- DNS message parsing (arpa/nameser.h) -----------------------------------------------------------

/// Expand the (possibly compressed) name at `src` into dotted text. Returns the bytes the
/// name occupies at `src`, or -1.
unsafe fn expand(msg: *const u8, eom: *const u8, src: *const u8, dst: *mut c_char, dstsiz: c_int) -> c_int {
    let mut out: Vec<u8> = Vec::new();
    let mut p = src;
    let mut used: isize = -1;
    let mut hops = 0;
    loop {
        if p < msg || p >= eom {
            return -1;
        }
        let l = *p as usize;
        if l & 0xc0 == 0xc0 {
            if p.add(1) >= eom {
                return -1;
            }
            if used < 0 {
                used = p.offset_from(src) + 2;
            }
            let off = ((l & 0x3f) << 8) | *p.add(1) as usize;
            p = msg.add(off);
            hops += 1;
            if hops > 64 {
                return -1;
            }
            continue;
        }
        if l & 0xc0 != 0 {
            return -1;
        }
        if l == 0 {
            if used < 0 {
                used = p.offset_from(src) + 1;
            }
            break;
        }
        if p.add(1 + l) > eom {
            return -1;
        }
        if !out.is_empty() {
            out.push(b'.');
        }
        for k in 0..l {
            let c = *p.add(1 + k);
            match c {
                b'.' | b'\\' | b'"' | b';' | b'(' | b')' | b'@' | b'$' => {
                    out.push(b'\\');
                    out.push(c);
                }
                0x21..=0x7e => out.push(c),
                _ => {
                    out.push(b'\\');
                    out.push(b'0' + c / 100);
                    out.push(b'0' + c / 10 % 10);
                    out.push(b'0' + c % 10);
                }
            }
        }
        p = p.add(1 + l);
    }
    if out.is_empty() {
        out.push(b'.');
    }
    if out.len() + 1 > dstsiz as usize {
        fl::errno::set(90); // EMSGSIZE
        return -1;
    }
    ptr::copy_nonoverlapping(out.as_ptr(), dst, out.len());
    *dst.add(out.len()) = 0;
    used as c_int
}

#[no_mangle]
pub unsafe extern "C" fn __dn_expand(msg: *const u8, eom: *const u8, src: *const u8, dst: *mut c_char, dstsiz: c_int) -> c_int {
    let r = expand(msg, eom, src, dst, dstsiz);
    // dn_expand gives "" for the root
    if r > 0 && *dst == b'.' && *dst.add(1) == 0 {
        *dst = 0;
    }
    r
}
#[no_mangle]
pub unsafe extern "C" fn ns_name_uncompress(msg: *const u8, eom: *const u8, src: *const u8, dst: *mut c_char, dstsiz: size_t) -> c_int {
    expand(msg, eom, src, dst, dstsiz.min(i32::MAX as usize) as c_int)
}

/// Skip a name; the bytes it occupies.
unsafe fn skip_name(p: *const u8, eom: *const u8) -> Option<usize> {
    let mut q = p;
    loop {
        if q >= eom {
            return None;
        }
        let l = *q as usize;
        if l & 0xc0 == 0xc0 {
            return if q.add(2) <= eom { Some(q.offset_from(p) as usize + 2) } else { None };
        }
        if l == 0 {
            return Some(q.offset_from(p) as usize + 1);
        }
        q = q.add(1 + l);
    }
}

/// ns_msg: the handle macros like ns_msg_count read its fields.
#[repr(C)]
pub struct NsMsg {
    msg: *const u8,
    eom: *const u8,
    id: u16,
    flags: u16,
    counts: [u16; 4],
    sections: [*const u8; 4],
    sect: c_int,
    rrnum: c_int,
    msg_ptr: *const u8,
}

#[repr(C)]
pub struct NsRr {
    name: [u8; 1025],
    ty: u16,
    class: u16,
    ttl: u32,
    rdlength: u16,
    rdata: *const u8,
}

/// Skip `count` records of section `sect` from p; the new position.
unsafe fn skip_rrs(mut p: *const u8, eom: *const u8, sect: usize, count: usize) -> Option<*const u8> {
    for _ in 0..count {
        p = p.add(skip_name(p, eom)?);
        if sect == 0 {
            p = p.add(4);
        } else {
            if p.add(10) > eom {
                return None;
            }
            let rdlen = u16::from_be_bytes([*p.add(8), *p.add(9)]) as usize;
            p = p.add(10 + rdlen);
        }
        if p > eom {
            return None;
        }
    }
    Some(p)
}

#[no_mangle]
pub unsafe extern "C" fn ns_initparse(msg: *const u8, len: c_int, h: *mut NsMsg) -> c_int {
    let eom = msg.add(len.max(0) as usize);
    if len < 12 {
        fl::errno::set(90);
        return -1;
    }
    (*h).msg = msg;
    (*h).eom = eom;
    (*h).id = u16::from_be_bytes([*msg, *msg.add(1)]);
    (*h).flags = u16::from_be_bytes([*msg.add(2), *msg.add(3)]);
    let mut p = msg.add(12);
    for s in 0..4 {
        (*h).counts[s] = u16::from_be_bytes([*msg.add(4 + 2 * s), *msg.add(5 + 2 * s)]);
    }
    for s in 0..4 {
        let c = (*h).counts[s] as usize;
        if c == 0 {
            (*h).sections[s] = ptr::null();
            continue;
        }
        (*h).sections[s] = p;
        match skip_rrs(p, eom, s, c) {
            Some(q) => p = q,
            None => {
                fl::errno::set(90);
                return -1;
            }
        }
    }
    (*h).sect = 4; // ns_s_max
    (*h).rrnum = -1;
    (*h).msg_ptr = ptr::null();
    0
}

#[no_mangle]
pub unsafe extern "C" fn ns_parserr(h: *mut NsMsg, sect: c_int, rrnum: c_int, rr: *mut NsRr) -> c_int {
    if !(0..4).contains(&sect) {
        fl::errno::set(19); // ENODEV
        return -1;
    }
    let s = sect as usize;
    if rrnum < 0 || rrnum >= (*h).counts[s] as c_int {
        fl::errno::set(19);
        return -1;
    }
    if (*h).sect != sect || rrnum < (*h).rrnum || (*h).rrnum < 0 {
        (*h).sect = sect;
        (*h).rrnum = 0;
        (*h).msg_ptr = (*h).sections[s];
    }
    if rrnum > (*h).rrnum {
        match skip_rrs((*h).msg_ptr, (*h).eom, s, (rrnum - (*h).rrnum) as usize) {
            Some(p) => (*h).msg_ptr = p,
            None => {
                fl::errno::set(90);
                return -1;
            }
        }
        (*h).rrnum = rrnum;
    }
    let p = (*h).msg_ptr;
    let n = expand((*h).msg, (*h).eom, p, (*rr).name.as_mut_ptr(), 1025);
    if n < 0 {
        return -1;
    }
    let mut q = p.add(n as usize);
    if q.add(4) > (*h).eom {
        fl::errno::set(90);
        return -1;
    }
    (*rr).ty = u16::from_be_bytes([*q, *q.add(1)]);
    (*rr).class = u16::from_be_bytes([*q.add(2), *q.add(3)]);
    q = q.add(4);
    if s == 0 {
        (*rr).ttl = 0;
        (*rr).rdlength = 0;
        (*rr).rdata = ptr::null();
    } else {
        if q.add(6) > (*h).eom {
            fl::errno::set(90);
            return -1;
        }
        (*rr).ttl = u32::from_be_bytes([*q, *q.add(1), *q.add(2), *q.add(3)]);
        (*rr).rdlength = u16::from_be_bytes([*q.add(4), *q.add(5)]);
        q = q.add(6);
        if q.add((*rr).rdlength as usize) > (*h).eom {
            fl::errno::set(90);
            return -1;
        }
        (*rr).rdata = q;
        q = q.add((*rr).rdlength as usize);
    }
    (*h).rrnum += 1;
    (*h).msg_ptr = q;
    0
}

#[no_mangle]
pub unsafe extern "C" fn ns_skiprr(p: *const u8, eom: *const u8, sect: c_int, count: c_int) -> c_int {
    match skip_rrs(p, eom, sect.clamp(0, 3) as usize, count.max(0) as usize) {
        Some(q) => q.offset_from(p) as c_int,
        None => {
            fl::errno::set(90);
            -1
        }
    }
}
#[no_mangle]
pub unsafe extern "C" fn __dn_skipname(p: *const u8, eom: *const u8) -> c_int {
    skip_name(p, eom).map_or(-1, |n| n as c_int)
}
#[no_mangle]
pub unsafe extern "C" fn ns_get16(p: *const u8) -> u32 {
    u16::from_be_bytes([*p, *p.add(1)]) as u32
}
#[no_mangle]
pub unsafe extern "C" fn ns_get32(p: *const u8) -> u64 {
    u32::from_be_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]) as u64
}

/// Build a query message (RES_NMKQUERY). Only QUERY (op 0) is made; data adds no record.
#[no_mangle]
pub unsafe extern "C" fn __res_nmkquery(
    _st: *mut ResState,
    op: c_int,
    dname: *const c_char,
    class: c_int,
    ty: c_int,
    _data: *const u8,
    _datalen: c_int,
    _newrr: *const u8,
    buf: *mut u8,
    buflen: c_int,
) -> c_int {
    if op != 0 || buf.is_null() {
        return -1;
    }
    let mut q: Vec<u8> = Vec::with_capacity(300);
    let mut id = [0u8; 2];
    fl::sys::sc3(318 /* getrandom */, id.as_mut_ptr() as usize, 2, 0);
    q.extend_from_slice(&[id[0], id[1], 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in fl::mem::cstr(dname).split(|&c| c == b'.').filter(|l| !l.is_empty()) {
        if label.len() > 63 {
            return -1;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label);
    }
    q.push(0);
    q.extend_from_slice(&(ty as u16).to_be_bytes());
    q.extend_from_slice(&(class as u16).to_be_bytes());
    if q.len() > buflen.max(0) as usize {
        return -1;
    }
    ptr::copy_nonoverlapping(q.as_ptr(), buf, q.len());
    q.len() as c_int
}

/// Send a prepared message to the configured nameservers and return the answer (RES_NSEND).
#[no_mangle]
pub unsafe extern "C" fn __res_nsend(_st: *mut ResState, msg: *const u8, msglen: c_int, ans: *mut u8, anslen: c_int) -> c_int {
    if msg.is_null() || msglen < 12 || ans.is_null() || anslen < 12 {
        return -1;
    }
    let q = core::slice::from_raw_parts(msg, msglen as usize);
    let out = core::slice::from_raw_parts_mut(ans, anslen as usize);
    let n = fl::resolve::exchange(q, out);
    if n < 12 {
        set_h(TRY_AGAIN);
        return -1;
    }
    n as c_int
}
#[no_mangle]
pub unsafe extern "C" fn __res_send(msg: *const u8, msglen: c_int, ans: *mut u8, anslen: c_int) -> c_int {
    __res_nsend(__res_state(), msg, msglen, ans, anslen)
}

/// Is `name` a valid host name (res_hnok) / domain name in a response (res_dnok)?
fn name_ok(s: &[u8], host: bool) -> bool {
    if s.len() > 255 {
        return false;
    }
    for label in s.split(|&c| c == b'.') {
        if label.len() > 63 {
            return false;
        }
        for (i, &c) in label.iter().enumerate() {
            let ok = c.is_ascii_alphanumeric()
                || c == b'_'
                || (c == b'-' && i > 0 && i + 1 < label.len())
                || (!host && c.is_ascii_graphic());
            if !ok {
                return false;
            }
        }
    }
    true
}
#[no_mangle]
pub unsafe extern "C" fn res_hnok(name: *const c_char) -> c_int {
    name_ok(fl::mem::cstr(name), true) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn res_dnok(name: *const c_char) -> c_int {
    name_ok(fl::mem::cstr(name), false) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn res_mailok(name: *const c_char) -> c_int {
    res_dnok(name)
}
#[no_mangle]
pub unsafe extern "C" fn res_ownok(name: *const c_char) -> c_int {
    res_hnok(name)
}

static mut NSAP: [u8; 64] = [0; 64];

#[no_mangle]
pub unsafe extern "C" fn inet_nsap_ntoa(len: c_int, src: *const u8, dst: *mut c_char) -> *mut c_char {
    let out = if dst.is_null() { ptr::addr_of_mut!(NSAP) as *mut c_char } else { dst };
    let n = len.clamp(0, 20) as usize;
    let mut p = out;
    *p = b'0';
    p = p.add(1);
    *p = b'x';
    p = p.add(1);
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for i in 0..n {
        let b = *src.add(i);
        *p = HEX[(b >> 4) as usize];
        p = p.add(1);
        *p = HEX[(b & 15) as usize];
        p = p.add(1);
    }
    *p = 0;
    out
}
