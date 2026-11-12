//! Kernels as packages: `pkg kernels list | install FLAVOR | remove FLAVOR | default FLAVOR`.
//!
//! A kernel package is named `linux-FLAVOR` and holds `usr/lib/flint/kernels/FLAVOR/`:
//! `KERNEL` (the kernel image), `MODULES.IMG` (its drivers, as built by flint-mkimage) and
//! `RELEASE` (what `uname -r` will say). After every install, upgrade or removal, the
//! installed kernels are copied to the boot partition (`/boot/FLINT/KERNELS/FLAVOR/`) and
//! the boot menu gets one entry for each. The kernel the system came with is "generic"
//! (`/boot/FLINT/KERNEL`), always there.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const BUILTIN: &str = "generic";
const MARK: &str = "# ---- kernels added by pkg (pkg kernels); this part is rewritten ----";
const FILES: [&str; 2] = ["KERNEL", "MODULES.IMG"];

pub fn package_name(flavor: &str) -> String {
    format!("linux-{flavor}")
}

pub fn flavor_of(package: &str) -> Option<&str> {
    package.strip_prefix("linux-").filter(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// Flavors installed under `root`, sorted.
pub fn installed(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(root.join("usr/lib/flint/kernels"))
        .map(|rd| rd.flatten().filter(|e| FILES.iter().all(|f| e.path().join(f).is_file())).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

pub fn release(root: &Path, flavor: &str) -> Option<String> {
    fs::read_to_string(root.join("usr/lib/flint/kernels").join(flavor).join("RELEASE")).ok().map(|s| s.trim().to_string())
}

fn default_file(root: &Path) -> PathBuf {
    root.join("etc/flint/kernel")
}

pub fn chosen_default(root: &Path) -> String {
    fs::read_to_string(default_file(root)).map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| BUILTIN.to_string())
}

pub fn set_default(root: &Path, flavor: &str) -> io::Result<()> {
    fs::create_dir_all(root.join("etc/flint"))?;
    fs::write(default_file(root), format!("{flavor}\n"))
}

/// The boot partition, when this is a running Flint system whose boot files can be managed.
fn boot_dir(root: &Path) -> Option<PathBuf> {
    if root != Path::new("/") {
        return None;
    }
    let b = PathBuf::from("/boot/FLINT");
    (b.join("BOOT.CFG").is_file() && b.join("KERNEL").is_file()).then_some(b)
}

fn same(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) else { return false };
    if ma.len() != mb.len() {
        return false;
    }
    let read = |p: &Path| -> io::Result<Vec<u8>> {
        let mut v = Vec::new();
        fs::File::open(p)?.read_to_end(&mut v)?;
        Ok(v)
    };
    matches!((read(a), read(b)), (Ok(x), Ok(y)) if x == y)
}

/// Copy `from` to `to` through a temporary file, so a failure never leaves half a kernel.
fn install_file(from: &Path, to: &Path) -> io::Result<()> {
    let tmp = to.with_extension("NEW");
    fs::copy(from, &tmp)?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, to)
}

fn free_bytes(dir: &Path) -> u64 {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().to_string_lossy().as_bytes()) else { return u64::MAX };
    // SAFETY: statvfs fills the zeroed struct for a valid path.
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return u64::MAX;
        }
        s.f_bavail as u64 * s.f_frsize as u64
    }
}

/// Options of the first entry, used for the kernels' entries too.
fn first_options(cfg: &str) -> String {
    cfg.lines().map(str::trim).find_map(|l| l.strip_prefix("options")).map(|s| s.trim().to_string()).unwrap_or_else(|| "quiet loglevel=3".into())
}

/// The initrd lines of the first entry except the modules image (it is per kernel).
fn shared_initrds(cfg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_first = false;
    for l in cfg.lines().map(str::trim) {
        if l.starts_with("entry ") {
            if in_first {
                break;
            }
            in_first = true;
            continue;
        }
        if let Some(p) = l.strip_prefix("initrd") {
            out.push(p.trim().to_string());
        }
    }
    if out.is_empty() {
        out = ["\\FLINT\\BASE.IMG", "\\FLINT\\MODULES.IMG", "\\FLINT\\FIRMWARE.IMG", "\\FLINT\\GPU.IMG"].map(String::from).to_vec();
    }
    out
}

