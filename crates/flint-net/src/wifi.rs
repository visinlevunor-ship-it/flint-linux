//! Wi-Fi for netd: saved networks, scanning, joining the best known network (open or
//! WPA2-Personal), staying connected through group rekeys, and reconnecting when the
//! connection drops.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::crypto::{hex, unhex};
use crate::eapol::{self, Action, Supplicant};
use crate::netd::{self, log, nap, write_status, RELOAD, RUN_DIR};
use crate::nl80211::{self, Bss, Event, Nl80211, Security};
use crate::packet::{PacketSocket, ETH_P_PAE};
use crate::rtnl::{Link, Rtnl};

/// Saved networks. In the image this is a symlink to the boot partition (/boot/FLINT/NETWORKS),
/// so they survive a reboot of the live system.
pub const NETWORKS: &str = "/etc/flint/networks";
/// Used when the boot partition is not writable: kept until the next reboot only.
pub const NETWORKS_VOLATILE: &str = "/run/net/networks";
/// The network `net wifi connect` asked for, by SSID in hex.
pub const WANT: &str = "/run/net/wifi-want";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub ssid: Vec<u8>,
    /// None for an open network.
    pub psk: Option<[u8; 32]>,
}

pub fn parse_networks(text: &str) -> Vec<Saved> {
    let mut out: Vec<Saved> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let Some(ssid) = f.first().and_then(|s| unhex(s)).filter(|s| !s.is_empty() && s.len() <= 32) else { continue };
        let psk = match f.get(1..) {
            Some(["open"]) => None,
            Some(["psk", k]) => match unhex(k).and_then(|k| <[u8; 32]>::try_from(k).ok()) {
                Some(k) => Some(k),
                None => continue,
            },
            _ => continue,
        };
        out.retain(|s| s.ssid != ssid);
        out.push(Saved { ssid, psk });
    }
    out
}

pub fn format_networks(list: &[Saved]) -> String {
    let mut s = String::from("# Saved Wi-Fi networks, written by `net wifi connect`.\n# SSID (hex)   psk KEY (hex, derived from the password)   or   open\n");
    for n in list {
        match &n.psk {
            Some(k) => s.push_str(&format!("{} psk {}\n", hex(&n.ssid), hex(k))),
            None => s.push_str(&format!("{} open\n", hex(&n.ssid))),
        }
    }
    s
}

pub fn load_networks() -> Vec<Saved> {
    let mut all = parse_networks(&fs::read_to_string(NETWORKS).unwrap_or_default());
    for n in parse_networks(&fs::read_to_string(NETWORKS_VOLATILE).unwrap_or_default()) {
        all.retain(|s| s.ssid != n.ssid);
        all.push(n);
    }
    all
}

/// Where NETWORKS really lives (it is normally a symlink onto the boot partition).
fn real_path(p: &Path) -> PathBuf {
    match fs::read_link(p) {
        Ok(t) if t.is_absolute() => t,
        Ok(t) => p.parent().unwrap_or(Path::new("/")).join(t),
        Err(_) => p.to_path_buf(),
    }
}

fn write_durably(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    if !dir.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist (is the boot partition mounted?)", dir.display())));
    }
    let tmp = dir.join(".networks.tmp");
    {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    // Make the rename itself durable: someone may pull the USB stick right after.
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Save the list. Ok(true) if it survives a reboot, Ok(false) if only until reboot.
pub fn save_networks(list: &[Saved]) -> io::Result<bool> {
    let text = format_networks(list);
    match write_durably(&real_path(Path::new(NETWORKS)), &text) {
        Ok(()) => {
            let _ = fs::remove_file(NETWORKS_VOLATILE);
            Ok(true)
        }
        Err(_) => {
            let _ = fs::create_dir_all(RUN_DIR);
            write_durably(Path::new(NETWORKS_VOLATILE), &text)?;
            Ok(false)
        }
    }
}

/// Turn off software radio kill switches (many laptops start with Wi-Fi soft-blocked).
pub fn unblock_rfkill() {
    // struct rfkill_event { u32 idx; u8 type; u8 op; u8 soft; u8 hard; }: CHANGE_ALL, type ALL, unblock
    let ev: [u8; 8] = [0, 0, 0, 0, 0, 3, 0, 0];
    if let Ok(mut f) = fs::OpenOptions::new().write(true).open("/dev/rfkill") {
        let _ = f.write_all(&ev);
    }
}

/// Hardware kill switch (a physical switch or a key the firmware handles) for this interface.
pub fn hard_blocked(iface: &str) -> bool {
    let Ok(rd) = fs::read_dir(format!("/sys/class/net/{iface}/phy80211")) else { return false };
    rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with("rfkill") && fs::read_to_string(e.path().join("hard")).is_ok_and(|h| h.trim() == "1"))
}

