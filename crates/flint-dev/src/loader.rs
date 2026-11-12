//! Loading kernel modules with their dependencies, and coldplug: loading the drivers for
//! every device that is already present.

use std::collections::{BTreeSet, HashSet};
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::index::{normalize, Index};

/// finit_module flag: the kernel decompresses the file itself (CONFIG_MODULE_DECOMPRESS).
const MODULE_INIT_COMPRESSED_FILE: libc::c_uint = 4;

pub struct Loader<'a> {
    pub idx: &'a Index,
    pub blacklist: HashSet<String>,
    pub verbose: bool,
    /// Modules this run loaded, in order.
    pub loaded: Vec<String>,
    tried: HashSet<String>,
}

/// Modules listed in a blacklist file (one per line, `#` comments).
pub fn read_blacklist(path: &Path) -> HashSet<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(normalize)
        .collect()
}

fn present(name: &str) -> bool {
    Path::new("/sys/module").join(name).exists()
}

fn insmod(path: &Path, params: &str) -> io::Result<()> {
    let f = fs::File::open(path)?;
    let p = CString::new(params).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in module parameters"))?;
    let name = path.to_string_lossy();
    let flags = if name.ends_with(".zst") || name.ends_with(".xz") || name.ends_with(".gz") { MODULE_INIT_COMPRESSED_FILE } else { 0 };
    // SAFETY: finit_module takes a valid fd, a NUL-terminated parameter string and flags.
    let rc = unsafe { libc::syscall(libc::SYS_finit_module, f.as_raw_fd(), p.as_ptr(), flags) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // A kernel without in-kernel decompression (or for another format) cannot take a
    // compressed file: gzip ones are unpacked here and handed over as plain data.
    let refused = matches!(err.raw_os_error(), Some(libc::EOPNOTSUPP | libc::EINVAL | libc::ENOEXEC));
    if refused && name.ends_with(".gz") {
        let data = fs::read(path)?;
        let plain = flint_gzip::decompress(&data, 1 << 30).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{name}: {e}")))?;
        // SAFETY: init_module takes the module image, its length and the parameter string.
        let rc = unsafe { libc::syscall(libc::SYS_init_module, plain.as_ptr(), plain.len(), p.as_ptr()) };
        return if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) };
    }
    Err(err)
}

impl<'a> Loader<'a> {
    pub fn new(idx: &'a Index) -> Self {
        Loader { idx, blacklist: HashSet::new(), verbose: false, loaded: Vec::new(), tried: HashSet::new() }
    }

    /// Load `name` and everything it needs. Ok(true) if something was loaded, Ok(false) if it
    /// was already there (or built in).
    pub fn load(&mut self, name: &str, params: &str) -> Result<bool, String> {
        let name = normalize(name);
        if self.idx.builtin.contains(&name) || present(&name) {
            return Ok(false);
        }
        if !self.tried.insert(name.clone()) {
            return Ok(false); // already tried this run (or a dependency cycle)
        }
        let Some(rel) = self.idx.path.get(&name).cloned() else {
            return Err(format!("module {name} not found"));
        };
        for pre in self.idx.softdep_pre.get(&name).cloned().unwrap_or_default() {
            let _ = self.load(&pre, "");
        }
        // modules.dep lists every module this one needs, in any order: load each one's own
        // dependencies first, so the order works out.
        for dep in self.idx.deps.get(&name).cloned().unwrap_or_default() {
            self.load(&dep, "").map_err(|e| format!("{name} needs {dep}: {e}"))?;
        }
        match insmod(&self.idx.dir.join(&rel), params) {
            Ok(()) => {
                if self.verbose {
                    eprintln!("modload: loaded {name}");
                }
                self.loaded.push(name.clone());
            }
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {}
            Err(e) => return Err(format!("{name}: {e}")),
        }
        for post in self.idx.softdep_post.get(&name).cloned().unwrap_or_default() {
            let _ = self.load(&post, "");
        }
        Ok(true)
    }

    /// Load every module for one device alias. Failures are normal (a driver that finds no
    /// device it can drive refuses to load), so they are only reported when verbose.
    pub fn load_for_alias(&mut self, modalias: &str) {
        for m in self.idx.resolve(modalias) {
            if self.blacklist.contains(&m) {
                continue;
            }
            if let Err(e) = self.load(&m, "") {
                if self.verbose {
                    eprintln!("modload: {e}");
                }
            }
        }
    }
}

/// Every `modalias` file under /sys/devices.
pub fn device_aliases(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() && e.file_name() == "modalias" {
                if let Ok(s) = fs::read_to_string(e.path()) {
                    let s = s.trim();
                    if !s.is_empty() {
                        out.insert(s.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Load drivers for all present devices, again and again while loading them makes new
/// devices appear (USB devices behind a new controller, HID behind usbhid, ...). Stops when
/// nothing new shows up for `settle`, or at `deadline`.
pub fn coldplug(loader: &mut Loader, sys_devices: &Path, settle: Duration, deadline: Duration) -> usize {
    let start = Instant::now();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut quiet_since = Instant::now();
    loop {
        let now = device_aliases(sys_devices);
        let fresh: Vec<String> = now.difference(&seen).cloned().collect();
        if fresh.is_empty() {
            if quiet_since.elapsed() >= settle || start.elapsed() >= deadline {
                break;
            }
        } else {
            for a in &fresh {
                loader.load_for_alias(a);
            }
            seen.extend(fresh);
            quiet_since = Instant::now();
        }
        if start.elapsed() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    seen.len()
}

pub fn unload(name: &str) -> io::Result<()> {
    let n = CString::new(normalize(name)).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad name"))?;
    // SAFETY: delete_module takes a NUL-terminated name and flags.
    let rc = unsafe { libc::syscall(libc::SYS_delete_module, n.as_ptr(), libc::O_NONBLOCK) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_modalias_files() {
        let d = std::env::temp_dir().join(format!("flint-dev-sys-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("pci0000:00/0000:00:14.0/usb1/1-1")).unwrap();
        fs::write(d.join("pci0000:00/0000:00:14.0/modalias"), "pci:v00008086d0000A0EDsv*\n").unwrap();
        fs::write(d.join("pci0000:00/0000:00:14.0/usb1/1-1/modalias"), "usb:v046Dp0825\n").unwrap();
        fs::write(d.join("pci0000:00/modalias"), "\n").unwrap();
        let a = device_aliases(&d);
        assert_eq!(a.into_iter().collect::<Vec<_>>(), ["pci:v00008086d0000A0EDsv*", "usb:v046Dp0825"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn blacklist_file() {
        let p = std::env::temp_dir().join(format!("flint-dev-bl-{}", std::process::id()));
        fs::write(&p, "# no beeps\npcspkr\nsnd-pcsp  # dash form\n\n").unwrap();
        let b = read_blacklist(&p);
        assert!(b.contains("pcspkr") && b.contains("snd_pcsp") && b.len() == 2);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn unknown_module_is_an_error_not_a_panic() {
        let idx = Index::default();
        let mut l = Loader::new(&idx);
        assert!(l.load("definitely_not_a_module", "").unwrap_err().contains("not found"));
    }
}