/// The boot menu with one entry per installed kernel and `default` pointing at `chosen`.
pub fn menu(cfg: &str, flavors: &[String], chosen: &str) -> String {
    let user: &str = cfg.split(MARK).next().unwrap_or(cfg);
    let mut lines: Vec<String> = user.trim_end().lines().map(String::from).collect();
    let entries_before = lines.iter().filter(|l| l.trim_start().starts_with("entry ")).count();
    let opts = first_options(cfg);
    let initrds = shared_initrds(cfg);
    let mut extra = String::new();
    for f in flavors {
        extra.push_str(&format!("\nentry Flint Linux ({f} kernel)\n  kernel \\FLINT\\KERNELS\\{f}\\KERNEL\n"));
        for i in &initrds {
            if i.eq_ignore_ascii_case("\\FLINT\\MODULES.IMG") {
                extra.push_str(&format!("  initrd \\FLINT\\KERNELS\\{f}\\MODULES.IMG\n"));
            } else {
                extra.push_str(&format!("  initrd {i}\n"));
            }
        }
        extra.push_str(&format!("  options {opts}\n"));
    }
    let default_n = match flavors.iter().position(|f| f == chosen) {
        Some(i) => entries_before + i + 1,
        None => 1,
    };
    // Replace (or add) the default line in the part people edit.
    let mut done = false;
    for l in lines.iter_mut() {
        if l.trim_start().starts_with("default ") {
            *l = format!("default {default_n}");
            done = true;
        }
    }
    if !done {
        let at = lines.iter().position(|l| l.trim_start().starts_with("entry ")).unwrap_or(lines.len());
        lines.insert(at, format!("default {default_n}"));
        if at > 0 && !lines[at - 1].trim().is_empty() {
            // keep the file readable
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    if !flavors.is_empty() {
        out.push_str(&format!("\n{MARK}\n"));
        out.push_str(&extra);
    }
    out
}

/// Bring the boot partition in line with the installed kernel packages. Does nothing for
/// another root (`--root`) or a system without a Flint boot partition.
pub fn sync(root: &Path) -> Result<(), String> {
    let Some(boot) = boot_dir(root) else { return Ok(()) };
    let flavors = installed(root);
    let kdir = boot.join("KERNELS");
    for f in &flavors {
        let src = root.join("usr/lib/flint/kernels").join(f);
        let dst = kdir.join(f);
        fs::create_dir_all(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        let need: u64 = FILES.iter().filter(|n| !same(&src.join(n), &dst.join(n))).map(|n| fs::metadata(src.join(n)).map_or(0, |m| m.len())).sum();
        if need == 0 {
            continue;
        }
        if need > free_bytes(&boot) {
            return Err(format!("the boot partition is too full for the {f} kernel ({} MiB needed, {} MiB free); remove a kernel with pkg kernels remove", need >> 20, free_bytes(&boot) >> 20));
        }
        for n in FILES {
            if !same(&src.join(n), &dst.join(n)) {
                install_file(&src.join(n), &dst.join(n)).map_err(|e| format!("{}: {e}", dst.join(n).display()))?;
            }
        }
        println!("kernel {f} copied to the boot partition");
    }
    if let Ok(rd) = fs::read_dir(&kdir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !flavors.contains(&name) {
                let _ = fs::remove_dir_all(e.path());
                println!("kernel {name} removed from the boot partition");
            }
        }
    }
    let mut chosen = chosen_default(root);
    if chosen != BUILTIN && !flavors.contains(&chosen) {
        chosen = BUILTIN.into();
        let _ = set_default(root, BUILTIN);
    }
    let cfg_path = boot.join("BOOT.CFG");
    let cfg = fs::read_to_string(&cfg_path).map_err(|e| format!("{}: {e}", cfg_path.display()))?;
    let new = menu(&cfg, &flavors, &chosen);
    if new != cfg {
        let tmp = boot.join("BOOT.NEW");
        fs::File::create(&tmp).and_then(|mut f| f.write_all(new.as_bytes()).and_then(|_| f.sync_all())).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, &cfg_path).map_err(|e| format!("{}: {e}", cfg_path.display()))?;
    }
    Ok(())
}

pub fn running_release() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = "timeout 3\n\nentry Flint Linux\n  kernel \\FLINT\\KERNEL\n  initrd \\FLINT\\BASE.IMG\n  initrd \\FLINT\\MODULES.IMG\n  initrd \\FLINT\\FIRMWARE.IMG\n  initrd \\FLINT\\GPU.IMG\n  options quiet loglevel=3\n\nentry Verbose\n  kernel \\FLINT\\KERNEL\n  options loglevel=7\n";

    #[test]
    fn menu_entries_and_default() {
        let m = menu(CFG, &["lts".into(), "zen".into()], "zen");
        assert!(m.contains("default 4"), "two own entries, then lts (3), zen (4):\n{m}");
        assert!(m.contains("entry Flint Linux (zen kernel)\n  kernel \\FLINT\\KERNELS\\zen\\KERNEL\n  initrd \\FLINT\\BASE.IMG\n  initrd \\FLINT\\KERNELS\\zen\\MODULES.IMG\n  initrd \\FLINT\\FIRMWARE.IMG\n  initrd \\FLINT\\GPU.IMG\n  options quiet loglevel=3\n"), "{m}");
        // rewriting is stable, and removing all kernels gives the original back (plus default)
        assert_eq!(menu(&m, &["lts".into(), "zen".into()], "zen"), m);
        let back = menu(&m, &[], "zen");
        assert!(!back.contains("KERNELS") && back.contains("default 1"), "{back}");
        assert_eq!(menu(&back, &[], BUILTIN), back);
    }

    #[test]
    fn names() {
        assert_eq!(flavor_of("linux-zen"), Some("zen"));
        assert_eq!(flavor_of("linux-"), None);
        assert_eq!(flavor_of("fastfetch"), None);
        assert_eq!(package_name("lts"), "linux-lts");
    }
}
