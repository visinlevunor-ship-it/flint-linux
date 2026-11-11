//! `netd`: the network daemon. Brings interfaces up, runs DHCP on every wired interface with
//! a cable plugged in, and joins saved Wi-Fi networks (see `wifi`). State for `net` goes to
//! /run/net/IFACE; DNS servers go to /run/resolv.conf (/etc/resolv.conf points there).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::dhcp::{self, Reply, Request};
use crate::packet::{self, PacketSocket};
use crate::rtnl::{self, Link, Rtnl};
use crate::wifi;

pub const RUN_DIR: &str = "/run/net";
/// One key=value file per interface, for `net`.
pub const STATUS_DIR: &str = "/run/net/status";
pub const RESOLV: &str = "/run/resolv.conf";
pub const PID_FILE: &str = "/run/netd.pid";

/// Bumped by SIGHUP: `net wifi connect` uses it to make netd re-read saved networks.
pub static RELOAD: AtomicU32 = AtomicU32::new(0);
static RESOLV_LOCK: Mutex<()> = Mutex::new(());

extern "C" fn on_sighup(_: libc::c_int) {
    RELOAD.fetch_add(1, Ordering::SeqCst);
}

pub fn log(msg: &str) {
    eprintln!("netd: {msg}");
}

/// Key=value status for `net`.
pub fn write_status(iface: &str, fields: &[(&str, String)]) {
    let mut s = String::new();
    for (k, v) in fields {
        s.push_str(&format!("{k}={v}\n"));
    }
    let _ = fs::create_dir_all(STATUS_DIR);
    let tmp = format!("{STATUS_DIR}/.{iface}.tmp");
    if fs::write(&tmp, s).is_ok() {
        let _ = fs::rename(&tmp, format!("{STATUS_DIR}/{iface}"));
    }
}

pub fn read_status(iface: &str) -> HashMap<String, String> {
    fs::read_to_string(format!("{STATUS_DIR}/{iface}"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub fn clear_status(iface: &str) {
    let _ = fs::remove_file(format!("{STATUS_DIR}/{iface}"));
    update_resolv();
}

/// /run/resolv.conf from every configured interface, best (lowest metric) first.
pub fn update_resolv() {
    let _g = RESOLV_LOCK.lock();
    let mut entries: Vec<(u32, Vec<String>, Option<String>)> = Vec::new();
    if let Ok(rd) = fs::read_dir(STATUS_DIR) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let st = read_status(&name);
            let metric = st.get("metric").and_then(|m| m.parse().ok()).unwrap_or(u32::MAX);
            let dns: Vec<String> = st.get("dns").map(|d| d.split(' ').filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default();
            entries.push((metric, dns, st.get("domain").cloned()));
        }
    }
    entries.sort_by_key(|e| e.0);
    let mut out = String::from("# Written by netd. Changes are overwritten.\n");
    if let Some(d) = entries.iter().find_map(|e| e.2.clone()) {
        out.push_str(&format!("search {d}\n"));
    }
    let mut seen = Vec::new();
    for (_, dns, _) in &entries {
        for d in dns {
            if !seen.contains(d) && seen.len() < 3 {
                out.push_str(&format!("nameserver {d}\n"));
                seen.push(d.clone());
            }
        }
    }
    let tmp = format!("{RESOLV}.tmp");
    if fs::write(&tmp, out).is_ok() {
        let _ = fs::rename(&tmp, RESOLV);
    }
}

fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_default()
}

fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    let _ = fs::File::open("/dev/urandom").and_then(|mut f| io::Read::read_exact(&mut f, &mut b));
    u32::from_ne_bytes(b) ^ std::process::id().rotate_left(16)
}

/// Sleep up to `d`, waking early if `stop` is set. Returns false if stopped.
pub fn nap(d: Duration, stop: &AtomicBool) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(Duration::from_millis(200).min(end.saturating_duration_since(Instant::now())));
    }
    !stop.load(Ordering::SeqCst)
}

/// One DHCP exchange: send `req` (resending with backoff) until a reply of a wanted kind
/// arrives or `total` passes.
fn exchange(sock: &PacketSocket, req: &Request, wanted: &[u8], total: Duration, stop: &AtomicBool) -> Option<Reply> {
    let start = Instant::now();
    let mut wait = Duration::from_secs(2);
    let packet = packet::ip_udp(req.ciaddr.unwrap_or(Ipv4Addr::UNSPECIFIED), Ipv4Addr::BROADCAST, 68, 67, &dhcp::build(req));
    while start.elapsed() < total && !stop.load(Ordering::SeqCst) {
        if let Err(e) = sock.broadcast(&packet) {
            log(&format!("cannot send DHCP request: {e}"));
        }
        let until = Instant::now() + wait;
        while Instant::now() < until && !stop.load(Ordering::SeqCst) {
            let left = until.saturating_duration_since(Instant::now()).min(Duration::from_millis(500));
            let Ok(Some(p)) = sock.recv(left) else { continue };
            let Some(payload) = packet::udp_payload(&p, 68) else { continue };
            if let Some(r) = dhcp::parse(payload, req.xid, &req.mac) {
                if wanted.contains(&r.kind) {
                    return Some(r);
                }
            }
        }
        wait = (wait * 2).min(Duration::from_secs(16));
    }
    None
}