pub fn ssid_text(ssid: &[u8]) -> String {
    String::from_utf8_lossy(ssid).chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    if fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_err() {
        // Without a random source the handshake is not safe; this should never happen.
        let t = Instant::now().elapsed().as_nanos().to_le_bytes();
        b[..16].copy_from_slice(&t);
    }
    b
}

/// The network asked for, and the request's id (echoed in the status so `net` can tell
/// its own attempt from an older one).
fn wanted_request() -> Option<(Vec<u8>, String)> {
    let text = fs::read_to_string(WANT).ok()?;
    let mut w = text.split_whitespace();
    let ssid = unhex(w.next()?)?;
    Some((ssid, w.next().unwrap_or("").to_string()))
}

fn wanted() -> Option<Vec<u8>> {
    wanted_request().map(|(s, _)| s)
}

fn set_status(link: &Link, state: &str, ssid: Option<&[u8]>, more: &[(&str, String)]) {
    let mut f: Vec<(&str, String)> = vec![("state", state.into()), ("type", "wifi".into())];
    if let Some(s) = ssid {
        f.push(("ssid", ssid_text(s)));
        f.push(("ssid_hex", hex(s)));
    }
    f.extend(more.iter().cloned());
    write_status(&link.name, &f);
}

/// Wait for activity on either descriptor, up to `timeout`.
fn wait_either(a: i32, b: i32, timeout: Duration) {
    let mut p = [libc::pollfd { fd: a, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: b, events: libc::POLLIN, revents: 0 }];
    // SAFETY: two valid pollfd entries.
    unsafe { libc::poll(p.as_mut_ptr(), 2, timeout.as_millis() as i32) };
}

struct Session {
    nl: Nl80211,
    events: Nl80211,
    eapol: PacketSocket,
    ifindex: u32,
    ap: [u8; 6],
    group_cipher: u32,
    supplicant: Option<Supplicant>,
}

impl Session {
    fn run_actions(&mut self, acts: Vec<Action>) -> io::Result<bool> {
        let mut authorized = false;
        for a in acts {
            match a {
                Action::Send(f) => self.eapol.send_to(&self.ap, &f)?,
                Action::InstallPairwise(tk) => self.nl.new_key(self.ifindex, Some(&self.ap), 0, nl80211::CIPHER_CCMP, &tk, &[])?,
                Action::InstallGroup(idx, key, rsc) => self.nl.new_key(self.ifindex, None, idx, self.group_cipher, &key, &rsc)?,
                Action::Authorize => {
                    self.nl.authorize(self.ifindex, &self.ap)?;
                    authorized = true;
                }
            }
        }
        Ok(authorized)
    }

    /// One look at both sockets. Returns Some(reason) if the connection ended.
    fn poll(&mut self, timeout: Duration) -> io::Result<Option<String>> {
        wait_either(self.events.sock.fd(), self.eapol.raw_fd(), timeout);
        while let Some(ev) = self.events.next_event(Duration::ZERO)? {
            if let Event::Disconnected(i, reason) = ev {
                if i == self.ifindex {
                    return Ok(Some(format!("disconnected (reason {reason})")));
                }
            }
        }
        while let Some((frame, from)) = self.eapol.recv_from(Duration::ZERO)? {
            if from != self.ap {
                continue;
            }
            if let Some(s) = self.supplicant.as_mut() {
                match s.handle(&frame) {
                    Ok(acts) => {
                        self.run_actions(acts)?;
                    }
                    Err(eapol::Error::BadMic) => log("ignored a group key message that did not verify"),
                    Err(_) => {}
                }
            }
        }
        Ok(None)
    }
}

