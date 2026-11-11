//! Netlink sockets: requests, dumps, multicast events and attributes.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

pub const NLMSG_ERROR: u16 = 2;
pub const NLMSG_DONE: u16 = 3;
pub const NLM_F_REQUEST: u16 = 1;
pub const NLM_F_MULTI: u16 = 2;
pub const NLM_F_ACK: u16 = 4;
pub const NLM_F_ROOT: u16 = 0x100;
pub const NLM_F_MATCH: u16 = 0x200;
pub const NLM_F_DUMP: u16 = NLM_F_ROOT | NLM_F_MATCH;
pub const NLM_F_REPLACE: u16 = 0x100;
pub const NLM_F_EXCL: u16 = 0x200;
pub const NLM_F_CREATE: u16 = 0x400;
pub const NLA_F_NESTED: u16 = 0x8000;
const HDR: usize = 16;

pub fn align4(n: usize) -> usize {
    (n + 3) & !3
}

#[derive(Debug, Clone)]
pub struct Msg {
    pub ty: u16,
    pub flags: u16,
    pub seq: u32,
    pub payload: Vec<u8>,
}

/// Split a buffer from recv into messages.
pub fn split(buf: &[u8]) -> Vec<Msg> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + HDR <= buf.len() {
        let len = u32::from_ne_bytes(buf[at..at + 4].try_into().unwrap()) as usize;
        if len < HDR || at + len > buf.len() {
            break;
        }
        out.push(Msg {
            ty: u16::from_ne_bytes(buf[at + 4..at + 6].try_into().unwrap()),
            flags: u16::from_ne_bytes(buf[at + 6..at + 8].try_into().unwrap()),
            seq: u32::from_ne_bytes(buf[at + 8..at + 12].try_into().unwrap()),
            payload: buf[at + HDR..at + len].to_vec(),
        });
        at += align4(len);
    }
    out
}

/// Attributes (type, value) in `buf`, with the nested flag stripped from the type.
pub fn attrs(buf: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= buf.len() {
        let len = u16::from_ne_bytes([buf[at], buf[at + 1]]) as usize;
        let ty = u16::from_ne_bytes([buf[at + 2], buf[at + 3]]) & 0x3FFF;
        if len < 4 || at + len > buf.len() {
            break;
        }
        out.push((ty, &buf[at + 4..at + len]));
        at += align4(len);
    }
    out
}

pub fn find<'a>(attrs: &[(u16, &'a [u8])], ty: u16) -> Option<&'a [u8]> {
    attrs.iter().find(|(t, _)| *t == ty).map(|(_, v)| *v)
}

pub fn get_u32(attrs: &[(u16, &[u8])], ty: u16) -> Option<u32> {
    find(attrs, ty).filter(|v| v.len() >= 4).map(|v| u32::from_ne_bytes(v[..4].try_into().unwrap()))
}

pub fn get_u16(attrs: &[(u16, &[u8])], ty: u16) -> Option<u16> {
    find(attrs, ty).filter(|v| v.len() >= 2).map(|v| u16::from_ne_bytes(v[..2].try_into().unwrap()))
}

pub fn get_u8(attrs: &[(u16, &[u8])], ty: u16) -> Option<u8> {
    find(attrs, ty).and_then(|v| v.first().copied())
}

pub fn get_str(attrs: &[(u16, &[u8])], ty: u16) -> Option<String> {
    find(attrs, ty).map(|v| String::from_utf8_lossy(v.split(|&b| b == 0).next().unwrap_or(v)).into_owned())
}

/// Builds a message payload: a fixed header followed by attributes.
#[derive(Default, Clone)]
pub struct Builder {
    pub buf: Vec<u8>,
    nests: Vec<usize>,
}

impl Builder {
    pub fn new(fixed: &[u8]) -> Builder {
        Builder { buf: fixed.to_vec(), nests: Vec::new() }
    }

