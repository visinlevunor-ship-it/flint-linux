//! DHCP (RFC 2131/2132) messages: building requests and reading replies.

use std::net::Ipv4Addr;

pub const DISCOVER: u8 = 1;
pub const OFFER: u8 = 2;
pub const REQUEST: u8 = 3;
pub const ACK: u8 = 5;
pub const NAK: u8 = 6;
pub const RELEASE: u8 = 7;

const MAGIC: [u8; 4] = [99, 130, 83, 99];
const OPT_PAD: u8 = 0;
const OPT_SUBNET: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_HOSTNAME: u8 = 12;
const OPT_DOMAIN: u8 = 15;
const OPT_MTU: u8 = 26;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_LEASE: u8 = 51;
const OPT_OVERLOAD: u8 = 52;
const OPT_MSG_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAMS: u8 = 55;
const OPT_MAX_SIZE: u8 = 57;
const OPT_T1: u8 = 58;
const OPT_T2: u8 = 59;
const OPT_CLIENT_ID: u8 = 61;
const OPT_END: u8 = 255;

pub struct Request<'a> {
    pub kind: u8,
    pub xid: u32,
    pub mac: [u8; 6],
    /// Our current address, when renewing.
    pub ciaddr: Option<Ipv4Addr>,
    pub requested: Option<Ipv4Addr>,
    pub server: Option<Ipv4Addr>,
    pub hostname: Option<&'a str>,
    /// Seconds since we started trying.
    pub secs: u16,
}

pub fn build(r: &Request) -> Vec<u8> {
    let mut p = vec![0u8; 236];
    p[0] = 1; // BOOTREQUEST
    p[1] = 1; // Ethernet
    p[2] = 6;
    p[4..8].copy_from_slice(&r.xid.to_be_bytes());
    p[8..10].copy_from_slice(&r.secs.to_be_bytes());
    if r.ciaddr.is_none() {
        p[10] = 0x80; // broadcast flag: we cannot receive unicast before we have an address
    }
    if let Some(c) = r.ciaddr {
        p[12..16].copy_from_slice(&c.octets());
    }
    p[28..34].copy_from_slice(&r.mac);
    p.extend_from_slice(&MAGIC);
    p.extend_from_slice(&[OPT_MSG_TYPE, 1, r.kind]);
    let mut cid = vec![OPT_CLIENT_ID, 7, 1];
    cid.extend_from_slice(&r.mac);
    p.extend_from_slice(&cid);
    if let Some(ip) = r.requested {
        p.push(OPT_REQUESTED_IP);
        p.push(4);
        p.extend_from_slice(&ip.octets());
    }
    if let Some(ip) = r.server {
        p.push(OPT_SERVER_ID);
        p.push(4);
        p.extend_from_slice(&ip.octets());
    }
    if let Some(h) = r.hostname.filter(|h| !h.is_empty() && h.len() < 64) {
        p.push(OPT_HOSTNAME);
        p.push(h.len() as u8);
        p.extend_from_slice(h.as_bytes());
    }
    if r.kind != RELEASE {
        p.extend_from_slice(&[OPT_MAX_SIZE, 2, 0x05, 0xDC]);
        p.extend_from_slice(&[OPT_PARAMS, 7, OPT_SUBNET, OPT_ROUTER, OPT_DNS, OPT_DOMAIN, OPT_MTU, OPT_T1, OPT_T2]);
    }
    p.push(OPT_END);
    // Some old servers ignore requests shorter than a BOOTP packet (300 bytes).
    if p.len() < 300 {
        p.resize(300, OPT_PAD);
    }
    p
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub kind: u8,
    pub yiaddr: Ipv4Addr,
    pub server: Option<Ipv4Addr>,
    pub subnet: Option<Ipv4Addr>,
    pub routers: Vec<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    pub lease_secs: Option<u32>,
    pub t1: Option<u32>,
    pub t2: Option<u32>,
    pub mtu: Option<u16>,
}

fn ips(v: &[u8]) -> Vec<Ipv4Addr> {
    v.chunks_exact(4).map(|c| Ipv4Addr::new(c[0], c[1], c[2], c[3])).collect()
}

fn be32(v: &[u8]) -> Option<u32> {
    v.get(..4).map(|b| u32::from_be_bytes(b.try_into().unwrap()))
}

