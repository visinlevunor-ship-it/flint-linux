//! Hardware support: `modload` (load drivers, by name or for every present device),
//! `devd` (load drivers for devices that appear later) and `lsmod`.

pub mod bootpart;
pub mod index;
pub mod loader;
pub mod pattern;
pub mod persist;
pub mod uevent;

use std::fs;
use std::path::Path;
use std::time::Duration;

use index::Index;
use loader::{coldplug, read_blacklist, Loader};

pub use bootpart::mountboot;

const BLACKLIST: &str = "/etc/flint/modules.blacklist";

const MODLOAD_USAGE: &str = "usage: modload MODULE [PARAM=VALUE...]   load a module and what it needs
       modload -r MODULE...                   unload modules
       modload --coldplug [--wait SECONDS] [-v]   load drivers for every device present
       modload --attach DIR [--coldplug ...]  use the drivers in DIR (modules/, firmware/)
                                              from now on, instead of the ones in RAM
       modload --alias MODALIAS               show which modules match a device alias";

fn index_or_complain(prog: &str) -> Option<Index> {
    match Index::running() {
        Ok(i) => Some(i),
        Err(e) => {
            eprintln!("{prog}: cannot read the module index in /lib/modules: {e}");
            None
        }
    }
}

/// Use the drivers kept on the boot partition: bind DIR/modules over /lib/modules and
/// DIR/firmware over /lib/firmware. Only the early drivers (what reaches the boot
/// partition) are in RAM; everything else is read from the disk when a device needs it.
pub fn attach(dir: &Path) -> Result<String, String> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).map_err(|e| e.to_string())?;
    let mods = dir.join("modules");
    if !mods.join(&release).join("modules.dep").is_file() {
        return Err(format!("no drivers for kernel {release} in {}", mods.display()));
    }
    let bind = |from: &Path, to: &str| -> Result<(), String> {
        std::fs::create_dir_all(to).map_err(|e| format!("{to}: {e}"))?;
        flint_core::sys::mount(&from.to_string_lossy(), to, "", libc::MS_BIND, None).map_err(|e| format!("bind {} on {to}: {e}", from.display()))?;
        // read-only: the boot partition is not changed through these paths
        let _ = flint_core::sys::mount("", to, "", libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY, None);
        Ok(())
    };
    bind(&mods, "/lib/modules")?;
    let fw = dir.join("firmware");
    if fw.is_dir() {
        bind(&fw, "/lib/firmware")?;
    }
    Ok(format!("drivers and firmware from {}", dir.display()))
}

/// `modload`: modprobe for Flint.
pub fn modload(args: Vec<String>) -> i32 {
    let rest: Vec<String> = args.into_iter().skip(1).collect();
    let verbose = rest.iter().any(|a| a == "-v");
    let mut rest: Vec<String> = rest.into_iter().filter(|a| a != "-v").collect();
    if rest.first().map(String::as_str) == Some("--attach") {
        let Some(dir) = rest.get(1).cloned() else {
            eprintln!("{MODLOAD_USAGE}");
            return 2;
        };
        match attach(Path::new(&dir)) {
            Ok(m) => eprintln!("modload: {m}"),
            Err(e) => eprintln!("modload: {e}; using the drivers in RAM"),
        }
        rest.drain(..2);
        if rest.is_empty() {
            return 0;
        }
    }
    match rest.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            println!("{MODLOAD_USAGE}");
            i32::from(rest.is_empty()) * 2
        }
        Some("--coldplug") => {
            let wait = match rest.get(1).map(String::as_str) {
                Some("--wait") => rest.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(10),
                _ => 10,
            };
            let Some(idx) = index_or_complain("modload") else { return 1 };
            let mut l = Loader::new(&idx);
            l.verbose = verbose;
            l.blacklist = read_blacklist(Path::new(BLACKLIST));
            let devices = coldplug(&mut l, Path::new("/sys/devices"), Duration::from_millis(1500), Duration::from_secs(wait));
            eprintln!("modload: {devices} devices, {} drivers loaded", l.loaded.len());
            0
        }
        Some("--alias") => {
            let Some(a) = rest.get(1) else {
                eprintln!("{MODLOAD_USAGE}");
                return 2;
            };
            let Some(idx) = index_or_complain("modload") else { return 1 };
            let found = idx.resolve(a);
            for m in &found {
                println!("{m}");
            }
            i32::from(found.is_empty())
        }
        Some("-r") => {
            let mut status = 0;
            for m in &rest[1..] {
                if let Err(e) = loader::unload(m) {
                    eprintln!("modload: cannot unload {m}: {e}");
                    status = 1;
                }
            }
            status
        }
        Some(m) if m.starts_with('-') => {
            eprintln!("modload: unknown option {m}\n{MODLOAD_USAGE}");
            2
        }
        Some(m) => {
            let Some(idx) = index_or_complain("modload") else { return 1 };
            let mut l = Loader::new(&idx);
            l.verbose = verbose;
            match l.load(m, &rest[1..].join(" ")) {
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("modload: {e}");
                    1
                }
            }
        }
    }
}

