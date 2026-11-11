//! `net`: network status and Wi-Fi.

use std::fs;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::crypto::{hex, wpa_psk};
use crate::netd::{read_status, PID_FILE, RUN_DIR};
use crate::nl80211::{Nl80211, Security};
use crate::rtnl::{self, Rtnl};
use crate::wifi::{self, load_networks, save_networks, ssid_text, Saved, WANT};

const USAGE: &str = "usage: net                           show interfaces, addresses and Wi-Fi
       net wifi scan                 list Wi-Fi networks in range
       net wifi connect NAME [PASSWORD]
                                     join a network and save it (asks for the password)
       net wifi list                 saved networks
       net wifi forget NAME          remove a saved network";

pub fn net(args: Vec<String>) -> i32 {
    let a: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let r = match a.as_slice() {
        [] | ["status"] => status(),
        ["wifi", "scan"] => scan(),
        ["wifi", "connect", name] => connect(name, None),
        ["wifi", "connect", name, pass] => connect(name, Some(pass)),
        ["wifi", "list"] => list(),
        ["wifi", "forget", name] => forget(name),
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            return 0;
        }
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("net: {e}");
            1
        }
    }
}

fn status() -> io::Result<()> {
    let mut rt = Rtnl::open()?;
    let links = rt.links()?;
    let addrs = rt.addresses()?;
    let routes = rt.default_routes().unwrap_or_default();
    let mut any = false;
    for l in links.iter().filter(|l| l.flags & rtnl::IFF_LOOPBACK == 0) {
        any = true;
        let st = read_status(&l.name);
        let kind = if l.is_wireless() { "wifi" } else if l.is_hardware() { "wired" } else { "virtual" };
        let state = st.get("state").cloned().unwrap_or_else(|| if !l.is_up() { "down".into() } else if l.has_carrier() { "up".into() } else { "no-carrier".into() });
        let shown = match state.as_str() {
            "no-carrier" => "no cable".to_string(),
            "no-saved-networks" => "not connected (no saved networks: net wifi connect NAME)".into(),
            "radio-off" => "Wi-Fi switched off (hardware switch or key)".into(),
            "configuring" => "getting an address (DHCP)".into(),
            "not-managed" => "not managed by netd (access point or monitor mode)".into(),
            "no-dhcp" => "no DHCP server answered".into(),
            s => s.to_string(),
        };
        println!("{:<10} {:<6} {}", l.name, kind, shown);
        if let Some(s) = st.get("ssid") {
            let signal = st.get("signal").map(|s| format!(", {s}")).unwrap_or_default();
            println!("           network  {s}{signal}");
        }
        if let Some(e) = st.get("error") {
            println!("           problem  {e}");
        }
        for (_, ip, p) in addrs.iter().filter(|(i, _, _)| *i == l.index) {
            println!("           address  {ip}/{p}");
        }
        for (_, gw, _) in routes.iter().filter(|(i, _, _)| *i == l.index) {
            println!("           gateway  {gw}");
        }
        if let Some(d) = st.get("dns").filter(|d| !d.is_empty()) {
            println!("           dns      {d}");
        }
        println!("           mac      {}", rtnl::mac_text(&l.mac));
    }
    if !any {
        println!("No network interfaces found. (A driver may be missing: see dmesg and lsmod.)");
    }
    if fs::metadata(PID_FILE).is_err() {
        println!("\nnetd is not running, so nothing is configured automatically.");
    }
    Ok(())
}

fn wireless_link() -> io::Result<rtnl::Link> {
    let mut rt = Rtnl::open()?;
    let links = rt.links()?;
    links.into_iter().find(|l| l.is_wireless()).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no Wi-Fi interface (no Wi-Fi card, or its driver or firmware is missing: see dmesg)"))
}

fn scan() -> io::Result<()> {
    let link = wireless_link()?;
    if wifi::hard_blocked(&link.name) {
        return Err(io::Error::new(io::ErrorKind::Other, "Wi-Fi is switched off by a hardware switch or key"));
    }
    wifi::unblock_rfkill();
    let mut rt = Rtnl::open()?;
    let _ = rt.set_up(link.index, true);
    let mut nl = Nl80211::open()?;
    let events = Nl80211::events()?;
    let found = nl.scan(&events, link.index).map_err(|e| if e.raw_os_error() == Some(libc::EPERM) { io::Error::new(e.kind(), "scanning needs root (try: su)") } else { e })?;
    let saved = load_networks();
    // One line per network name: the strongest access point.
    let mut seen: Vec<Vec<u8>> = Vec::new();
    println!("{:<32}  {:>7}  {:<5}  security", "network", "signal", "saved");
    for b in &found {
        if b.ssid.is_empty() || b.ssid.iter().all(|&c| c == 0) || seen.contains(&b.ssid) {
            continue;
        }
        seen.push(b.ssid.clone());
        let is_saved = saved.iter().any(|s| s.ssid == b.ssid);
        let bars = match b.signal {
            s if s >= -55 => "****",
            s if s >= -67 => "***",
            s if s >= -75 => "**",
            _ => "*",
        };
        println!("{:<32}  {:>3} {:<4}  {:<5}  {}", ssid_text(&b.ssid), b.signal, bars, if is_saved { "yes" } else { "" }, b.security.name());
    }
    if seen.is_empty() {
        println!("(no networks found)");
    }
    Ok(())
}