/// Associate and, for WPA2, run the 4-way handshake. Err(message) says why it failed.
fn join(link: &Link, bss: &Bss, psk: Option<[u8; 32]>) -> Result<Session, String> {
    let io = |e: io::Error| e.to_string();
    let mut nl = Nl80211::open().map_err(io)?;
    let events = Nl80211::events().map_err(io)?;
    let eapol = PacketSocket::open_proto(link.index, ETH_P_PAE).map_err(io)?;
    let _ = nl.disconnect(link.index);
    let ie = psk.map(|_| nl80211::rsn_ie(bss.rsn.as_ref().map_or(nl80211::CIPHER_CCMP, |r| r.group)));
    if let Err(e) = nl.connect(link.index, bss, ie.as_deref()) {
        return Err(format!("cannot start connecting: {e}"));
    }
    let mut s = Session {
        nl,
        events,
        eapol,
        ifindex: link.index,
        ap: bss.bssid,
        group_cipher: bss.rsn.as_ref().map_or(nl80211::CIPHER_CCMP, |r| r.group),
        supplicant: None,
    };
    // Association result.
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if Instant::now() > end {
            return Err("the access point did not answer".into());
        }
        match s.events.next_event(Duration::from_millis(500)).map_err(io)? {
            Some(Event::Connected(i, status, mac)) if i == link.index => {
                if status != 0 {
                    return Err(format!("the access point refused the connection (status {status})"));
                }
                if let Some(m) = mac {
                    s.ap = m;
                }
                break;
            }
            Some(Event::Disconnected(i, r)) if i == link.index => return Err(format!("disconnected while connecting (reason {r})")),
            _ => {}
        }
    }
    let Some(pmk) = psk else { return Ok(s) };
    let our_mac: [u8; 6] = link.mac.as_slice().try_into().map_err(|_| "no MAC address".to_string())?;
    s.supplicant = Some(Supplicant::new(pmk, s.ap, our_mac, ie.unwrap_or_default(), random32()));
    let end = Instant::now() + Duration::from_secs(12);
    while Instant::now() < end {
        wait_either(s.events.sock.fd(), s.eapol.raw_fd(), Duration::from_millis(300));
        while let Some(ev) = s.events.next_event(Duration::ZERO).map_err(io)? {
            if let Event::Disconnected(i, reason) = ev {
                if i == link.index {
                    // 15 = 4-way handshake timeout, 2/23 = the AP gave up on our key: a wrong password.
                    let wrong = s.supplicant.as_ref().is_some_and(|x| x.msg1_count > 0);
                    return Err(if wrong { "wrong password (the access point rejected the key)".into() } else { format!("disconnected during the handshake (reason {reason})") });
                }
            }
        }
        while let Some((frame, from)) = s.eapol.recv_from(Duration::ZERO).map_err(io)? {
            if from != s.ap {
                continue;
            }
            let sup = s.supplicant.as_mut().unwrap();
            match sup.handle(&frame) {
                Ok(acts) => {
                    if s.run_actions(acts).map_err(|e| format!("cannot install the keys: {e}"))? {
                        return Ok(s);
                    }
                }
                Err(eapol::Error::BadMic) => return Err("wrong password (the key did not match)".into()),
                Err(_) => {}
            }
            if s.supplicant.as_ref().is_some_and(|x| x.msg1_count >= 3 && !x.complete) {
                return Err("wrong password (the access point rejected the key)".into());
            }
        }
    }
    let _ = s.nl.disconnect(link.index);
    Err("no answer during the handshake (wrong password?)".into())
}

/// Pick the network to join: the one asked for, else the strongest saved one.
pub fn choose<'a>(found: &'a [Bss], saved: &[Saved], want: Option<&[u8]>) -> Option<(&'a Bss, Option<[u8; 32]>)> {
    let usable = |b: &Bss| -> Option<Option<[u8; 32]>> {
        let s = saved.iter().find(|s| s.ssid == b.ssid)?;
        match (b.security, s.psk) {
            (Security::Wpa2, Some(k)) => Some(Some(k)),
            (Security::Open, None) => Some(None),
            _ => None,
        }
    };
    if let Some(w) = want {
        if let Some(b) = found.iter().filter(|b| b.ssid == w).max_by_key(|b| b.signal) {
            if let Some(k) = usable(b) {
                return Some((b, k));
            }
        }
    }
    found.iter().filter_map(|b| usable(b).map(|k| (b, k))).max_by_key(|(b, _)| b.signal)
}

