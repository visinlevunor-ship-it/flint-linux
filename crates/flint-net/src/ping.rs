//! `ping`: ICMP echo, IPv4 and IPv6.

use std::io;
use std::mem;
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::dns;
use crate::netlink::wait_readable;

const USAGE: &str = "usage: ping [-c COUNT] [-i SECONDS] [-W SECONDS] [-4|-6] HOST";

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

struct Opts {
    count: Option<u32>,
    interval: f64,
    wait: f64,
    family: Option<u8>,
    host: String,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { count: None, interval: 1.0, wait: 2.0, family: None, host: String::new() };
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let mut num = |what: &str| -> Result<f64, String> { it.next().and_then(|v| v.parse::<f64>().ok()).filter(|v| *v >= 0.0).ok_or(format!("{what} needs a number")) };
        match a.as_str() {
            "-c" => o.count = Some(num("-c")? as u32),
            "-i" => o.interval = num("-i")?.max(0.2),
            "-W" => o.wait = num("-W")?.max(0.1),
            "-4" => o.family = Some(4),
            "-6" => o.family = Some(6),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s if o.host.is_empty() => o.host = s.to_string(),
            _ => return Err("only one host".into()),
        }
    }
    if o.host.is_empty() {
        return Err("no host given".into());
    }
    Ok(o)
}

fn open(v6: bool) -> io::Result<(OwnedFd, bool)> {
    let (dom, proto) = if v6 { (libc::AF_INET6, libc::IPPROTO_ICMPV6) } else { (libc::AF_INET, libc::IPPROTO_ICMP) };
    // Unprivileged ICMP sockets first (allowed by net.ipv4.ping_group_range), then raw (root).
    for ty in [libc::SOCK_DGRAM, libc::SOCK_RAW] {
        // SAFETY: plain socket call.
        let fd = unsafe { libc::socket(dom, ty | libc::SOCK_CLOEXEC, proto) };
        if fd >= 0 {
            // SAFETY: fresh descriptor we own.
            return Ok((unsafe { OwnedFd::from_raw_fd(fd) }, ty == libc::SOCK_RAW));
        }
    }
    Err(io::Error::last_os_error())
}

fn sockaddr(ip: IpAddr) -> (libc::sockaddr_storage, u32) {
    // SAFETY: zeroed storage, then filled as the right family.
    unsafe {
        let mut ss: libc::sockaddr_storage = mem::zeroed();
        let len = match ip {
            IpAddr::V4(a) => {
                let sin = &mut *(&mut ss as *mut libc::sockaddr_storage).cast::<libc::sockaddr_in>();
                sin.sin_family = libc::AF_INET as u16;
                sin.sin_addr.s_addr = u32::from_ne_bytes(a.octets());
                mem::size_of::<libc::sockaddr_in>()
            }
            IpAddr::V6(a) => {
                let sin = &mut *(&mut ss as *mut libc::sockaddr_storage).cast::<libc::sockaddr_in6>();
                sin.sin6_family = libc::AF_INET6 as u16;
                sin.sin6_addr.s6_addr = a.octets();
                mem::size_of::<libc::sockaddr_in6>()
            }
        };
        (ss, len as u32)
    }
}

pub fn echo_request(v6: bool, id: u16, seq: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![if v6 { 128 } else { 8 }, 0, 0, 0];
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(payload);
    if !v6 {
        // The kernel fills in the checksum for ICMPv6, and for ICMPv4 on datagram sockets,
        // but raw IPv4 sockets need it.
        let c = crate::packet::checksum(&p);
        p[2..4].copy_from_slice(&c.to_be_bytes());
    }
    p
}