/// Read a password from the terminal without echoing it.
fn read_password(prompt: &str) -> io::Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed then filled termios.
    let saved = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            let old = t;
            t.c_lflag &= !libc::ECHO;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            Some(old)
        } else {
            None
        }
    };
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    let r = loop {
        match io::stdin().read(&mut b) {
            Ok(0) => break Ok(()),
            Ok(_) if b[0] == b'\n' => break Ok(()),
            Ok(_) => line.push(b[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e),
        }
    };
    if let Some(old) = saved {
        // SAFETY: restoring the settings read above.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &old) };
        println!();
    }
    r?;
    Ok(String::from_utf8_lossy(&line).trim_end_matches('\r').to_string())
}

fn signal_netd() -> io::Result<()> {
    let pid: i32 = fs::read_to_string(PID_FILE)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "netd is not running"))?;
    // SAFETY: kill with a pid and SIGHUP.
    if unsafe { libc::kill(pid, libc::SIGHUP) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn connect(name: &str, pass: Option<&str>) -> io::Result<()> {
    // SAFETY: plain query.
    if unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "joining a network needs root (try: su)"));
    }
    let ssid = name.as_bytes().to_vec();
    if ssid.is_empty() || ssid.len() > 32 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a network name is 1 to 32 bytes"));
    }
    let link = wireless_link()?;
    wifi::unblock_rfkill();
    let mut rt = Rtnl::open()?;
    let _ = rt.set_up(link.index, true);
    // Find out what security the network uses.
    let mut nl = Nl80211::open()?;
    let events = Nl80211::events()?;
    let found = nl.scan(&events, link.index)?;
    let Some(bss) = found.iter().find(|b| b.ssid == ssid) else {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no network called \"{name}\" in range (see: net wifi scan)")));
    };
    let psk = match bss.security {
        Security::Open => None,
        Security::Wpa2 => {
            let saved = load_networks().into_iter().find(|s| s.ssid == ssid).and_then(|s| s.psk);
            let pass = match (pass, saved) {
                (Some(p), _) => p.to_string(),
                (None, Some(_)) => String::new(),
                (None, None) => read_password(&format!("Password for {name}: "))?,
            };
            if pass.is_empty() {
                saved
            } else if pass.len() == 64 && pass.bytes().all(|b| b.is_ascii_hexdigit()) {
                crate::crypto::unhex(&pass).and_then(|k| k.try_into().ok())
            } else if !(8..=63).contains(&pass.len()) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "a WPA2 password is 8 to 63 characters"));
            } else {
                Some(wpa_psk(pass.as_bytes(), &ssid))
            }
        }
        other => return Err(io::Error::new(io::ErrorKind::Unsupported, format!("\"{name}\" uses {}", other.name()))),
    };
    let mut list = load_networks();
    let previous = list.clone();
    list.retain(|s| s.ssid != ssid);
    list.push(Saved { ssid: ssid.clone(), psk });
    let durable = save_networks(&list)?;
    fs::create_dir_all(RUN_DIR)?;
    let request = format!("{}-{}", std::process::id(), Instant::now().elapsed().as_nanos() ^ u128::from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0)));
    fs::write(WANT, format!("{} {request}\n", hex(&ssid)))?;
    signal_netd()?;
    println!("Connecting to {name}...");
    let end = Instant::now() + Duration::from_secs(60);
    let mut last = String::new();
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(300));
        let st = read_status(&link.name);
        if st.get("ssid_hex").map(String::as_str) != Some(&hex(&ssid)) || st.get("request") != Some(&request) {
            continue;
        }
        let state = st.get("state").cloned().unwrap_or_default();
        if state != last {
            last = state.clone();
            match state.as_str() {
                "configuring" => println!("Connected, getting an address..."),
                "up" => {
                    println!("Connected to {name}: {}", st.get("address").cloned().unwrap_or_default());
                    if !durable {
                        println!("(saved until reboot only: the boot partition is not writable)");
                    }
                    return Ok(());
                }
                "failed" => {
                    // A network that does not work is not kept, unless it was saved before.
                    if !previous.iter().any(|s| s.ssid == ssid) {
                        let _ = save_networks(&previous);
                    }
                    let _ = fs::remove_file(WANT);
                    return Err(io::Error::new(io::ErrorKind::Other, format!("{name}: {}", st.get("error").cloned().unwrap_or_else(|| "failed".into()))));
                }
                "no-dhcp" => println!("Connected, but no DHCP server answered yet; still trying."),
                "searching" => println!("Not found by netd yet, still looking..."),
                _ => {}
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, format!("{name}: still not connected after a minute (netd keeps trying; see: net)")))
}

fn list() -> io::Result<()> {
    let l = load_networks();
    if l.is_empty() {
        println!("No saved networks. Add one with: net wifi connect NAME");
    }
    for n in l {
        println!("{}  ({})", ssid_text(&n.ssid), if n.psk.is_some() { "WPA2" } else { "open" });
    }
    Ok(())
}

fn forget(name: &str) -> io::Result<()> {
    let mut l = load_networks();
    let before = l.len();
    l.retain(|s| s.ssid != name.as_bytes());
    if l.len() == before {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no saved network called \"{name}\"")));
    }
    save_networks(&l)?;
    let _ = signal_netd();
    println!("Forgot {name}.");
    Ok(())
}