/// Read a reply meant for us (matching transaction and hardware address).
pub fn parse(p: &[u8], xid: u32, mac: &[u8; 6]) -> Option<Reply> {
    if p.len() < 240 || p[0] != 2 || p[4..8] != xid.to_be_bytes() || &p[28..34] != mac || p[236..240] != MAGIC {
        return None;
    }
    let mut r = Reply {
        kind: 0,
        yiaddr: Ipv4Addr::new(p[16], p[17], p[18], p[19]),
        server: None,
        subnet: None,
        routers: Vec::new(),
        dns: Vec::new(),
        domain: None,
        lease_secs: None,
        t1: None,
        t2: None,
        mtu: None,
    };
    // Options, then (if the server overloaded them) the file and sname fields.
    let mut areas: Vec<&[u8]> = vec![&p[240..]];
    let mut i = 0;
    while i < areas.len() {
        let area = areas[i];
        i += 1;
        let mut at = 0;
        while at < area.len() {
            let code = area[at];
            if code == OPT_PAD {
                at += 1;
                continue;
            }
            if code == OPT_END || at + 1 >= area.len() {
                break;
            }
            let len = area[at + 1] as usize;
            let Some(v) = area.get(at + 2..at + 2 + len) else { break };
            match code {
                OPT_MSG_TYPE => r.kind = *v.first()?,
                OPT_SERVER_ID => r.server = ips(v).first().copied(),
                OPT_SUBNET => r.subnet = ips(v).first().copied(),
                OPT_ROUTER => r.routers = ips(v),
                OPT_DNS => r.dns = ips(v),
                OPT_DOMAIN => r.domain = Some(String::from_utf8_lossy(v).trim_end_matches('\0').to_string()).filter(|d| !d.is_empty()),
                OPT_LEASE => r.lease_secs = be32(v),
                OPT_T1 => r.t1 = be32(v),
                OPT_T2 => r.t2 = be32(v),
                OPT_MTU => r.mtu = v.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]])),
                OPT_OVERLOAD if i == 1 => {
                    let o = v.first().copied().unwrap_or(0);
                    if o & 1 != 0 {
                        areas.push(&p[108..236]);
                    }
                    if o & 2 != 0 {
                        areas.push(&p[44..108]);
                    }
                }
                _ => {}
            }
            at += 2 + len;
        }
    }
    (r.kind != 0).then_some(r)
}

/// When to renew and rebind, from the lease (defaults: half and 7/8 of it).
pub fn timers(r: &Reply) -> (u32, u32, u32) {
    let lease = r.lease_secs.unwrap_or(3600).max(60);
    let t1 = r.t1.filter(|&t| t < lease).unwrap_or(lease / 2);
    let t2 = r.t2.filter(|&t| t > t1 && t < lease).unwrap_or(lease / 8 * 7);
    (t1, t2.max(t1), lease)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];

    fn reply(kind: u8, opts: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 236];
        p[0] = 2;
        p[4..8].copy_from_slice(&0x1234_5678u32.to_be_bytes());
        p[16..20].copy_from_slice(&[10, 0, 2, 15]);
        p[28..34].copy_from_slice(&MAC);
        p.extend_from_slice(&MAGIC);
        p.extend_from_slice(&[OPT_MSG_TYPE, 1, kind]);
        p.extend_from_slice(opts);
        p.push(OPT_END);
        p
    }

    #[test]
    fn request_layout() {
        let p = build(&Request { kind: DISCOVER, xid: 0x1234_5678, mac: MAC, ciaddr: None, requested: Some(Ipv4Addr::new(10, 0, 2, 15)), server: None, hostname: Some("flint"), secs: 3 });
        assert_eq!(p.len(), 300);
        assert_eq!(&p[4..8], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(p[10], 0x80, "broadcast flag");
        assert_eq!(&p[28..34], &MAC);
        assert_eq!(&p[236..240], &MAGIC);
        assert_eq!(&p[240..243], &[OPT_MSG_TYPE, 1, DISCOVER]);
        let renew = build(&Request { kind: REQUEST, xid: 1, mac: MAC, ciaddr: Some(Ipv4Addr::new(10, 0, 2, 15)), requested: None, server: None, hostname: None, secs: 0 });
        assert_eq!(renew[10], 0);
        assert_eq!(&renew[12..16], &[10, 0, 2, 15]);
    }

    #[test]
    fn reading_replies() {
        let p = reply(ACK, &[1, 4, 255, 255, 255, 0, 3, 8, 10, 0, 2, 2, 10, 0, 2, 1, 6, 4, 10, 0, 2, 3, 51, 4, 0, 0, 0x0E, 0x10, 54, 4, 10, 0, 2, 2, 15, 4, b'l', b'a', b'n', 0]);
        let r = parse(&p, 0x1234_5678, &MAC).unwrap();
        assert_eq!(r.kind, ACK);
        assert_eq!(r.yiaddr, Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(r.subnet, Some(Ipv4Addr::new(255, 255, 255, 0)));
        assert_eq!(r.routers, [Ipv4Addr::new(10, 0, 2, 2), Ipv4Addr::new(10, 0, 2, 1)]);
        assert_eq!(r.dns, [Ipv4Addr::new(10, 0, 2, 3)]);
        assert_eq!(r.lease_secs, Some(3600));
        assert_eq!(r.server, Some(Ipv4Addr::new(10, 0, 2, 2)));
        assert_eq!(r.domain.as_deref(), Some("lan"));
        assert_eq!(timers(&r), (1800, 3150, 3600));

        assert!(parse(&p, 0x1234_5679, &MAC).is_none(), "other transaction");
        assert!(parse(&p, 0x1234_5678, &[0; 6]).is_none(), "other client");
        assert!(parse(&p[..200], 0x1234_5678, &MAC).is_none());
        let truncated_option = reply(OFFER, &[6, 200, 1, 2]);
        assert_eq!(parse(&truncated_option, 0x1234_5678, &MAC).unwrap().dns, Vec::<Ipv4Addr>::new());
    }
}