    pub fn put(&mut self, ty: u16, data: &[u8]) -> &mut Self {
        let len = 4 + data.len();
        self.buf.extend_from_slice(&(len as u16).to_ne_bytes());
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(data);
        self.buf.resize(align4(self.buf.len()), 0);
        self
    }

    pub fn put_u8(&mut self, ty: u16, v: u8) -> &mut Self {
        self.put(ty, &[v])
    }

    pub fn put_u16(&mut self, ty: u16, v: u16) -> &mut Self {
        self.put(ty, &v.to_ne_bytes())
    }

    pub fn put_u32(&mut self, ty: u16, v: u32) -> &mut Self {
        self.put(ty, &v.to_ne_bytes())
    }

    pub fn put_flag(&mut self, ty: u16) -> &mut Self {
        self.put(ty, &[])
    }

    pub fn put_str(&mut self, ty: u16, s: &str) -> &mut Self {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        self.put(ty, &v)
    }

    pub fn begin(&mut self, ty: u16) -> &mut Self {
        self.nests.push(self.buf.len());
        self.buf.extend_from_slice(&[0, 0]);
        self.buf.extend_from_slice(&(ty | NLA_F_NESTED).to_ne_bytes());
        self
    }

    pub fn end(&mut self) -> &mut Self {
        let start = self.nests.pop().expect("end without begin");
        let len = (self.buf.len() - start) as u16;
        self.buf[start..start + 2].copy_from_slice(&len.to_ne_bytes());
        self
    }
}

pub struct Socket {
    fd: OwnedFd,
    seq: u32,
}

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