/// `modprobe`: what the kernel runs when it needs a module it does not have
/// (`modprobe -q -- fs-vfat`, `crypto-ccm(aes)`, `net-pf-17`, ...). The name is a module
/// name or an alias. Only the options the kernel and common scripts use are accepted.
pub fn modprobe(args: Vec<String>) -> i32 {
    let mut quiet = false;
    let mut honour_blacklist = false;
    let mut verbose = false;
    let mut rest = Vec::new();
    let mut opts_done = false;
    for a in args.into_iter().skip(1) {
        if opts_done || !a.starts_with('-') {
            opts_done = true;
            rest.push(a);
            continue;
        }
        match a.as_str() {
            "--" => opts_done = true,
            "-q" | "--quiet" => quiet = true,
            "-b" | "--use-blacklist" => honour_blacklist = true,
            "-v" | "--verbose" => verbose = true,
            _ => {
                eprintln!("modprobe: unsupported option {a} (use modload for anything but loading)");
                return 2;
            }
        }
    }
    let Some(name) = rest.first() else {
        eprintln!("usage: modprobe [-q] [-b] [-v] [--] NAME [PARAM=VALUE...]");
        return 2;
    };
    let Some(idx) = index_or_complain("modprobe") else { return 1 };
    let mut l = Loader::new(&idx);
    l.verbose = verbose;
    let blacklist = if honour_blacklist { read_blacklist(Path::new(BLACKLIST)) } else { Default::default() };
    let norm = index::normalize(name);
    let candidates = if idx.path.contains_key(&norm) || idx.builtin.contains(&norm) { vec![norm] } else { idx.resolve(name) };
    if candidates.is_empty() {
        if !quiet {
            eprintln!("modprobe: no module named or aliased {name}");
        }
        return 1;
    }
    let params = rest[1..].join(" ");
    let mut ok = false;
    for m in candidates.iter().filter(|m| !blacklist.contains(*m)) {
        match l.load(m, &params) {
            Ok(_) => ok = true,
            Err(e) if !quiet => eprintln!("modprobe: {e}"),
            Err(_) => {}
        }
    }
    i32::from(!ok)
}

/// `devd`: load drivers for devices as they appear. Meant to run as a service.
pub fn devd(args: Vec<String>) -> i32 {
    let verbose = args.iter().skip(1).any(|a| a == "-v");
    let sock = match uevent::Socket::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("devd: cannot listen for device events: {e}");
            return 1;
        }
    };
    let Some(idx) = index_or_complain("devd") else { return 1 };
    let mut l = Loader::new(&idx);
    l.blacklist = read_blacklist(Path::new(BLACKLIST));
    // Catch up with anything that appeared before the socket was open.
    coldplug(&mut l, Path::new("/sys/devices"), Duration::from_millis(500), Duration::from_secs(5));
    loop {
        let ev = match sock.recv() {
            Ok(ev) => ev,
            Err(e) => {
                eprintln!("devd: {e}");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        if ev.get("ACTION").map(String::as_str) != Some("add") {
            continue;
        }
        if let Some(alias) = ev.get("MODALIAS") {
            let before = l.loaded.len();
            // A module that failed earlier may work now (firmware arrived, a dependency
            // appeared): let it be tried again for a new device.
            let mut fresh = Loader::new(&idx);
            fresh.blacklist = l.blacklist.clone();
            fresh.load_for_alias(alias);
            l.loaded.extend(fresh.loaded.iter().cloned());
            if verbose {
                for m in &l.loaded[before..] {
                    eprintln!("devd: loaded {m} for {alias}");
                }
            }
        }
    }
}

/// `lsmod`: the loaded modules.
pub fn lsmod(_args: Vec<String>) -> i32 {
    let text = match fs::read_to_string("/proc/modules") {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lsmod: /proc/modules: {e}");
            return 1;
        }
    };
    println!("{:<24} {:>8}  Used by", "Module", "Size");
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 4 {
            let users = f[3].trim_end_matches(',');
            let users = if users == "-" { "" } else { users };
            println!("{:<24} {:>8}  {} {}", f[0], f[1], f[2], users);
        }
    }
    0
}
