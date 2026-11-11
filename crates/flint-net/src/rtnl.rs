//! Interfaces, addresses and routes over rtnetlink.

use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::time::Duration;

use crate::netlink::{self, Builder, Socket, NLM_F_CREATE, NLM_F_DUMP, NLM_F_REPLACE};

const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_GETADDR: u16 = 22;
const RTM_NEWROUTE: u16 = 24;
const RTM_DELROUTE: u16 = 25;
const RTM_GETROUTE: u16 = 26;

pub const RTMGRP_LINK: u32 = 1;

const IFLA_ADDRESS: u16 = 1;
const IFLA_IFNAME: u16 = 3;
const IFLA_OPERSTATE: u16 = 16;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_BROADCAST: u16 = 4;
const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PRIORITY: u16 = 6;

pub const IFF_UP: u32 = 1;
pub const IFF_LOOPBACK: u32 = 8;
pub const IFF_LOWER_UP: u32 = 0x10000;
const ARPHRD_ETHER: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub index: u32,
    pub name: String,
    pub flags: u32,
    pub kind: u16,
    pub mac: Vec<u8>,
    /// IF_OPER_UP (6) means the link is usable.
    pub operstate: u8,
}

impl Link {
    pub fn is_up(&self) -> bool {
        self.flags & IFF_UP != 0
    }

    pub fn has_carrier(&self) -> bool {
        self.flags & IFF_LOWER_UP != 0
    }

    pub fn is_ethernet_like(&self) -> bool {
        self.kind == ARPHRD_ETHER && self.mac.len() == 6
    }

    pub fn is_wireless(&self) -> bool {
        is_wireless(&self.name)
    }

    /// A physical device (or a virtual machine's NIC), not a bridge, tunnel or veth.
    pub fn is_hardware(&self) -> bool {
        fs::metadata(format!("/sys/class/net/{}/device", self.name)).is_ok()
    }
}

pub fn is_wireless(name: &str) -> bool {
    fs::metadata(format!("/sys/class/net/{name}/phy80211")).is_ok() || fs::metadata(format!("/sys/class/net/{name}/wireless")).is_ok()
}