pub fn ping(args: Vec<String>) -> i32 {
    let o = match parse(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("ping: {e}\n{USAGE}");
            return 2;
        }
    };
    let addrs = match dns::resolve(&o.host) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ping: {e}");
            return 2;
        }
    };
    let Some(ip) = addrs.iter().copied().find(|a| match o.family {
        Some(4) => a.is_ipv4(),
        Some(6) => a.is_ipv6(),
        _ => true,
    }) else {
        eprintln!("ping: {}: no address of that family", o.host);
        return 2;
    };
    let v6 = ip.is_ipv6();
    let (fd, raw) = match open(v6) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ping: cannot open an ICMP socket: {e}");
            return 2;
        }
    };
    // SAFETY: installing a handler that only sets an atomic.
    unsafe {
        let mut sa: libc::sigaction = mem::zeroed();
        sa.sa_sigaction = on_sigint as *const () as usize;
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
    let (dst, dlen) = sockaddr(ip);
    let id = (std::process::id() & 0xFFFF) as u16;
    let payload: Vec<u8> = (0..56u8).collect();
    println!("PING {} ({ip}): {} data bytes", o.host, payload.len());
    let (mut sent, mut got) = (0u32, 0u32);
    let mut rtts: Vec<f64> = Vec::new();
    let start = Instant::now();
    let mut seq: u16 = 1;
    while !STOP.load(Ordering::SeqCst) && o.count.map_or(true, |c| sent < c) {
        let pkt = echo_request(v6, id, seq, &payload);
        let t0 = Instant::now();
        // SAFETY: sendto with our buffer and a filled sockaddr.
        let r = unsafe { libc::sendto(fd.as_raw_fd(), pkt.as_ptr().cast(), pkt.len(), 0, (&dst as *const libc::sockaddr_storage).cast(), dlen) };
        if r < 0 {
            eprintln!("ping: sending to {ip}: {}", io::Error::last_os_error());
        }
        sent += 1;
        // Wait for the reply (or the interval, whichever is longer is the wait budget).
        let budget = Duration::from_secs_f64(if o.count == Some(sent) { o.wait } else { o.interval.max(0.2) });
        let deadline = t0 + budget;
        let mut answered = false;
        while !STOP.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match wait_readable(fd.as_raw_fd(), Some(deadline - now)) {
                Ok(true) => {}
                _ => break,
            }
            let mut buf = [0u8; 2048];
            let mut from: libc::sockaddr_storage = unsafe { mem::zeroed() };
            let mut flen = mem::size_of::<libc::sockaddr_storage>() as u32;
            // SAFETY: recvfrom into our buffers.
            let n = unsafe { libc::recvfrom(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0, (&mut from as *mut libc::sockaddr_storage).cast(), &mut flen) };
            if n <= 0 {
                continue;
            }
            let mut icmp = &buf[..n as usize];
            if raw && !v6 {
                // Raw IPv4 sockets include the IP header.
                let ihl = usize::from(icmp[0] & 15) * 4;
                icmp = icmp.get(ihl..).unwrap_or(&[]);
            }
            let reply_type = if v6 { 129 } else { 0 };
            if icmp.len() < 8 || icmp[0] != reply_type {
                continue;
            }
            // Datagram sockets rewrite the id; raw ones must match ours.
            if raw && u16::from_be_bytes([icmp[4], icmp[5]]) != id {
                continue;
            }
            let rseq = u16::from_be_bytes([icmp[6], icmp[7]]);
            if rseq != seq {
                continue;
            }
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            println!("{} bytes from {ip}: icmp_seq={rseq} time={ms:.2} ms", icmp.len());
            got += 1;
            rtts.push(ms);
            answered = true;
            break;
        }
        if !answered && !STOP.load(Ordering::SeqCst) && o.count.is_none() {
            println!("no reply for icmp_seq={seq}");
        }
        seq = seq.wrapping_add(1);
        // Keep a steady interval.
        let next = t0 + Duration::from_secs_f64(o.interval);
        while !STOP.load(Ordering::SeqCst) && Instant::now() < next && o.count.map_or(true, |c| sent < c) {
            std::thread::sleep(Duration::from_millis(20).min(next.saturating_duration_since(Instant::now())));
        }
    }
    let loss = if sent == 0 { 0.0 } else { 100.0 * f64::from(sent - got) / f64::from(sent) };
    println!("\n--- {} ping statistics ---", o.host);
    println!("{sent} packets transmitted, {got} received, {loss:.0}% packet loss, time {} ms", start.elapsed().as_millis());
    if !rtts.is_empty() {
        let min = rtts.iter().copied().fold(f64::MAX, f64::min);
        let max = rtts.iter().copied().fold(0.0, f64::max);
        let avg = rtts.iter().sum::<f64>() / rtts.len() as f64;
        println!("rtt min/avg/max = {min:.2}/{avg:.2}/{max:.2} ms");
    }
    i32::from(got == 0)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let o = parse(&v(&["ping", "-c", "3", "example.org"])).unwrap();
        assert_eq!(o.count, Some(3));
        assert_eq!(o.host, "example.org");
        assert!(parse(&v(&["ping"])).is_err());
        assert!(parse(&v(&["ping", "-c"])).is_err());
        assert!(parse(&v(&["ping", "-x", "h"])).is_err());
        let p = echo_request(false, 7, 1, b"ab");
        assert_eq!(crate::packet::checksum(&p), 0);
        assert_eq!(&p[4..8], &[0, 7, 0, 1]);
    }
}