pub fn manage(link: &Link, stop: &AtomicBool) {
    let mut failures: Vec<(Vec<u8>, Instant)> = Vec::new();
    let mut reported_no_radio = false;
    while !stop.load(Ordering::SeqCst) {
        if hard_blocked(&link.name) {
            if !reported_no_radio {
                log(&format!("{}: Wi-Fi is switched off by a hardware switch or key", link.name));
                reported_no_radio = true;
            }
            set_status(link, "radio-off", None, &[]);
            nap(Duration::from_secs(3), stop);
            continue;
        }
        let reload = RELOAD.load(Ordering::SeqCst);
        let (mut nl, events) = match (Nl80211::open(), Nl80211::events()) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                set_status(link, "error", None, &[("error", e.to_string())]);
                nap(Duration::from_secs(5), stop);
                continue;
            }
        };
        // Only client interfaces are ours: an access point (hostapd) or monitor interface is not.
        if let Ok(t) = nl.iftype(link.index) {
            if t != nl80211::IFTYPE_STATION {
                set_status(link, "not-managed", None, &[("error", format!("interface type {t} is not a client"))]);
                wait_reload(reload, Duration::from_secs(10), stop);
                continue;
            }
        }
        let saved = load_networks();
        if saved.is_empty() {
            set_status(link, "no-saved-networks", None, &[]);
            wait_reload(reload, Duration::from_secs(30), stop);
            continue;
        }
        let _ = Rtnl::open().and_then(|mut r| r.set_up(link.index, true));
        set_status(link, "scanning", None, &[]);
        let found = match nl.scan(&events, link.index) {
            Ok(f) => f,
            Err(e) => {
                set_status(link, "error", None, &[("error", format!("scan failed: {e}"))]);
                nap(Duration::from_secs(5), stop);
                continue;
            }
        };
        // Networks that just failed (say, a wrong password) wait a minute, unless asked for again.
        failures.retain(|(_, t)| t.elapsed() < Duration::from_secs(60));
        let want = wanted();
        let candidates: Vec<Bss> = found.iter().filter(|b| want.as_deref() == Some(&b.ssid[..]) || !failures.iter().any(|(s, _)| *s == b.ssid)).cloned().collect();
        let Some((bss, psk)) = choose(&candidates, &saved, want.as_deref()) else {
            let msg = match &want {
                Some(w) if !found.iter().any(|b| &b.ssid == w) => format!("{} is not in range", ssid_text(w)),
                _ => "no saved network in range".into(),
            };
            let mut more: Vec<(&str, String)> = wanted_request().map(|(_, id)| vec![("request", id)]).unwrap_or_default();
            more.push(("error", msg));
            set_status(link, "searching", want.as_deref(), &more);
            wait_reload(reload, Duration::from_secs(20), stop);
            continue;
        };
        let bss = bss.clone();
        let request: Vec<(&str, String)> = match wanted_request() {
            Some((w, id)) if w == bss.ssid => vec![("request", id)],
            _ => Vec::new(),
        };
        set_status(link, "connecting", Some(&bss.ssid), &request);
        let mut session = match join(link, &bss, psk) {
            Ok(s) => s,
            Err(e) => {
                log(&format!("{}: {}: {e}", link.name, ssid_text(&bss.ssid)));
                let mut more = request.clone();
                more.push(("error", e));
                set_status(link, "failed", Some(&bss.ssid), &more);
                failures.push((bss.ssid.clone(), Instant::now()));
                if want.as_deref() == Some(&bss.ssid[..]) {
                    let _ = fs::remove_file(WANT);
                }
                wait_reload(reload, Duration::from_secs(5), stop);
                continue;
            }
        };
        log(&format!("{}: connected to {}", link.name, ssid_text(&bss.ssid)));
        let mut extra = vec![("type", "wifi".to_string()), ("ssid", ssid_text(&bss.ssid)), ("ssid_hex", hex(&bss.ssid)), ("signal", format!("{} dBm", bss.signal)), ("bssid", crate::rtnl::mac_text(&bss.bssid))];
        extra.extend(request.iter().cloned());
        let conn_stop = Arc::new(AtomicBool::new(false));
        let dhcp = {
            let (l, s, x, m) = (link.clone(), conn_stop.clone(), extra.clone(), netd::metric_for(link));
            thread::spawn(move || {
                let x: Vec<(&str, String)> = x.iter().map(|(k, v)| (*k, v.clone())).collect();
                netd::run_dhcp(&l, m, &s, &x)
            })
        };
        let mut seen_reload = RELOAD.load(Ordering::SeqCst);
        let reason = loop {
            if stop.load(Ordering::SeqCst) {
                let _ = session.nl.disconnect(link.index);
                break "stopping".to_string();
            }
            let r = RELOAD.load(Ordering::SeqCst);
            if r != seen_reload {
                seen_reload = r;
                let still_saved = load_networks().iter().any(|s| s.ssid == bss.ssid);
                let other_wanted = wanted().is_some_and(|w| w != bss.ssid);
                if !still_saved || other_wanted {
                    let _ = session.nl.disconnect(link.index);
                    break "switching networks".to_string();
                }
            }
            match session.poll(Duration::from_secs(1)) {
                Ok(Some(r)) => break r,
                Ok(None) => {}
                Err(e) => break format!("error: {e}"),
            }
        };
        log(&format!("{}: {} ({reason})", link.name, ssid_text(&bss.ssid)));
        conn_stop.store(true, Ordering::SeqCst);
        let _ = dhcp.join();
        if let Ok(mut r) = Rtnl::open() {
            let _ = r.flush_addresses(link.index);
        }
        nap(Duration::from_secs(1), stop);
    }
    if let Ok(mut nl) = Nl80211::open() {
        let _ = nl.disconnect(link.index);
    }
    netd::clear_status(&link.name);
}

