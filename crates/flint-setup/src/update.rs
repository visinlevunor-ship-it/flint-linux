//! `flint-update`: bring a Flint system (on a USB stick or installed) up to date.
//!
//! The base system is four packages in the Flint repository: flint-system (the base image
//! and the bootloader), flint-kernel (the kernel Flint comes with, and its drivers),
//! flint-firmware and flint-gpu-firmware (both into the same firmware tree). They hold the
//! same files the boot partition has; flint-update installs or upgrades them and every
//! other package, then copies what changed to the boot partition. The new base system starts with the next boot; what is
//! on the data partition (packages, files, settings) stays as it is.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const SYSTEM_PACKAGES: [&str; 4] = ["flint-system", "flint-kernel", "flint-firmware", "flint-gpu-firmware"];
const SYSTEM_DIR: &str = "/usr/lib/flint/system";
const BOOT: &str = "/boot";

/// Files in usr/lib/flint/system and where they go on the boot partition, in the order
/// they are replaced (the kernel last, right after what it needs).
const PLACES: [(&str, &[&str]); 5] = [
    ("BASE.IMG", &["FLINT/BASE.IMG"]),
    ("EARLY.IMG", &["FLINT/EARLY.IMG"]),
    ("RELEASE", &["FLINT/RELEASE"]),
    ("KERNEL", &["FLINT/KERNEL"]),
    ("BOOTX64.EFI", &["EFI/BOOT/BOOTX64.EFI", "EFI/FLINT/BOOTX64.EFI"]),
];

