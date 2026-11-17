//! The socket system calls: AF_INET (TCP, UDP, ICMP "ping" sockets, raw IPv4) on top of
//! `net`, and AF_UNIX (stream, datagram and seqpacket; socketpair, and bind/listen/connect
//! for stream sockets by path or abstract name) kept entirely in the kernel.

use crate::net::{self, Proto, St};
use crate::proc::{self, user_slice};
use crate::syscall::{fd_file, fd_install, iovecs, now_ns, nonblock, Kind, OpenFile};
use crate::syscall::{EAFNOSUPPORT, EAGAIN, EBADF, EINVAL, ENOENT, EPIPE};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

const ENOTSOCK: i64 = 88;
const EMSGSIZE: i64 = 90;
const EPROTONOSUPPORT: i64 = 93;
const EOPNOTSUPP: i64 = 95;
const EADDRINUSE: i64 = 98;
const EADDRNOTAVAIL: i64 = 99;
const EISCONN: i64 = 106;
const ENOTCONN: i64 = 107;
const EALREADY: i64 = 114;
const EINPROGRESS: i64 = 115;
const ECONNREFUSED: i64 = 111;

const SOCK_NONBLOCK: u64 = 0o4000;
const SOCK_CLOEXEC: u64 = 0o2000000;
const MSG_PEEK: u64 = 2;
const MSG_TRUNC: u64 = 0x20;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_WAITALL: u64 = 0x100;
const MSG_NOSIGNAL: u64 = 0x4000;

// ---------------------------------------------------------------------------------------
// AF_UNIX

const UNIX_CAP: usize = 256 * 1024;

pub struct Chan {
    bytes: VecDeque<u8>,
    msgs: VecDeque<Vec<u8>>,
    reader_gone: bool,
    writer_gone: bool,
}

impl Chan {
    fn new() -> Rc<RefCell<Chan>> {
        Rc::new(RefCell::new(Chan { bytes: VecDeque::new(), msgs: VecDeque::new(), reader_gone: false, writer_gone: false }))
    }
    fn len(&self) -> usize {
        self.bytes.len() + self.msgs.iter().map(|m| m.len()).sum::<usize>()
    }
    fn empty(&self) -> bool {
        self.bytes.is_empty() && self.msgs.is_empty()
    }
}

pub enum UState {
    New,
    Listening(VecDeque<Rc<RefCell<Unix>>>, usize),
    Connected { rx: Rc<RefCell<Chan>>, tx: Rc<RefCell<Chan>> },
}

pub struct Unix {
    ty: u64,
    pub state: UState,
    path: Option<String>,
    peer_path: Option<String>,
    rcvtimeo: u64,
    sndtimeo: u64,
    shut_wr: bool,
}

impl Unix {
    fn new(ty: u64) -> Unix {
        Unix { ty, state: UState::New, path: None, peer_path: None, rcvtimeo: 0, sndtimeo: 0, shut_wr: false }
    }
    fn messages(&self) -> bool {
        self.ty != 1
    }
}

impl Drop for Unix {
    fn drop(&mut self) {
        match &self.state {
            UState::Connected { rx, tx } => {
                rx.borrow_mut().reader_gone = true;
                tx.borrow_mut().writer_gone = true;
            }
            UState::Listening(..) => {
                if let Some(p) = &self.path {
                    registry().remove(p);
                }
            }
            UState::New => {}
        }
        proc::wake_all();
    }
}

static mut REGISTRY: Option<BTreeMap<String, Weak<RefCell<Unix>>>> = None;

fn registry() -> &'static mut BTreeMap<String, Weak<RefCell<Unix>>> {
    // SAFETY: one CPU, only reached from system calls.
    unsafe { (*core::ptr::addr_of_mut!(REGISTRY)).get_or_insert_with(BTreeMap::new) }
}

fn pair(ty: u64) -> (Unix, Unix) {
    let (a, b) = (Chan::new(), Chan::new());
    let mut x = Unix::new(ty);
    let mut y = Unix::new(ty);
    x.state = UState::Connected { rx: a.clone(), tx: b.clone() };
    y.state = UState::Connected { rx: b, tx: a };
    (x, y)
}

/// The key a unix address is registered under: the canonical path, or "@name" for the
/// abstract namespace.
fn unix_key(addr: u64, len: u64) -> Result<String, i64> {
    if len < 3 {
        return Err(EINVAL);
    }
    let b = user_slice(addr, len.min(110))?;
    if u16::from_le_bytes([b[0], b[1]]) != 1 {
        return Err(EAFNOSUPPORT);
    }
    let raw = &b[2..];
    if raw[0] == 0 {
        let name = &raw[1..];
        return Ok(alloc::format!("@{}", String::from_utf8_lossy(name)));
    }
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let path = String::from_utf8_lossy(&raw[..end]).into_owned();
    let cwd = proc::current().cwd.borrow().clone();
    crate::vfs::resolve(&cwd, &path, true).or_else(|_| crate::vfs::resolve_parent(&cwd, &path))
}

