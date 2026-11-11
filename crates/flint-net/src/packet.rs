//! Raw IPv4/UDP over a packet socket: how a DHCP client talks before it has an address.

use std::io;
use std::mem;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use crate::netlink::wait_readable;

const ETH_P_IP: u16 = 0x0800;

pub fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for c in data.chunks(2) {
        sum += u32::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]));
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// IPv4 + UDP headers around `payload`.
pub fn ip_udp(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let total = 20 + 8 + payload.len();
    let mut p = Vec::with_capacity(total);
    p.extend_from_slice(&[0x45, 0x10]); // version 4, 20-byte header; low-delay TOS like dhclient
    p.extend_from_slice(&(total as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0, 64, 17, 0, 0]); // id, flags, ttl 64, UDP, checksum later
    p.extend_from_slice(&src.octets());
    p.extend_from_slice(&dst.octets());
    let c = checksum(&p[..20]);
    p[10..12].copy_from_slice(&c.to_be_bytes());
    let ulen = (8 + payload.len()) as u16;
    p.extend_from_slice(&sport.to_be_bytes());
    p.extend_from_slice(&dport.to_be_bytes());
    p.extend_from_slice(&ulen.to_be_bytes());
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(payload);
    // UDP checksum over the pseudo header.
    let mut pseudo = Vec::with_capacity(12 + ulen as usize);
    pseudo.extend_from_slice(&src.octets());
    pseudo.extend_from_slice(&dst.octets());
    pseudo.extend_from_slice(&[0, 17]);
    pseudo.extend_from_slice(&ulen.to_be_bytes());
    pseudo.extend_from_slice(&p[20..]);
    let mut uc = checksum(&pseudo);
    if uc == 0 {
        uc = 0xFFFF;
    }
    p[26..28].copy_from_slice(&uc.to_be_bytes());
    p
}

/// The UDP payload of an IPv4 packet sent to `dport`, if that is what it is.
pub fn udp_payload(p: &[u8], dport: u16) -> Option<&[u8]> {
    if p.len() < 28 || p[0] >> 4 != 4 || p[9] != 17 {
        return None;
    }
    let ihl = usize::from(p[0] & 15) * 4;
    let total = usize::from(u16::from_be_bytes([p[2], p[3]])).min(p.len());
    if ihl < 20 || total < ihl + 8 || u16::from_be_bytes([p[6], p[7]]) & 0x3FFF != 0 {
        return None; // malformed or a fragment
    }
    let u = &p[ihl..total];
    if u16::from_be_bytes([u[2], u[3]]) != dport {
        return None;
    }
    let ulen = usize::from(u16::from_be_bytes([u[4], u[5]])).min(u.len());
    if ulen < 8 {
        return None;
    }
    Some(&u[8..ulen])
}

pub const ETH_P_PAE: u16 = 0x888E;

pub struct PacketSocket {
    fd: OwnedFd,
    ifindex: u32,
    proto: u16,
}

impl PacketSocket {
    /// IPv4 packets on one interface.
    pub fn open(ifindex: u32) -> io::Result<PacketSocket> {
        Self::open_proto(ifindex, ETH_P_IP)
    }

    /// Frames of one ethertype on one interface (payload after the Ethernet header).
    pub fn open_proto(ifindex: u32, proto: u16) -> io::Result<PacketSocket> {
        // SAFETY: socket + bind with a zeroed, filled sockaddr_ll.
        unsafe {
            let fd = libc::socket(libc::AF_PACKET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, i32::from(proto.to_be()));
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut sa: libc::sockaddr_ll = mem::zeroed();
            sa.sll_family = libc::AF_PACKET as u16;
            sa.sll_protocol = proto.to_be();
            sa.sll_ifindex = ifindex as i32;
            if libc::bind(fd.as_raw_fd(), (&sa as *const libc::sockaddr_ll).cast(), mem::size_of::<libc::sockaddr_ll>() as u32) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(PacketSocket { fd, ifindex, proto })
        }
    }

    pub fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    /// Send to the Ethernet broadcast address.
    pub fn broadcast(&self, packet: &[u8]) -> io::Result<()> {
        self.send_to(&[0xFF; 6], packet)
    }

    pub fn send_to(&self, mac: &[u8; 6], packet: &[u8]) -> io::Result<()> {
        // SAFETY: sendto with a filled sockaddr_ll.
        unsafe {
            let mut sa: libc::sockaddr_ll = mem::zeroed();
            sa.sll_family = libc::AF_PACKET as u16;
            sa.sll_protocol = self.proto.to_be();
            sa.sll_ifindex = self.ifindex as i32;
            sa.sll_halen = 6;
            sa.sll_addr[..6].copy_from_slice(mac);
            let r = libc::sendto(self.fd.as_raw_fd(), packet.as_ptr().cast(), packet.len(), 0, (&sa as *const libc::sockaddr_ll).cast(), mem::size_of::<libc::sockaddr_ll>() as u32);
            if r < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// The next packet, or None after `timeout`.
    pub fn recv(&self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        Ok(self.recv_from(timeout)?.map(|(p, _)| p))
    }

    /// The next packet and the sender's hardware address.
    pub fn recv_from(&self, timeout: Duration) -> io::Result<Option<(Vec<u8>, [u8; 6])>> {
        if !wait_readable(self.fd.as_raw_fd(), Some(timeout))? {
            return Ok(None);
        }
        let mut buf = vec![0u8; 4096];
        // SAFETY: recvfrom into our buffers.
        unsafe {
            let mut sa: libc::sockaddr_ll = mem::zeroed();
            let mut len = mem::size_of::<libc::sockaddr_ll>() as u32;
            let n = libc::recvfrom(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0, (&mut sa as *mut libc::sockaddr_ll).cast(), &mut len);
            if n < 0 {
                let e = io::Error::last_os_error();
                return if e.kind() == io::ErrorKind::Interrupted { Ok(None) } else { Err(e) };
            }
            buf.truncate(n as usize);
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&sa.sll_addr[..6]);
            Ok(Some((buf, mac)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_round_trip() {
        let p = ip_udp(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, 68, 67, b"hello");
        assert_eq!(p.len(), 33);
        assert_eq!(checksum(&p[..20]), 0, "header checksum verifies");
        assert_eq!(udp_payload(&p, 67), Some(&b"hello"[..]));
        assert_eq!(udp_payload(&p, 68), None);
        assert_eq!(udp_payload(&p[..20], 67), None);
        // RFC 1071 example
        assert_eq!(checksum(&[0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7]), !0xDDF2);
    }
}