/// Sleep until SIGHUP (RELOAD changes), `d` passes, or `stop`.
fn wait_reload(seen: u32, d: Duration, stop: &AtomicBool) {
    let end = Instant::now() + d;
    while Instant::now() < end && !stop.load(Ordering::SeqCst) && RELOAD.load(Ordering::SeqCst) == seen {
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bss(ssid: &str, signal: i32, security: Security) -> Bss {
        Bss { bssid: [0; 6], freq: 2412, signal, ssid: ssid.as_bytes().to_vec(), rsn: None, security, associated: false }
    }

    #[test]
    fn networks_file() {
        let list = vec![Saved { ssid: b"home wifi".to_vec(), psk: Some([0xAB; 32]) }, Saved { ssid: b"cafe".to_vec(), psk: None }];
        let text = format_networks(&list);
        assert_eq!(parse_networks(&text), list);
        let messy = format!("{text}\nzz psk 00\n{} psk 1234\n{} open\n", hex(b"x"), hex(b"cafe"));
        assert_eq!(parse_networks(&messy), list, "bad lines skipped, duplicates keep the last");
    }

    #[test]
    fn choosing_a_network() {
        let found = vec![bss("cafe", -40, Security::Open), bss("home", -70, Security::Wpa2), bss("home", -60, Security::Wpa2), bss("work", -30, Security::Enterprise)];
        let saved = vec![Saved { ssid: b"home".to_vec(), psk: Some([1; 32]) }, Saved { ssid: b"cafe".to_vec(), psk: None }, Saved { ssid: b"work".to_vec(), psk: Some([2; 32]) }];
        let (b, k) = choose(&found, &saved, None).unwrap();
        assert_eq!((b.ssid.as_slice(), b.signal, k), (&b"cafe"[..], -40, None), "strongest usable");
        let (b, k) = choose(&found, &saved, Some(b"home")).unwrap();
        assert_eq!((b.ssid.as_slice(), b.signal, k), (&b"home"[..], -60, Some([1; 32])), "asked for, strongest access point of it");
        assert!(choose(&found[3..], &saved, None).is_none(), "enterprise is not usable");
        assert!(choose(&found, &[], None).is_none());
    }
}