fn pkg(args: &[&str]) -> i32 {
    let mut argv = vec!["pkg".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    flint_pkg::main(argv)
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

/// Copy the base system files that differ to the boot partition. Returns what changed.
pub fn sync_boot(root: &Path, boot: &Path) -> Result<Vec<String>, String> {
    let src_dir = root.join(SYSTEM_DIR.trim_start_matches('/'));
    let mut changed = Vec::new();
    // Drivers first: firmware, then the modules of the new kernel (beside the old ones).
    if src_dir.join("firmware").is_dir() {
        let n = flint_pkg::sync_tree(&src_dir.join("firmware"), &boot.join("FLINT/DRIVERS/firmware")).map_err(|e| format!("firmware: {e}"))?;
        if n > 0 {
            changed.push(format!("{n} firmware files"));
        }
    }
    if let Ok(rd) = fs::read_dir(src_dir.join("modules")) {
        for e in rd.flatten() {
            let rel = e.file_name().to_string_lossy().into_owned();
            let n = flint_pkg::sync_tree(&e.path(), &boot.join("FLINT/DRIVERS/modules").join(&rel)).map_err(|err| format!("drivers for {rel}: {err}"))?;
            if n > 0 {
                changed.push(format!("drivers for {rel}"));
            }
        }
    }
    for (name, targets) in PLACES {
        let src = src_dir.join(name);
        if !src.is_file() {
            continue;
        }
        for t in targets {
            let dst = boot.join(t);
            // The second loader copy only exists on installed systems.
            if t.starts_with("EFI/FLINT") && !dst.exists() {
                continue;
            }
            if same(&src, &dst) {
                continue;
            }
            let len = fs::metadata(&src).map_err(|e| e.to_string())?.len();
            let old = fs::metadata(&dst).map_or(0, |m| m.len());
            if len > free_bytes(boot) + old {
                return Err(format!("the boot partition has no room for the new {name} ({} MiB); remove a kernel (pkg kernels remove) and run flint-update again", len >> 20));
            }
            let tmp: PathBuf = dst.with_extension("NEW");
            if let Some(dir) = dst.parent() {
                fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            fs::copy(&src, &tmp).and_then(|_| fs::File::open(&tmp)?.sync_all()).map_err(|e| format!("{}: {e}", tmp.display()))?;
            // Replacing a file needs room for both copies on a nearly full partition: drop
            // the old one first then (the data partition still has the new one).
            fs::rename(&tmp, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            changed.push(t.to_string());
        }
    }
    flint_core::sys::sync();
    Ok(changed)
}

pub fn main(args: Vec<String>) -> i32 {
    match run(&args[1..]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("flint-update: {e}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<i32, String> {
    let mut check = false;
    for a in args {
        match a.as_str() {
            "-n" | "--check" => check = true,
            "-h" | "--help" => {
                println!("usage: flint-update [--check]\n\nUpdates Flint: the base system (kernel, drivers, firmware, base image, bootloader)\nand every installed package. --check only shows what would change.");
                return Ok(0);
            }
            _ => return Err(format!("unknown option {a} (see flint-update --help)")),
        }
    }
    // SAFETY: plain query.
    if unsafe { libc::geteuid() } != 0 {
        return Err("run it as root".into());
    }
    if !Path::new(BOOT).join("FLINT/KERNEL").is_file() {
        return Err("the Flint boot partition is not mounted at /boot".into());
    }
    let persistent = flint_core::sys::is_mounted(flint_dev::persist::DATA_DIR);
    if !persistent {
        println!("note: this is a live session; the update goes to the boot partition, but packages installed now are gone after a reboot");
    }
    println!("{}: checking for updates", flint_core::version::long());
    if pkg(&["update"]) != 0 {
        return Err("could not fetch the package lists".into());
    }
    if check {
        return Ok(pkg(&["-n", "upgrade"]));
    }
    // The base system packages, the first time (the base itself came with the image).
    let have = |name: &str| Path::new("/var/lib/pkg/installed").join(name).exists();
    let missing: Vec<&str> = SYSTEM_PACKAGES.iter().copied().filter(|p| !have(p)).collect();
    if !missing.is_empty() {
        let mut a = vec!["install"];
        a.extend(&missing);
        if pkg(&a) != 0 {
            return Err(format!("could not install {}", missing.join(", ")));
        }
    }
    if pkg(&["upgrade"]) != 0 {
        return Err("upgrading packages failed; nothing on the boot partition was changed".into());
    }
    let changed = sync_boot(Path::new("/"), Path::new(BOOT))?;
    // drops the drivers of a kernel that was replaced
    flint_pkg::sync_kernels(Path::new("/"))?;
    if changed.is_empty() {
        println!("The base system is up to date.");
    } else {
        let v = fs::read_to_string(Path::new(SYSTEM_DIR).join("VERSION")).map(|s| s.trim().to_string()).unwrap_or_default();
        println!("Updated on the boot partition: {}", changed.join(", "));
        println!("Restart to use {}.", if v.is_empty() { "the new base system".to_string() } else { format!("Flint {v}") });
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_only_what_changed() {
        let base = std::env::temp_dir().join(format!("flint-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (root, boot) = (base.join("root"), base.join("boot"));
        let sys = root.join("usr/lib/flint/system");
        fs::create_dir_all(&sys).unwrap();
        fs::create_dir_all(boot.join("FLINT")).unwrap();
        fs::create_dir_all(boot.join("EFI/BOOT")).unwrap();
        fs::write(sys.join("KERNEL"), "new kernel").unwrap();
        fs::create_dir_all(sys.join("modules/7.1/kernel")).unwrap();
        fs::write(sys.join("modules/7.1/modules.dep"), "").unwrap();
        fs::create_dir_all(sys.join("firmware/amdgpu")).unwrap();
        fs::write(sys.join("firmware/amdgpu/x.bin"), "fw").unwrap();
        fs::write(sys.join("BASE.IMG"), "same base").unwrap();
        fs::write(sys.join("BOOTX64.EFI"), "loader").unwrap();
        fs::write(boot.join("FLINT/KERNEL"), "old kernel").unwrap();
        fs::write(boot.join("FLINT/BASE.IMG"), "same base").unwrap();
        let changed = sync_boot(&root, &boot).unwrap();
        assert_eq!(changed, ["1 firmware files", "drivers for 7.1", "FLINT/KERNEL", "EFI/BOOT/BOOTX64.EFI"]);
        assert!(boot.join("FLINT/DRIVERS/modules/7.1/modules.dep").is_file());
        assert_eq!(fs::read_to_string(boot.join("FLINT/KERNEL")).unwrap(), "new kernel");
        assert!(!boot.join("EFI/FLINT").exists(), "the installed-system copy only where it exists");
        assert!(sync_boot(&root, &boot).unwrap().is_empty());
        let _ = fs::remove_dir_all(&base);
    }
}
