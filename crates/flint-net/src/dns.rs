//! A small DNS resolver: /etc/hosts, then the servers in /etc/resolv.conf (A and AAAA
//! records over UDP, falling back to TCP when an answer is truncated).

use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

pub const RESOLV_CONF: &str = "/etc/resolv.conf";
const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
const TYPE_AAAA: u16 = 28;

pub fn nameservers(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some("nameserver")).then(|| w.next()).flatten()
        })
        .filter_map(|s| s.parse().ok())
        .take(3)
        .collect()
}

pub fn hosts_lookup(text: &str, name: &str) -> Vec<IpAddr> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut w = line.split_whitespace();
        let Some(ip) = w.next().and_then(|s| s.parse::<IpAddr>().ok()) else { continue };
        if w.any(|h| h.eq_ignore_ascii_case(name)) {
            out.push(ip);
        }
    }
    out
}

pub fn query(id: u16, name: &str, qtype: u16) -> Option<Vec<u8>> {
    let mut q = Vec::with_capacity(32 + name.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]); // recursion desired, 1 question
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&[0, 1]); // class IN
    Some(q)
}

/// Skip a (possibly compressed) name; returns the offset after it.
fn skip_name(m: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *m.get(at)?;
        if len & 0xC0 == 0xC0 {
            return Some(at + 2);
        }
        if len == 0 {
            return Some(at + 1);
        }
        at += 1 + usize::from(len);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Addrs(Vec<IpAddr>),
    /// The server says the name does not exist.
    NoSuchName,
    Truncated,
}

pub fn parse_answer(m: &[u8], id: u16) -> Option<Answer> {
    if m.len() < 12 || m[0..2] != id.to_be_bytes() || m[2] & 0x80 == 0 {
        return None;
    }
    if m[2] & 0x02 != 0 {
        return Some(Answer::Truncated);
    }
    let rcode = m[3] & 0x0F;
    if rcode == 3 {
        return Some(Answer::NoSuchName);
    }
    if rcode != 0 {
        return None;
    }
    let qd = u16::from_be_bytes([m[4], m[5]]);
    let an = u16::from_be_bytes([m[6], m[7]]);
    let mut at = 12;
    for _ in 0..qd {
        at = skip_name(m, at)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        at = skip_name(m, at)?;
        let h = m.get(at..at + 10)?;
        let ty = u16::from_be_bytes([h[0], h[1]]);
        let len = usize::from(u16::from_be_bytes([h[8], h[9]]));
        let data = m.get(at + 10..at + 10 + len)?;
        match (ty, len) {
            (TYPE_A, 4) => out.push(IpAddr::V4(Ipv4Addr::new(data[0], data[1], data[2], data[3]))),
            (TYPE_AAAA, 16) => out.push(IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(data).ok()?))),
            (TYPE_CNAME, _) => {} // the records for the target follow
            _ => {}
        }
        at += 10 + len;
    }
    Some(Answer::Addrs(out))
}

fn random_id() -> u16 {
    let mut b = [0u8; 2];
    if fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_err() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
        return (t ^ std::process::id()) as u16;
    }
    u16::from_ne_bytes(b)
}

fn ask_tcp(server: SocketAddr, q: &[u8], id: u16) -> Option<Answer> {
    let mut s = TcpStream::connect_timeout(&server, Duration::from_secs(3)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let mut msg = (q.len() as u16).to_be_bytes().to_vec();
    msg.extend_from_slice(q);
    s.write_all(&msg).ok()?;
    let mut len = [0u8; 2];
    s.read_exact(&mut len).ok()?;
    let mut buf = vec![0u8; usize::from(u16::from_be_bytes(len))];
    s.read_exact(&mut buf).ok()?;
    parse_answer(&buf, id)
}

fn ask(server: IpAddr, name: &str, qtype: u16) -> Option<Answer> {
    let addr = SocketAddr::new(server, 53);
    let bind: SocketAddr = if server.is_ipv4() { "0.0.0.0:0".parse().ok()? } else { "[::]:0".parse().ok()? };
    let sock = UdpSocket::bind(bind).ok()?;
    sock.connect(addr).ok()?;
    let id = random_id();
    let q = query(id, name, qtype)?;
    for timeout in [2, 3] {
        sock.set_read_timeout(Some(Duration::from_secs(timeout))).ok()?;
        sock.send(&q).ok()?;
        let mut buf = [0u8; 1500];
        loop {
            match sock.recv(&mut buf) {
                Ok(n) => match parse_answer(&buf[..n], id) {
                    Some(Answer::Truncated) => return ask_tcp(addr, &q, id),
                    Some(a) => return Some(a),
                    None => continue, // not our answer
                },
                Err(_) => break,
            }
        }
    }
    None
}

/// Addresses for `host`: an address literal, /etc/hosts, then DNS (IPv4 first).
pub fn resolve(host: &str) -> io::Result<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let hosts = hosts_lookup(&fs::read_to_string("/etc/hosts").unwrap_or_default(), host);
    if !hosts.is_empty() {
        return Ok(hosts);
    }
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    }
    let servers = nameservers(&fs::read_to_string(RESOLV_CONF).unwrap_or_default());
    if servers.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotConnected, "no DNS servers (is the network up? see: net)"));
    }
    let mut answered = false;
    for s in servers {
        let mut out = Vec::new();
        for qtype in [TYPE_A, TYPE_AAAA] {
            match ask(s, host, qtype) {
                Some(Answer::Addrs(a)) => {
                    answered = true;
                    out.extend(a);
                }
                Some(Answer::NoSuchName) => return Err(io::Error::new(io::ErrorKind::NotFound, format!("{host}: no such host"))),
                _ => {}
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    if answered {
        Err(io::Error::new(io::ErrorKind::NotFound, format!("{host}: no addresses")))
    } else {
        Err(io::Error::new(io::ErrorKind::TimedOut, format!("{host}: no answer from the DNS servers")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_files() {
        let ns = nameservers("# x\nnameserver 10.0.2.3\nsearch lan\nnameserver ::1\nnameserver bogus\n");
        assert_eq!(ns, [IpAddr::V4(Ipv4Addr::new(10, 0, 2, 3)), "::1".parse().unwrap()]);
        let h = hosts_lookup("127.0.0.1 localhost flint # me\n::1 localhost\n", "flint");
        assert_eq!(h, [IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    }

    #[test]
    fn query_and_answer() {
        let q = query(0xABCD, "example.org", TYPE_A).unwrap();
        assert_eq!(&q[12..25], b"\x07example\x03org\x00");
        assert!(query(1, "a..b", TYPE_A).is_none());
        // Response: the question, a CNAME, then an A record using compression pointers.
        let mut r = q.clone();
        r[2] = 0x81;
        r[3] = 0x80;
        r[7] = 2;
        r.extend_from_slice(&[0xC0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 6, 3, b'w', b'w', b'w', 0xC0, 12]);
        r.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34]);
        assert_eq!(parse_answer(&r, 0xABCD), Some(Answer::Addrs(vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))])));
        assert_eq!(parse_answer(&r, 0xABCE), None);
        let mut nx = q.clone();
        nx[2] = 0x81;
        nx[3] = 0x83;
        assert_eq!(parse_answer(&nx, 0xABCD), Some(Answer::NoSuchName));
        r.truncate(r.len() - 3);
        assert_eq!(parse_answer(&r, 0xABCD), None, "cut short");
    }
}