pub fn mac_text(mac: &[u8]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

fn ifinfomsg(index: u32, flags: u32, change: u32) -> [u8; 16] {
    let mut m = [0u8; 16];
    m[0] = libc::AF_UNSPEC as u8;
    m[4..8].copy_from_slice(&(index as i32).to_ne_bytes());
    m[8..12].copy_from_slice(&flags.to_ne_bytes());
    m[12..16].copy_from_slice(&change.to_ne_bytes());
    m
}

pub fn parse_link(payload: &[u8]) -> Option<Link> {
    if payload.len() < 16 {
        return None;
    }
    let kind = u16::from_ne_bytes([payload[2], payload[3]]);
    let index = i32::from_ne_bytes(payload[4..8].try_into().ok()?) as u32;
    let flags = u32::from_ne_bytes(payload[8..12].try_into().ok()?);
    let a = netlink::attrs(&payload[16..]);
    Some(Link {
        index,
        name: netlink::get_str(&a, IFLA_IFNAME)?,
        flags,
        kind,
        mac: netlink::find(&a, IFLA_ADDRESS).map(<[u8]>::to_vec).unwrap_or_default(),
        operstate: netlink::get_u8(&a, IFLA_OPERSTATE).unwrap_or(0),
    })
}

pub struct Rtnl {
    pub sock: Socket,
}

impl Rtnl {
    pub fn open() -> io::Result<Rtnl> {
        Ok(Rtnl { sock: Socket::open(libc::NETLINK_ROUTE)? })
    }

    /// A second socket that receives link changes (carrier up/down, new devices).
    pub fn events() -> io::Result<Rtnl> {
        let r = Rtnl::open()?;
        r.sock.join(RTMGRP_LINK)?;
        Ok(r)
    }

    /// Link messages that arrived on an events socket.
    pub fn next_links(&self, timeout: Option<Duration>) -> io::Result<Vec<Link>> {
        match self.sock.recv(timeout) {
            Ok(msgs) => Ok(msgs.iter().filter(|m| m.ty == RTM_NEWLINK).filter_map(|m| parse_link(&m.payload)).collect()),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    pub fn links(&mut self) -> io::Result<Vec<Link>> {
        let msgs = self.sock.request(RTM_GETLINK, NLM_F_DUMP, &ifinfomsg(0, 0, 0))?;
        let mut v: Vec<Link> = msgs.iter().filter(|m| m.ty == RTM_NEWLINK).filter_map(|m| parse_link(&m.payload)).collect();
        v.sort_by_key(|l| l.index);
        Ok(v)
    }

    pub fn set_up(&mut self, index: u32, up: bool) -> io::Result<()> {
        self.sock.request(RTM_NEWLINK, 0, &ifinfomsg(index, if up { IFF_UP } else { 0 }, IFF_UP)).map(|_| ())
    }

    fn ifaddrmsg(index: u32, prefix: u8) -> [u8; 8] {
        let mut m = [0u8; 8];
        m[0] = libc::AF_INET as u8;
        m[1] = prefix;
        m[4..8].copy_from_slice(&index.to_ne_bytes());
        m
    }

    /// IPv4 addresses: (interface index, address, prefix length).
    pub fn addresses(&mut self) -> io::Result<Vec<(u32, Ipv4Addr, u8)>> {
        let msgs = self.sock.request(RTM_GETADDR, NLM_F_DUMP, &Self::ifaddrmsg(0, 0))?;
        let mut out = Vec::new();
        for m in msgs.iter().filter(|m| m.ty == RTM_NEWADDR && m.payload.len() >= 8) {
            if m.payload[0] != libc::AF_INET as u8 {
                continue;
            }
            let prefix = m.payload[1];
            let index = u32::from_ne_bytes(m.payload[4..8].try_into().unwrap());
            let a = netlink::attrs(&m.payload[8..]);
            if let Some(ip) = netlink::find(&a, IFA_LOCAL).or_else(|| netlink::find(&a, IFA_ADDRESS)).filter(|v| v.len() == 4) {
                out.push((index, Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]), prefix));
            }
        }
        Ok(out)
    }

    pub fn add_address(&mut self, index: u32, ip: Ipv4Addr, prefix: u8) -> io::Result<()> {
        let mut b = Builder::new(&Self::ifaddrmsg(index, prefix));
        let bcast = if prefix < 31 { u32::from(ip) | (u32::MAX >> prefix) } else { u32::from(ip) };
        b.put(IFA_LOCAL, &ip.octets()).put(IFA_ADDRESS, &ip.octets()).put(IFA_BROADCAST, &Ipv4Addr::from(bcast).octets());
        self.sock.request(RTM_NEWADDR, NLM_F_CREATE | NLM_F_REPLACE, &b.buf).map(|_| ())
    }

    pub fn flush_addresses(&mut self, index: u32) -> io::Result<()> {
        for (i, ip, prefix) in self.addresses()? {
            if i == index {
                let mut b = Builder::new(&Self::ifaddrmsg(index, prefix));
                b.put(IFA_LOCAL, &ip.octets());
                let _ = self.sock.request(RTM_DELADDR, 0, &b.buf);
            }
        }
        Ok(())
    }

    fn rtmsg(dst_len: u8) -> [u8; 12] {
        let mut m = [0u8; 12];
        m[0] = libc::AF_INET as u8;
        m[1] = dst_len;
        m[4] = libc::RT_TABLE_MAIN;
        m[5] = 16; // RTPROT_DHCP
        m[6] = libc::RT_SCOPE_UNIVERSE;
        m[7] = libc::RTN_UNICAST;
        m
    }

    /// A default route through `gw` on interface `index`. A lower metric wins, so wired
    /// links are given lower ones than Wi-Fi.
    pub fn add_default_route(&mut self, index: u32, gw: Ipv4Addr, metric: u32) -> io::Result<()> {
        let mut b = Builder::new(&Self::rtmsg(0));
        b.put(RTA_GATEWAY, &gw.octets()).put_u32(RTA_OIF, index).put_u32(RTA_PRIORITY, metric);
        self.sock.request(RTM_NEWROUTE, NLM_F_CREATE | NLM_F_REPLACE, &b.buf).map(|_| ())
    }

    /// Default routes: (interface index, gateway, metric).
    pub fn default_routes(&mut self) -> io::Result<Vec<(u32, Ipv4Addr, u32)>> {
        let msgs = self.sock.request(RTM_GETROUTE, NLM_F_DUMP, &Self::rtmsg(0))?;
        let mut out = Vec::new();
        for m in msgs.iter().filter(|m| m.ty == RTM_NEWROUTE && m.payload.len() >= 12) {
            if m.payload[0] != libc::AF_INET as u8 || m.payload[1] != 0 || m.payload[4] != libc::RT_TABLE_MAIN {
                continue;
            }
            let a = netlink::attrs(&m.payload[12..]);
            if netlink::find(&a, RTA_DST).is_some() {
                continue;
            }
            let (Some(gw), Some(oif)) = (netlink::find(&a, RTA_GATEWAY).filter(|g| g.len() == 4), netlink::get_u32(&a, RTA_OIF)) else { continue };
            out.push((oif, Ipv4Addr::new(gw[0], gw[1], gw[2], gw[3]), netlink::get_u32(&a, RTA_PRIORITY).unwrap_or(0)));
        }
        Ok(out)
    }

    pub fn delete_default_routes(&mut self, index: u32) -> io::Result<()> {
        for (oif, gw, metric) in self.default_routes()? {
            if oif == index {
                let mut b = Builder::new(&Self::rtmsg(0));
                b.put(RTA_GATEWAY, &gw.octets()).put_u32(RTA_OIF, oif).put_u32(RTA_PRIORITY, metric);
                let _ = self.sock.request(RTM_DELROUTE, 0, &b.buf);
            }
        }
        Ok(())
    }
}

/// Netmask to prefix length (255.255.255.0 -> 24). Non-contiguous masks count their ones.
pub fn prefix_len(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_parsing() {
        let mut b = Builder::new(&ifinfomsg(3, IFF_UP | IFF_LOWER_UP, 0));
        b.buf[2..4].copy_from_slice(&ARPHRD_ETHER.to_ne_bytes());
        b.put_str(IFLA_IFNAME, "eth0").put(IFLA_ADDRESS, &[0x52, 0x54, 0, 0x12, 0x34, 0x56]).put_u8(IFLA_OPERSTATE, 6);
        let l = parse_link(&b.buf).unwrap();
        assert_eq!(l.index, 3);
        assert_eq!(l.name, "eth0");
        assert!(l.is_up() && l.has_carrier() && l.is_ethernet_like());
        assert_eq!(mac_text(&l.mac), "52:54:00:12:34:56");
        assert_eq!(prefix_len(Ipv4Addr::new(255, 255, 255, 0)), 24);
        assert_eq!(prefix_len(Ipv4Addr::new(255, 255, 240, 0)), 20);
    }
}