pub struct Configured {
    pub address: String,
}

fn apply(rt: &mut Rtnl, link: &Link, r: &Reply, metric: u32, extra: &[(&str, String)]) -> io::Result<Configured> {
    let prefix = r.subnet.map_or(24, rtnl::prefix_len);
    let current = rt.addresses()?.into_iter().any(|(i, ip, p)| i == link.index && ip == r.yiaddr && p == prefix);
    if !current {
        rt.flush_addresses(link.index)?;
        rt.add_address(link.index, r.yiaddr, prefix)?;
    }
    rt.delete_default_routes(link.index)?;
    if let Some(gw) = r.routers.first() {
        if let Err(e) = rt.add_default_route(link.index, *gw, metric) {
            log(&format!("{}: cannot add a route through {gw}: {e}", link.name));
        }
    }
    let address = format!("{}/{prefix}", r.yiaddr);
    let mut fields: Vec<(&str, String)> = vec![
        ("state", "up".into()),
        ("address", address.clone()),
        ("gateway", r.routers.first().map(|g| g.to_string()).unwrap_or_default()),
        ("dns", r.dns.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(" ")),
        ("metric", metric.to_string()),
        ("lease", r.lease_secs.unwrap_or(0).to_string()),
    ];
    if let Some(d) = &r.domain {
        fields.push(("domain", d.clone()));
    }
    fields.extend(extra.iter().cloned());
    write_status(&link.name, &fields);
    update_resolv();
    Ok(Configured { address })
}

/// Keep `link` configured by DHCP until `stop` is set. `extra` goes into the status file
/// (the Wi-Fi network name, for example).
pub fn run_dhcp(link: &Link, metric: u32, stop: &AtomicBool, extra: &[(&str, String)]) {
    let Ok(mac) = <[u8; 6]>::try_from(link.mac.as_slice()) else { return };
    let sock = match PacketSocket::open(link.index) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("{}: cannot open a packet socket: {e}", link.name));
            return;
        }
    };
    let Ok(mut rt) = Rtnl::open() else { return };
    let host = hostname();
    let status = |state: &str| {
        let mut f: Vec<(&str, String)> = vec![("state", state.into()), ("metric", metric.to_string())];
        f.extend(extra.iter().cloned());
        write_status(&link.name, &f);
    };
    let mut previous: Option<Ipv4Addr> = None;
    let mut failures = 0u32;
    'acquire: while !stop.load(Ordering::SeqCst) {
        status("configuring");
        let started = Instant::now();
        let xid = random_u32();
        let base = Request { kind: dhcp::DISCOVER, xid, mac, ciaddr: None, requested: previous, server: None, hostname: Some(&host), secs: 0 };
        let Some(offer) = exchange(&sock, &base, &[dhcp::OFFER], Duration::from_secs(30), stop) else {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            failures += 1;
            if failures == 1 {
                log(&format!("{}: no DHCP server answered; still trying", link.name));
            }
            status("no-dhcp");
            nap(Duration::from_secs(10), stop);
            continue;
        };
        let req = Request { kind: dhcp::REQUEST, requested: Some(offer.yiaddr), server: offer.server, secs: started.elapsed().as_secs() as u16, ..base };
        let Some(ack) = exchange(&sock, &req, &[dhcp::ACK, dhcp::NAK], Duration::from_secs(15), stop) else { continue };
        if ack.kind == dhcp::NAK {
            previous = None;
            continue;
        }
        let mut lease = ack;
        loop {
            match apply(&mut rt, link, &lease, metric, extra) {
                Ok(c) => {
                    if previous != Some(lease.yiaddr) || failures > 0 {
                        log(&format!("{}: {} via {}", link.name, c.address, lease.routers.first().map_or("no gateway".into(), |g| g.to_string())));
                    }
                    failures = 0;
                    previous = Some(lease.yiaddr);
                }
                Err(e) => {
                    log(&format!("{}: cannot configure {}: {e}", link.name, lease.yiaddr));
                    nap(Duration::from_secs(5), stop);
                    continue 'acquire;
                }
            }
            let (t1, t2, total) = dhcp::timers(&lease);
            let got = Instant::now();
            if !nap(Duration::from_secs(u64::from(t1)), stop) {
                break 'acquire;
            }
            // Renew, then rebind, until the lease runs out.
            let mut renewed = None;
            while got.elapsed() < Duration::from_secs(u64::from(total)) && !stop.load(Ordering::SeqCst) {
                let r = Request { kind: dhcp::REQUEST, xid: random_u32(), mac, ciaddr: Some(lease.yiaddr), requested: None, server: None, hostname: Some(&host), secs: 0 };
                let budget = if got.elapsed() < Duration::from_secs(u64::from(t2)) { 20 } else { 10 };
                if let Some(a) = exchange(&sock, &r, &[dhcp::ACK, dhcp::NAK], Duration::from_secs(budget), stop) {
                    renewed = Some(a);
                    break;
                }
                nap(Duration::from_secs(30), stop);
            }
            match renewed {
                Some(a) if a.kind == dhcp::ACK => lease = a,
                _ => {
                    log(&format!("{}: lease on {} lost", link.name, lease.yiaddr));
                    let _ = rt.flush_addresses(link.index);
                    previous = None;
                    continue 'acquire;
                }
            }
        }
    }
    let _ = rt.flush_addresses(link.index);
    clear_status(&link.name);
}