impl Socket {
    pub fn open(protocol: i32) -> io::Result<Socket> {
        // SAFETY: plain socket/bind calls with a zeroed sockaddr_nl.
        unsafe {
            let fd = cvt(libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, protocol))?;
            let fd = OwnedFd::from_raw_fd(fd);
            let mut sa: libc::sockaddr_nl = mem::zeroed();
            sa.nl_family = libc::AF_NETLINK as u16;
            cvt(libc::bind(fd.as_raw_fd(), (&sa as *const libc::sockaddr_nl).cast(), mem::size_of::<libc::sockaddr_nl>() as u32))?;
            // Room for big dumps (scan results) and bursts of events.
            let size: libc::c_int = 1 << 20;
            libc::setsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, (&size as *const libc::c_int).cast(), 4);
            // Extended ACKs carry readable error messages; harmless if unsupported.
            let one: libc::c_int = 1;
            libc::setsockopt(fd.as_raw_fd(), libc::SOL_NETLINK, 11 /* NETLINK_EXT_ACK */, (&one as *const libc::c_int).cast(), 4);
            Ok(Socket { fd, seq: std::process::id().wrapping_mul(7919) })
        }
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    pub fn join(&self, group: u32) -> io::Result<()> {
        // SAFETY: setsockopt with a u32 value.
        cvt(unsafe { libc::setsockopt(self.fd(), libc::SOL_NETLINK, libc::NETLINK_ADD_MEMBERSHIP, (&group as *const u32).cast(), 4) }).map(|_| ())
    }

    pub fn send(&mut self, ty: u16, flags: u16, payload: &[u8]) -> io::Result<u32> {
        self.seq = self.seq.wrapping_add(1);
        let len = HDR + payload.len();
        let mut buf = Vec::with_capacity(align4(len));
        buf.extend_from_slice(&(len as u32).to_ne_bytes());
        buf.extend_from_slice(&ty.to_ne_bytes());
        buf.extend_from_slice(&flags.to_ne_bytes());
        buf.extend_from_slice(&self.seq.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes());
        buf.extend_from_slice(payload);
        // SAFETY: sending our own buffer to the kernel (zeroed sockaddr = the kernel).
        unsafe {
            let mut sa: libc::sockaddr_nl = mem::zeroed();
            sa.nl_family = libc::AF_NETLINK as u16;
            cvt(libc::sendto(self.fd(), buf.as_ptr().cast(), buf.len(), 0, (&sa as *const libc::sockaddr_nl).cast(), mem::size_of::<libc::sockaddr_nl>() as u32) as i32)?;
        }
        Ok(self.seq)
    }

    /// Wait up to `timeout` (None = forever) for data; then read one datagram's messages.
    pub fn recv(&self, timeout: Option<Duration>) -> io::Result<Vec<Msg>> {
        if !wait_readable(self.fd(), timeout)? {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut buf = vec![0u8; 1 << 16];
        loop {
            // Peek first to learn the datagram's size: big dumps can exceed the buffer.
            // SAFETY: reading into our own buffer.
            let size = unsafe { libc::recv(self.fd(), buf.as_mut_ptr().cast(), buf.len(), libc::MSG_PEEK | libc::MSG_TRUNC) };
            if size < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if size as usize > buf.len() {
                buf.resize(size as usize, 0);
            }
            // SAFETY: as above.
            let n = unsafe { libc::recv(self.fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            return Ok(split(&buf[..n as usize]));
        }
    }

    /// Send a request and collect the replies to it: Ok(messages) when acknowledged or the
    /// dump is done, Err for a netlink error.
    pub fn request(&mut self, ty: u16, flags: u16, payload: &[u8]) -> io::Result<Vec<Msg>> {
        let seq = self.send(ty, flags | NLM_F_REQUEST | NLM_F_ACK, payload)?;
        let mut out = Vec::new();
        loop {
            for m in self.recv(Some(Duration::from_secs(10)))? {
                if m.seq != seq {
                    continue; // an event or a late reply to something else
                }
                match m.ty {
                    NLMSG_DONE => return Ok(out),
                    NLMSG_ERROR => {
                        let code = i32::from_ne_bytes(m.payload[..4].try_into().unwrap_or([0; 4]));
                        if code == 0 {
                            if flags & NLM_F_DUMP != NLM_F_DUMP {
                                return Ok(out);
                            }
                            continue;
                        }
                        return Err(io::Error::from_raw_os_error(-code));
                    }
                    _ => out.push(m),
                }
            }
        }
    }
}

/// poll() for input. Ok(false) on timeout.
pub fn wait_readable(fd: RawFd, timeout: Option<Duration>) -> io::Result<bool> {
    let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    loop {
        // SAFETY: one pollfd.
        let r = unsafe { libc::poll(&mut p, 1, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(r > 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_round_trip() {
        let mut b = Builder::new(&[1, 2, 3, 4]);
        b.put_u32(1, 7).put_str(2, "wlan0").begin(3).put_u8(1, 9).put(2, b"abcde").end().put_flag(4);
        let a = attrs(&b.buf[4..]);
        assert_eq!(get_u32(&a, 1), Some(7));
        assert_eq!(get_str(&a, 2).as_deref(), Some("wlan0"));
        let nested = attrs(find(&a, 3).unwrap());
        assert_eq!(get_u8(&nested, 1), Some(9));
        assert_eq!(find(&nested, 2), Some(&b"abcde"[..]));
        assert_eq!(find(&a, 4), Some(&[][..]));
        assert_eq!(b.buf.len() % 4, 0);
    }

    #[test]
    fn splitting_messages() {
        let mut buf = Vec::new();
        for (ty, payload) in [(16u16, vec![1u8, 2, 3]), (NLMSG_DONE, vec![0, 0, 0, 0])] {
            let len = 16 + payload.len();
            buf.extend_from_slice(&(len as u32).to_ne_bytes());
            buf.extend_from_slice(&ty.to_ne_bytes());
            buf.extend_from_slice(&NLM_F_MULTI.to_ne_bytes());
            buf.extend_from_slice(&5u32.to_ne_bytes());
            buf.extend_from_slice(&0u32.to_ne_bytes());
            buf.extend_from_slice(&payload);
            buf.resize(align4(buf.len()), 0);
        }
        let m = split(&buf);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].payload, [1, 2, 3]);
        assert_eq!(m[1].ty, NLMSG_DONE);
        assert!(split(&buf[..10]).is_empty());
    }
}
