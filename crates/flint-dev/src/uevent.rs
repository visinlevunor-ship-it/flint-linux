//! Kernel hotplug events (uevents) over netlink.

use std::collections::HashMap;
use std::io;

pub struct Socket {
    fd: libc::c_int,
}

impl Socket {
    pub fn open() -> io::Result<Socket> {
        // SAFETY: plain socket setup with a zeroed sockaddr_nl.
        unsafe {
            let fd = libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, libc::NETLINK_KOBJECT_UEVENT);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let size: libc::c_int = 4 << 20;
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUFFORCE, (&size as *const libc::c_int).cast(), 4);
            let mut addr: libc::sockaddr_nl = std::mem::zeroed();
            addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            addr.nl_groups = 1;
            if libc::bind(fd, (&addr as *const libc::sockaddr_nl).cast(), std::mem::size_of::<libc::sockaddr_nl>() as u32) != 0 {
                let e = io::Error::last_os_error();
                libc::close(fd);
                return Err(e);
            }
            Ok(Socket { fd })
        }
    }

    /// Wait for the next event.
    pub fn recv(&self) -> io::Result<HashMap<String, String>> {
        let mut buf = vec![0u8; 16384];
        loop {
            // SAFETY: the buffer is valid for its length.
            let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(e);
            }
            return Ok(parse(&buf[..n as usize]));
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // SAFETY: closing our own descriptor.
        unsafe { libc::close(self.fd) };
    }
}

/// `add@/devices/...\0ACTION=add\0KEY=VALUE\0...` -> map of KEY=VALUE pairs.
pub fn parse(msg: &[u8]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for field in msg.split(|&b| b == 0) {
        let s = String::from_utf8_lossy(field);
        if let Some((k, v)) = s.split_once('=') {
            m.insert(k.to_string(), v.to_string());
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_messages() {
        let raw = b"add@/devices/pci0000:00/usb1/1-2\0ACTION=add\0DEVPATH=/devices/pci0000:00/usb1/1-2\0SUBSYSTEM=usb\0MODALIAS=usb:v0BDAp8153d3000dc00dsc00dp00icFFisc00ip00in00\0SEQNUM=1234\0";
        let m = parse(raw);
        assert_eq!(m["ACTION"], "add");
        assert_eq!(m["SUBSYSTEM"], "usb");
        assert!(m["MODALIAS"].starts_with("usb:v0BDAp8153"));
        assert!(!m.contains_key("add@/devices/pci0000:00/usb1/1-2"));
    }
}