struct Worker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

fn stop_worker(w: Worker) {
    w.stop.store(true, Ordering::SeqCst);
    let _ = w.handle.join();
}

/// Lower metrics win: wired before Wi-Fi.
pub fn metric_for(link: &Link) -> u32 {
    if link.is_wireless() {
        600 + link.index
    } else {
        100 + link.index
    }
}

pub fn netd(_args: Vec<String>) -> i32 {
    let _ = fs::create_dir_all(STATUS_DIR);
    let _ = fs::write(PID_FILE, format!("{}\n", std::process::id()));
    // Let every user ping without a setuid binary.
    let _ = fs::write("/proc/sys/net/ipv4/ping_group_range", "0 2147483647");
    // SAFETY: installing a handler that only bumps an atomic.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sighup as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGHUP, &sa, std::ptr::null_mut());
    }
    let (mut rt, events) = match (Rtnl::open(), Rtnl::events()) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            log(&format!("cannot open a netlink socket: {e}"));
            return 1;
        }
    };
    wifi::unblock_rfkill();
    update_resolv();
    let mut wired: HashMap<u32, Worker> = HashMap::new();
    let mut wireless: HashMap<u32, Worker> = HashMap::new();
    let mut last_scan = Instant::now() - Duration::from_secs(60);
    loop {
        // Re-read all links now and then, and whenever an event arrives.
        let got_event = !events.next_links(Some(Duration::from_secs(2))).unwrap_or_default().is_empty();
        if !got_event && last_scan.elapsed() < Duration::from_secs(5) {
            continue;
        }
        last_scan = Instant::now();
        let links = match rt.links() {
            Ok(l) => l,
            Err(e) => {
                log(&format!("cannot list interfaces: {e}"));
                continue;
            }
        };
        for l in &links {
            if l.flags & rtnl::IFF_LOOPBACK != 0 && !l.is_up() {
                let _ = rt.set_up(l.index, true);
            }
        }
        let present: Vec<u32> = links.iter().map(|l| l.index).collect();
        for l in links.iter().filter(|l| l.is_hardware() && l.is_ethernet_like()) {
            if l.is_wireless() {
                if !wireless.contains_key(&l.index) {
                    if !l.is_up() {
                        let _ = rt.set_up(l.index, true);
                    }
                    let stop = Arc::new(AtomicBool::new(false));
                    let (link, s) = (l.clone(), stop.clone());
                    let handle = thread::spawn(move || wifi::manage(&link, &s));
                    wireless.insert(l.index, Worker { stop, handle });
                }
                continue;
            }
            if !l.is_up() {
                if let Err(e) = rt.set_up(l.index, true) {
                    log(&format!("{}: cannot bring up: {e}", l.name));
                }
                continue;
            }
            match (l.has_carrier(), wired.contains_key(&l.index)) {
                (true, false) => {
                    let stop = Arc::new(AtomicBool::new(false));
                    let (link, s, metric) = (l.clone(), stop.clone(), metric_for(l));
                    let handle = thread::spawn(move || run_dhcp(&link, metric, &s, &[]));
                    wired.insert(l.index, Worker { stop, handle });
                }
                (false, true) => {
                    log(&format!("{}: cable unplugged", l.name));
                    if let Some(w) = wired.remove(&l.index) {
                        stop_worker(w);
                    }
                    write_status(&l.name, &[("state", "no-carrier".into())]);
                }
                (false, false) => {
                    if read_status(&l.name).is_empty() {
                        write_status(&l.name, &[("state", "no-carrier".into())]);
                    }
                }
                (true, true) => {}
            }
        }
        // Devices that went away (USB adapters).
        for map in [&mut wired, &mut wireless] {
            let gone: Vec<u32> = map.keys().copied().filter(|i| !present.contains(i)).collect();
            for i in gone {
                if let Some(w) = map.remove(&i) {
                    stop_worker(w);
                }
            }
        }
        let names: Vec<String> = links.iter().map(|l| l.name.clone()).collect();
        if let Ok(rd) = fs::read_dir(STATUS_DIR) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if !n.starts_with('.') && !names.contains(&n) {
                    clear_status(&n);
                }
            }
        }
    }
}