fn put_unix_addr(addr: u64, lenp: u64, key: Option<&str>) -> Result<(), i64> {
    if addr == 0 || lenp == 0 {
        return Ok(());
    }
    let mut sa = alloc::vec![1u8, 0];
    if let Some(k) = key {
        if let Some(name) = k.strip_prefix('@') {
            sa.push(0);
            sa.extend_from_slice(name.as_bytes());
        } else {
            sa.push(b'/');
            sa.extend_from_slice(k.as_bytes());
            sa.push(0);
        }
    }
    put_addr_bytes(addr, lenp, &sa)
}

fn put_addr_bytes(addr: u64, lenp: u64, sa: &[u8]) -> Result<(), i64> {
    let lb = user_slice(lenp, 4)?;
    let room = u32::from_le_bytes(lb[..4].try_into().unwrap()) as usize;
    let n = room.min(sa.len());
    user_slice(addr, n as u64)?.copy_from_slice(&sa[..n]);
    user_slice(lenp, 4)?.copy_from_slice(&(sa.len() as u32).to_le_bytes());
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Addresses

fn read_in(addr: u64, len: u64) -> Result<(u32, u16), i64> {
    if len < 8 {
        return Err(EINVAL);
    }
    let b = user_slice(addr, 8)?;
    let fam = u16::from_le_bytes([b[0], b[1]]);
    if fam != 2 {
        return Err(if fam == 0 { EAFNOSUPPORT } else { EAFNOSUPPORT });
    }
    Ok((u32::from_be_bytes([b[4], b[5], b[6], b[7]]), u16::from_be_bytes([b[2], b[3]])))
}

fn put_in(addr: u64, lenp: u64, ip: u32, port: u16) -> Result<(), i64> {
    if addr == 0 || lenp == 0 {
        return Ok(());
    }
    let mut sa = [0u8; 16];
    sa[0] = 2;
    sa[2..4].copy_from_slice(&port.to_be_bytes());
    sa[4..8].copy_from_slice(&ip.to_be_bytes());
    put_addr_bytes(addr, lenp, &sa)
}

// ---------------------------------------------------------------------------------------
// Helpers

enum S {
    Inet(usize),
    Unix(Rc<RefCell<Unix>>),
}

fn sock_of(fd: u64) -> Result<(Rc<RefCell<OpenFile>>, S), i64> {
    let file = fd_file(fd)?;
    let s = match &file.borrow().kind {
        Kind::Sock(i) => S::Inet(*i),
        Kind::Unix(u) => S::Unix(u.clone()),
        _ => return Err(ENOTSOCK),
    };
    Ok((file, s))
}

/// Wait for the next event, or say why not.
fn wait(file: &Rc<RefCell<OpenFile>>, flags: u64, deadline: u64) -> Result<(), i64> {
    if nonblock(file) || flags & MSG_DONTWAIT != 0 || now_ns() >= deadline {
        return Err(EAGAIN);
    }
    if proc::block() {
        return Err(proc::ERESTART);
    }
    Ok(())
}

fn deadline(timeo: u64) -> u64 {
    if timeo == 0 { u64::MAX } else { now_ns().saturating_add(timeo) }
}

fn sigpipe(flags: u64) -> i64 {
    if flags & MSG_NOSIGNAL == 0 {
        proc::signal(proc::pid(), 13);
    }
    EPIPE
}

fn install(kind: Kind, flags: u64) -> Result<i64, i64> {
    let fl = 2 | (flags & SOCK_NONBLOCK) as u32;
    fd_install(OpenFile { kind, pos: 0, flags: fl, path: "socket".into() }, flags & SOCK_CLOEXEC != 0)
}

// ---------------------------------------------------------------------------------------
// The calls

pub fn socket(domain: u64, ty: u64, proto: u64) -> Result<i64, i64> {
    let base = ty & 0xF;
    match domain {
        1 => {
            if !matches!(base, 1 | 2 | 5) {
                return Err(EPROTONOSUPPORT);
            }
            install(Kind::Unix(Rc::new(RefCell::new(Unix::new(base)))), ty)
        }
        2 => {
            let p = match (base, proto) {
                (1, 0 | 6) => Proto::Tcp,
                (2, 0 | 17) => Proto::Udp,
                (2, 1) => Proto::Icmp,
                (3, 1..=255) => Proto::Raw(proto as u8),
                _ => return Err(EPROTONOSUPPORT),
            };
            let i = net::with(|n| n.alloc(p));
            install(Kind::Sock(i), ty)
        }
        _ => Err(EAFNOSUPPORT),
    }
}

pub fn socketpair(domain: u64, ty: u64, _proto: u64, out: u64) -> Result<i64, i64> {
    if domain != 1 {
        return Err(EOPNOTSUPP);
    }
    let base = ty & 0xF;
    if !matches!(base, 1 | 2 | 5) {
        return Err(EPROTONOSUPPORT);
    }
    let (a, b) = pair(base);
    let x = install(Kind::Unix(Rc::new(RefCell::new(a))), ty)?;
    let y = install(Kind::Unix(Rc::new(RefCell::new(b))), ty)?;
    let s = user_slice(out, 8)?;
    s[0..4].copy_from_slice(&(x as i32).to_le_bytes());
    s[4..8].copy_from_slice(&(y as i32).to_le_bytes());
    Ok(0)
}

pub fn bind(fd: u64, addr: u64, len: u64) -> Result<i64, i64> {
    match sock_of(fd)?.1 {
        S::Inet(i) => {
            let (ip, port) = read_in(addr, len)?;
            net::with(|n| {
                if ip != 0 && !(ip >> 24 == 127) && ip != n.ip && ip != 0xFFFF_FFFF {
                    return Err(EADDRNOTAVAIL);
                }
                let s = n.sock(i);
                if s.lport != 0 {
                    return Err(EINVAL);
                }
                let proto = s.proto;
                let reuse = s.reuse;
                let port = if port == 0 && proto != Proto::Raw(0) { n.ephemeral(proto) } else { port };
                if !matches!(proto, Proto::Raw(_)) && !reuse && !n.port_free(proto, port) {
                    return Err(EADDRINUSE);
                }
                let s = n.sock(i);
                s.lip = ip;
                s.lport = port;
                Ok(0)
            })
        }
        S::Unix(u) => {
            let key = unix_key(addr, len)?;
            if registry().get(&key).is_some_and(|w| w.upgrade().is_some()) {
                return Err(EADDRINUSE);
            }
            if !key.starts_with('@') {
                if crate::vfs::lookup(&key).is_some() {
                    return Err(EADDRINUSE);
                }
                let m = crate::vfs::writable(&key, true, 0o777)?;
                m.borrow_mut().mode = 0o140000 | 0o755;
            }
            u.borrow_mut().path = Some(key.clone());
            registry().insert(key, Rc::downgrade(&u));
            Ok(0)
        }
    }
}

pub fn listen(fd: u64, backlog: u64) -> Result<i64, i64> {
    let max = (backlog as i32).clamp(1, 4096) as usize;
    match sock_of(fd)?.1 {
        S::Inet(i) => net::with(|n| {
            let s = n.sock(i);
            if s.proto != Proto::Tcp {
                return Err(EOPNOTSUPP);
            }
            if !matches!(s.tcp.st, St::Closed | St::Listen) {
                return Err(EINVAL);
            }
            if s.lport == 0 {
                let p = n.ephemeral(Proto::Tcp);
                n.sock(i).lport = p;
            }
            let s = n.sock(i);
            s.tcp.st = St::Listen;
            s.tcp.set_backlog(max);
            Ok(0)
        }),
        S::Unix(u) => {
            let mut u = u.borrow_mut();
            if u.ty == 2 {
                return Err(EOPNOTSUPP);
            }
            match &mut u.state {
                UState::New => {
                    if u.path.is_none() {
                        // autobind to an abstract name
                        let key = alloc::format!("@{:05x}", crate::syscall::random_u64() & 0xFFFFF);
                        u.path = Some(key);
                    }
                    u.state = UState::Listening(VecDeque::new(), max);
                    Ok(0)
                }
                UState::Listening(_, m) => {
                    *m = max;
                    Ok(0)
                }
                UState::Connected { .. } => Err(EINVAL),
            }
        }
    }
}

pub fn accept(fd: u64, addr: u64, lenp: u64, flags: u64) -> Result<i64, i64> {
    let (file, s) = sock_of(fd)?;
    match s {
        S::Inet(i) => loop {
            net::poll();
            let got = net::with(|n| {
                let s = n.sock(i);
                if s.tcp.st != St::Listen {
                    return Err(EINVAL);
                }
                while let Some(c) = n.sock(i).tcp.backlog.pop_front() {
                    if let Some(Some(cs)) = n.socks.get_mut(c) {
                        if cs.tcp.parent_is(i) && cs.orphan {
                            cs.orphan = false;
                            return Ok(Some((c, cs.rip, cs.rport)));
                        }
                    }
                }
                Ok(None)
            })?;
            if let Some((c, ip, port)) = got {
                let fdn = install(Kind::Sock(c), flags)?;
                put_in(addr, lenp, ip, port)?;
                return Ok(fdn);
            }
            wait(&file, 0, u64::MAX)?;
        },
        S::Unix(u) => loop {
            let next = match &mut u.borrow_mut().state {
                UState::Listening(q, _) => q.pop_front(),
                _ => return Err(EINVAL),
            };
            if let Some(c) = next {
                let peer = c.borrow().peer_path.clone();
                let fdn = fd_install(OpenFile { kind: Kind::Unix(c), pos: 0, flags: 2 | (flags & SOCK_NONBLOCK) as u32, path: "socket".into() }, flags & SOCK_CLOEXEC != 0)?;
                put_unix_addr(addr, lenp, peer.as_deref())?;
                return Ok(fdn);
            }
            let to = u.borrow().rcvtimeo;
            wait(&file, 0, deadline(to))?;
        },
    }
}

pub fn connect(fd: u64, addr: u64, len: u64) -> Result<i64, i64> {
    let (file, s) = sock_of(fd)?;
    match s {
        S::Inet(i) => {
            if len >= 2 && u16::from_le_bytes(user_slice(addr, 2)?.try_into().unwrap()) == 0 {
                // AF_UNSPEC: forget the peer (datagram sockets)
                net::with(|n| {
                    let s = n.sock(i);
                    if s.proto != Proto::Tcp {
                        s.connected = false;
                        s.rip = 0;
                        s.rport = 0;
                    }
                });
                return Ok(0);
            }
            let (ip, port) = read_in(addr, len)?;
            let ip = if ip == 0 { 0x7F00_0001 } else { ip };
            let start = net::with(|n| -> Result<bool, i64> {
                let Some(src) = n.src_for(ip) else { return Err(net::ENETUNREACH) };
                let proto = n.sock(i).proto;
                if n.sock(i).lport == 0 {
                    let p = if let Proto::Raw(_) = proto { 0 } else { n.ephemeral(proto) };
                    n.sock(i).lport = p;
                }
                let s = n.sock(i);
                if proto != Proto::Tcp {
                    if s.lip == 0 && ip != 0xFFFF_FFFF {
                        s.lip = src;
                    }
                    s.rip = ip;
                    s.rport = port;
                    s.connected = true;
                    return Ok(false);
                }
                match s.tcp.st {
                    St::Closed if s.err == 0 => {}
                    St::Closed => return Err(core::mem::replace(&mut s.err, 0)),
                    St::SynSent => return Err(EALREADY),
                    St::Listen => return Err(EINVAL),
                    _ => return Err(EISCONN),
                }
                if s.lip == 0 {
                    s.lip = src;
                }
                s.rip = ip;
                s.rport = port;
                n.tcp_connect(i);
                Ok(true)
            })?;
            if !start {
                return Ok(0);
            }
            if nonblock(&file) {
                net::poll();
                return Err(EINPROGRESS);
            }
            let until = deadline(net::with(|n| n.sock(i).sndtimeo));
            loop {
                net::poll();
                let r = net::with(|n| {
                    let s = n.sock(i);
                    match s.tcp.st {
                        St::SynSent => None,
                        St::Closed => Some(Err(match core::mem::replace(&mut s.err, 0) {
                            0 => ECONNREFUSED,
                            e => e,
                        })),
                        _ => Some(Ok(0)),
                    }
                });
                if let Some(r) = r {
                    return r;
                }
                match wait(&file, 0, until) {
                    Err(EAGAIN) => return Err(EINPROGRESS),
                    Err(e) => return Err(e),
                    Ok(()) => {}
                }
            }
        }
        S::Unix(u) => {
            let key = unix_key(addr, len)?;
            let Some(l) = registry().get(&key).and_then(|w| w.upgrade()) else {
                return Err(if key.starts_with('@') || crate::vfs::lookup(&key).is_some() { ECONNREFUSED } else { ENOENT });
            };
            let ty = u.borrow().ty;
            if !matches!(u.borrow().state, UState::New) {
                return Err(EISCONN);
            }
            if ty == 2 {
                return Err(EOPNOTSUPP);
            }
            loop {
                let mut lb = l.borrow_mut();
                let lpath = lb.path.clone();
                let UState::Listening(q, max) = &mut lb.state else { return Err(ECONNREFUSED) };
                if q.len() < *max {
                    let (mut mine, mut theirs) = pair(ty);
                    theirs.path = lpath.clone();
                    theirs.peer_path = u.borrow().path.clone();
                    q.push_back(Rc::new(RefCell::new(theirs)));
                    let mut ub = u.borrow_mut();
                    // move the connected state into our socket
                    ub.state = core::mem::replace(&mut mine.state, UState::New);
                    ub.peer_path = lpath;
                    drop(ub);
                    drop(lb);
                    drop(mine);
                    proc::wake_all();
                    return Ok(0);
                }
                drop(lb);
                wait(&file, 0, u64::MAX)?;
            }
        }
    }
}

pub fn shutdown(fd: u64, how: u64) -> Result<i64, i64> {
    match sock_of(fd)?.1 {
        S::Inet(i) => net::with(|n| {
            let s = n.sock(i);
            if s.proto == Proto::Tcp && matches!(s.tcp.st, St::Closed | St::Listen | St::SynSent) {
                return Err(ENOTCONN);
            }
            if how == 0 || how == 2 {
                s.shut_rd = true;
                s.tcp.rbuf.clear();
            }
            if (how == 1 || how == 2) && s.proto == Proto::Tcp {
                n.shutdown_write(i);
            }
            Ok(0)
        }),
        S::Unix(u) => {
            let mut u = u.borrow_mut();
            let UState::Connected { rx, tx } = &u.state else { return Err(ENOTCONN) };
            if how == 0 || how == 2 {
                rx.borrow_mut().reader_gone = true;
            }
            if how == 1 || how == 2 {
                tx.borrow_mut().writer_gone = true;
                u.shut_wr = true;
            }
            proc::wake_all();
            Ok(0)
        }
    }
}

pub fn getname(fd: u64, addr: u64, lenp: u64, peer: bool) -> Result<i64, i64> {
    match sock_of(fd)?.1 {
        S::Inet(i) => {
            let (ip, port) = net::with(|n| {
                let me = n.ip;
                let s = n.sock(i);
                if peer {
                    if !s.connected || (s.proto == Proto::Tcp && !matches!(s.tcp.st, St::Established | St::CloseWait | St::FinWait1 | St::FinWait2)) {
                        return Err(ENOTCONN);
                    }
                    Ok((s.rip, s.rport))
                } else {
                    let ip = if s.lip == 0 && s.connected { me } else { s.lip };
                    Ok((ip, s.lport))
                }
            })?;
            put_in(addr, lenp, ip, port)?;
            Ok(0)
        }
        S::Unix(u) => {
            let u = u.borrow();
            if peer {
                if !matches!(u.state, UState::Connected { .. }) {
                    return Err(ENOTCONN);
                }
                put_unix_addr(addr, lenp, u.peer_path.as_deref())?;
            } else {
                put_unix_addr(addr, lenp, u.path.as_deref())?;
            }
            Ok(0)
        }
    }
}

fn timeval(p: u64, len: u64) -> Result<u64, i64> {
    if len < 16 {
        return Err(EINVAL);
    }
    let b = user_slice(p, 16)?;
    let s = i64::from_le_bytes(b[0..8].try_into().unwrap()).max(0) as u64;
    let us = i64::from_le_bytes(b[8..16].try_into().unwrap()).max(0) as u64;
    Ok(s.saturating_mul(1_000_000_000).saturating_add(us * 1000))
}

pub fn setsockopt(fd: u64, level: u64, name: u64, val: u64, len: u64) -> Result<i64, i64> {
    let s = sock_of(fd)?.1;
    if level == 1 {
        match name {
            2 => {
                let on = len >= 4 && user_slice(val, 4)?.iter().any(|&b| b != 0);
                if let S::Inet(i) = s {
                    net::with(|n| n.sock(i).reuse = on);
                }
            }
            20 | 21 => {
                let t = timeval(val, len)?;
                match s {
                    S::Inet(i) => net::with(|n| {
                        let s = n.sock(i);
                        if name == 20 { s.rcvtimeo = t } else { s.sndtimeo = t }
                    }),
                    S::Unix(u) => {
                        let mut u = u.borrow_mut();
                        if name == 20 { u.rcvtimeo = t } else { u.sndtimeo = t }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(0)
}

pub fn getsockopt(fd: u64, level: u64, name: u64, val: u64, lenp: u64) -> Result<i64, i64> {
    let s = sock_of(fd)?.1;
    let int = |v: i32| -> Result<i64, i64> {
        put_addr_bytes(val, lenp, &v.to_le_bytes())?;
        Ok(0)
    };
    if level != 1 {
        return int(0);
    }
    match (name, &s) {
        (4, S::Inet(i)) => {
            let e = net::with(|n| core::mem::replace(&mut n.sock(*i).err, 0));
            int(e as i32)
        }
        (4, S::Unix(_)) => int(0),
        (3, S::Inet(i)) => int(match net::with(|n| n.sock(*i).proto) {
            Proto::Tcp => 1,
            Proto::Udp | Proto::Icmp => 2,
            Proto::Raw(_) => 3,
        }),
        (3, S::Unix(u)) => int(u.borrow().ty as i32),
        (39, S::Inet(_)) => int(2),
        (39, S::Unix(_)) => int(1),
        (30, S::Inet(i)) => int((net::with(|n| n.sock(*i).tcp.st) == St::Listen) as i32),
        (30, S::Unix(u)) => int(matches!(u.borrow().state, UState::Listening(..)) as i32),
        (7 | 8, _) => int(212992),
        (17, _) => {
            // SO_PEERCRED: everyone is root here
            let mut c = [0u8; 12];
            c[0..4].copy_from_slice(&(proc::pid() as i32).to_le_bytes());
            put_addr_bytes(val, lenp, &c)?;
            Ok(0)
        }
        _ => int(0),
    }
}

// ---------------------------------------------------------------------------------------
// Data

/// Send `data`; `to` is an address for datagram sockets.
pub fn send(fd: u64, data_p: u64, len: u64, flags: u64, to: u64, tolen: u64) -> Result<i64, i64> {
    let (file, s) = sock_of(fd)?;
    let data = user_slice(data_p, len)?;
    send_data(&file, s, data, flags, to, tolen)
}

fn send_data(file: &Rc<RefCell<OpenFile>>, s: S, data: &[u8], flags: u64, to: u64, tolen: u64) -> Result<i64, i64> {
    match s {
        S::Inet(i) => {
            let proto = net::with(|n| n.sock(i).proto);
            if proto != Proto::Tcp {
                let dest = if to != 0 { Some(read_in(to, tolen)?) } else { None };
                return net::with(|n| {
                    let (ip, port) = match dest {
                        Some(d) => d,
                        None => {
                            let s = n.sock(i);
                            if !s.connected {
                                return Err(net_err_dest(s));
                            }
                            if s.err != 0 {
                                return Err(core::mem::replace(&mut s.err, 0));
                            }
                            (s.rip, s.rport)
                        }
                    };
                    let ip = if ip == 0 { 0x7F00_0001 } else { ip };
                    let Some(src) = n.src_for(ip) else { return Err(net::ENETUNREACH) };
                    if data.len() > 65507 || (data.len() > 1472 && ip >> 24 != 127) {
                        return Err(EMSGSIZE);
                    }
                    if n.sock(i).lport == 0 && !matches!(proto, Proto::Raw(_)) {
                        let p = n.ephemeral(proto);
                        n.sock(i).lport = p;
                    }
                    let s = n.sock(i);
                    let from = if s.lip != 0 && ip >> 24 != 127 { s.lip } else { src };
                    match proto {
                        Proto::Udp => {
                            let lport = s.lport;
                            n.udp_send(from, lport, ip, port, data);
                        }
                        Proto::Icmp => {
                            if data.len() < 8 {
                                return Err(EINVAL);
                            }
                            let mut m = data.to_vec();
                            m[4..6].copy_from_slice(&s.lport.to_be_bytes());
                            m[2] = 0;
                            m[3] = 0;
                            let c = net::checksum_end(net::checksum_add(0, &m));
                            m[2..4].copy_from_slice(&c.to_be_bytes());
                            n.icmp_send(from, ip, &m);
                        }
                        Proto::Raw(p) => n.raw_send(p, from, ip, data),
                        Proto::Tcp => unreachable!(),
                    }
                    Ok(data.len() as i64)
                });
            }
            let until = deadline(net::with(|n| n.sock(i).sndtimeo));
            let mut done = 0;
            loop {
                net::poll();
                let r = net::with(|n| {
                    let s = n.sock(i);
                    if s.err != 0 {
                        return Some(Err(core::mem::replace(&mut s.err, 0)));
                    }
                    match s.tcp.st {
                        St::Established | St::CloseWait if !s.tcp.fin_wanted() => {}
                        St::SynSent | St::SynRcvd => return None,
                        St::Closed | St::Listen if !s.connected => return Some(Err(ENOTCONN)),
                        _ => return Some(Err(EPIPE)),
                    }
                    let room = n.send_room(i);
                    if room == 0 {
                        return None;
                    }
                    let k = room.min(data.len() - done);
                    n.queue_send(i, &data[done..done + k]);
                    done += k;
                    if done == data.len() { Some(Ok(done as i64)) } else { None }
                });
                match r {
                    Some(Err(EPIPE)) if done == 0 => return Err(sigpipe(flags)),
                    Some(Err(_)) if done > 0 => return Ok(done as i64),
                    Some(r) => return r,
                    None => {}
                }
                if let Err(e) = wait(file, flags, until) {
                    return if done > 0 { Ok(done as i64) } else { Err(e) };
                }
            }
        }
        S::Unix(u) => {
            let until = deadline(u.borrow().sndtimeo);
            let mut done = 0;
            loop {
                {
                    let ub = u.borrow();
                    let UState::Connected { tx, .. } = &ub.state else { return Err(ENOTCONN) };
                    let mut c = tx.borrow_mut();
                    if c.reader_gone || ub.shut_wr {
                        drop(c);
                        drop(ub);
                        return if done > 0 { Ok(done as i64) } else { Err(sigpipe(flags)) };
                    }
                    if ub.messages() {
                        if data.len() > UNIX_CAP {
                            return Err(EMSGSIZE);
                        }
                        if c.len() + data.len() <= UNIX_CAP || c.empty() {
                            c.msgs.push_back(data.to_vec());
                            drop(c);
                            drop(ub);
                            proc::wake_all();
                            return Ok(data.len() as i64);
                        }
                    } else {
                        let room = UNIX_CAP.saturating_sub(c.bytes.len());
                        if room > 0 {
                            let k = room.min(data.len() - done);
                            c.bytes.extend(&data[done..done + k]);
                            done += k;
                            drop(c);
                            drop(ub);
                            proc::wake_all();
                            if done == data.len() {
                                return Ok(done as i64);
                            }
                            continue;
                        }
                    }
                }
                if let Err(e) = wait(file, flags, until) {
                    return if done > 0 { Ok(done as i64) } else { Err(e) };
                }
            }
        }
    }
}

fn net_err_dest(s: &net::Sock) -> i64 {
    if s.proto == Proto::Udp { 89 } else { ENOTCONN } // EDESTADDRREQ
}

/// Receive into `out`. Returns (bytes, real size of the message, source address).
fn recv_data(file: &Rc<RefCell<OpenFile>>, s: &S, out: &mut [u8], flags: u64) -> Result<(usize, usize, Option<(u32, u16)>), i64> {
    match s {
        S::Inet(i) => {
            let i = *i;
            let until = deadline(net::with(|n| n.sock(i).rcvtimeo));
            let mut got = 0;
            loop {
                net::poll();
                let r = net::with(|n| {
                    let s = n.sock(i);
                    if s.proto != Proto::Tcp {
                        if let Some((ip, port, d)) = if flags & MSG_PEEK != 0 { s.dgrams.front().cloned() } else { s.dgrams.pop_front() } {
                            let k = d.len().min(out.len());
                            out[..k].copy_from_slice(&d[..k]);
                            return Some(Ok((k, d.len(), Some((ip, port)))));
                        }
                        if s.err != 0 {
                            return Some(Err(core::mem::replace(&mut s.err, 0)));
                        }
                        if s.shut_rd {
                            return Some(Ok((0, 0, None)));
                        }
                        return None;
                    }
                    let (rip, rport) = (s.rip, s.rport);
                    let t = &mut s.tcp;
                    if !t.rbuf.is_empty() {
                        let k = t.rbuf.len().min(out.len() - got);
                        for (o, b) in out[got..got + k].iter_mut().zip(t.rbuf.iter()) {
                            *o = *b;
                        }
                        if flags & MSG_PEEK == 0 {
                            t.rbuf.drain(..k);
                            n.window_update(i);
                        }
                        got += k;
                        if flags & MSG_WAITALL != 0 && got < out.len() && flags & MSG_PEEK == 0 {
                            return None;
                        }
                        return Some(Ok((got, got, Some((rip, rport)))));
                    }
                    if got > 0 && (t.rcv_fin || t.st == St::Closed) {
                        return Some(Ok((got, got, Some((rip, rport)))));
                    }
                    if s.err != 0 {
                        return Some(Err(core::mem::replace(&mut s.err, 0)));
                    }
                    let t = &s.tcp;
                    if t.rcv_fin || s.shut_rd || (t.st == St::Closed && s.connected) {
                        return Some(Ok((0, 0, None)));
                    }
                    if matches!(t.st, St::Closed | St::Listen) {
                        return Some(Err(ENOTCONN));
                    }
                    None
                });
                if let Some(r) = r {
                    return r;
                }
                if let Err(e) = wait(file, flags, until) {
                    return if got > 0 { Ok((got, got, None)) } else { Err(e) };
                }
            }
        }
        S::Unix(u) => {
            let until = deadline(u.borrow().rcvtimeo);
            loop {
                {
                    let ub = u.borrow();
                    let UState::Connected { rx, .. } = &ub.state else { return Err(ENOTCONN) };
                    let mut c = rx.borrow_mut();
                    if ub.messages() {
                        if let Some(m) = c.msgs.front() {
                            let k = m.len().min(out.len());
                            out[..k].copy_from_slice(&m[..k]);
                            let real = m.len();
                            if flags & MSG_PEEK == 0 {
                                c.msgs.pop_front();
                                drop(c);
                                drop(ub);
                                proc::wake_all();
                            }
                            return Ok((k, real, None));
                        }
                    } else if !c.bytes.is_empty() {
                        let k = c.bytes.len().min(out.len());
                        for (o, b) in out[..k].iter_mut().zip(c.bytes.iter()) {
                            *o = *b;
                        }
                        if flags & MSG_PEEK == 0 {
                            c.bytes.drain(..k);
                            drop(c);
                            drop(ub);
                            proc::wake_all();
                        }
                        return Ok((k, k, None));
                    }
                    if c.writer_gone || c.reader_gone {
                        return Ok((0, 0, None));
                    }
                }
                wait(file, flags, until)?;
            }
        }
    }
}

pub fn recvfrom(fd: u64, buf: u64, len: u64, flags: u64, addr: u64, lenp: u64) -> Result<i64, i64> {
    let (file, s) = sock_of(fd)?;
    let out = user_slice(buf, len)?;
    let (k, real, from) = recv_data(&file, &s, out, flags)?;
    if let (Some((ip, port)), S::Inet(_)) = (from, &s) {
        put_in(addr, lenp, ip, port)?;
    } else if addr != 0 && lenp != 0 {
        if let S::Unix(u) = &s {
            put_unix_addr(addr, lenp, u.borrow().peer_path.as_deref())?;
        }
    }
    Ok(if flags & MSG_TRUNC != 0 { real } else { k } as i64)
}

/// read(2) and write(2) on a socket.
pub fn read(file: &Rc<RefCell<OpenFile>>, buf: u64, len: u64) -> Option<Result<i64, i64>> {
    let s = match &file.borrow().kind {
        Kind::Sock(i) => S::Inet(*i),
        Kind::Unix(u) => S::Unix(u.clone()),
        _ => return None,
    };
    Some(user_slice(buf, len).and_then(|out| recv_data(file, &s, out, 0).map(|(k, _, _)| k as i64)))
}

pub fn write(file: &Rc<RefCell<OpenFile>>, buf: u64, len: u64) -> Option<Result<i64, i64>> {
    let s = match &file.borrow().kind {
        Kind::Sock(i) => S::Inet(*i),
        Kind::Unix(u) => S::Unix(u.clone()),
        _ => return None,
    };
    Some(user_slice(buf, len).and_then(|d| send_data(file, s, d, 0, 0, 0)))
}

pub fn sendmsg(fd: u64, msg: u64, flags: u64) -> Result<i64, i64> {
    let (file, s) = sock_of(fd)?;
    let h = user_slice(msg, 56)?;
    let g = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap());
    let (name, namelen, iov, iovlen) = (g(0), g(8) & 0xFFFF_FFFF, g(16), g(24));
    let mut data = Vec::new();
    for (b, l) in iovecs(iov, iovlen)? {
        data.extend_from_slice(user_slice(b, l)?);
    }
    send_data(&file, s, &data, flags, name, namelen)
}

pub fn recvmsg(fd: u64, msg: u64, flags: u64) -> Result<i64, i64> {
    let (file, s) = sock_of(fd)?;
    let h = user_slice(msg, 56)?;
    let g = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap());
    let (name, iov, iovlen) = (g(0), g(16), g(24));
    let vecs = iovecs(iov, iovlen)?;
    let total: u64 = vecs.iter().map(|v| v.1).sum();
    let mut buf = alloc::vec![0u8; total.min(1 << 20) as usize];
    let (k, real, from) = recv_data(&file, &s, &mut buf, flags)?;
    let mut at = 0;
    for (b, l) in vecs {
        if at >= k {
            break;
        }
        let n = (l as usize).min(k - at);
        user_slice(b, n as u64)?.copy_from_slice(&buf[at..at + n]);
        at += n;
    }
    let h = user_slice(msg, 56)?;
    if name != 0 {
        if let (Some((ip, port)), S::Inet(_)) = (from, &s) {
            put_in(name, msg + 8, ip, port)?;
        } else {
            h[8..12].copy_from_slice(&0u32.to_le_bytes());
        }
    }
    let h = user_slice(msg, 56)?;
    h[40..48].copy_from_slice(&0u64.to_le_bytes()); // no control messages
    let mut mflags = 0u32;
    if real > k {
        mflags |= MSG_TRUNC as u32;
    }
    h[48..52].copy_from_slice(&mflags.to_le_bytes());
    Ok(if flags & MSG_TRUNC != 0 { real } else { k } as i64)
}

/// poll(2): (readable, writable, hung up).
pub fn ready(kind: &Kind) -> Option<(bool, bool, bool)> {
    match kind {
        Kind::Sock(i) => Some(net::with(|n| {
            let s = n.sock(*i);
            if s.proto != Proto::Tcp {
                return (!s.dgrams.is_empty() || s.err != 0 || s.shut_rd, true, false);
            }
            let t = &s.tcp;
            match t.st {
                St::Listen => (!t.backlog.is_empty(), false, false),
                St::SynSent | St::SynRcvd => (false, false, false),
                St::Closed => (s.connected || s.err != 0, true, true),
                _ => {
                    let r = !t.rbuf.is_empty() || t.rcv_fin || s.err != 0 || s.shut_rd;
                    let w = matches!(t.st, St::Established | St::CloseWait) && n.send_room(*i) > 0;
                    let s = n.sock(*i);
                    (r, w, s.tcp.rcv_fin && s.tcp.fin_wanted())
                }
            }
        })),
        Kind::Unix(u) => {
            let u = u.borrow();
            Some(match &u.state {
                UState::New => (false, false, true),
                UState::Listening(q, _) => (!q.is_empty(), false, false),
                UState::Connected { rx, tx } => {
                    let (r, t) = (rx.borrow(), tx.borrow());
                    (!r.empty() || r.writer_gone || r.reader_gone, t.len() < UNIX_CAP || t.reader_gone, r.writer_gone && (t.reader_gone || u.shut_wr))
                }
            })
        }
        _ => None,
    }
}

/// FIONREAD.
pub fn pending(kind: &Kind) -> Option<usize> {
    match kind {
        Kind::Sock(i) => Some(net::with(|n| {
            let s = n.sock(*i);
            if s.proto == Proto::Tcp { s.tcp.rbuf.len() } else { s.dgrams.front().map(|d| d.2.len()).unwrap_or(0) }
        })),
        Kind::Unix(u) => Some(match &u.borrow().state {
            UState::Connected { rx, .. } => rx.borrow().len(),
            _ => 0,
        }),
        _ => None,
    }
}

pub fn closed(i: usize) {
    net::with(|n| n.close(i));
}

pub fn is_sock(kind: &Kind) -> bool {
    matches!(kind, Kind::Sock(_) | Kind::Unix(_))
}

#[allow(dead_code)]
fn unused() -> i64 {
    EBADF
}
