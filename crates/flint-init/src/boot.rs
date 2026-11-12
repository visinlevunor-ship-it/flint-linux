//! Early boot: pseudo filesystems, console, hostname, boot tasks.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use flint_core::power::set_ctrl_alt_del;
use flint_core::{flint_err, flint_info, flint_warn, sys};
use libc::c_ulong;

use crate::spawn;

const TASK_TIMEOUT: Duration = Duration::from_secs(30);

struct Fs {
    source: &'static str,
    target: &'static str,
    fstype: &'static str,
    flags: c_ulong,
    data: Option<&'static str>,
}

const NOSUID: c_ulong = libc::MS_NOSUID;
const NODEV: c_ulong = libc::MS_NODEV;
const NOEXEC: c_ulong = libc::MS_NOEXEC;

/// Order matters: /dev must exist before /dev/pts and /dev/shm.
const FILESYSTEMS: [Fs; 7] = [
    Fs { source: "proc", target: "/proc", fstype: "proc", flags: NOSUID | NODEV | NOEXEC, data: None },
    Fs { source: "sysfs", target: "/sys", fstype: "sysfs", flags: NOSUID | NODEV | NOEXEC, data: None },
    Fs { source: "devtmpfs", target: "/dev", fstype: "devtmpfs", flags: NOSUID, data: Some("mode=0755") },
    Fs { source: "devpts", target: "/dev/pts", fstype: "devpts", flags: NOSUID | NOEXEC, data: Some("mode=0620,ptmxmode=0666") },
    Fs { source: "tmpfs", target: "/dev/shm", fstype: "tmpfs", flags: NOSUID | NODEV, data: Some("mode=1777") },
    Fs { source: "tmpfs", target: "/run", fstype: "tmpfs", flags: NOSUID | NODEV, data: Some("mode=0755") },
    Fs { source: "tmpfs", target: "/tmp", fstype: "tmpfs", flags: NOSUID | NODEV, data: Some("mode=1777") },
];

/// Everything that must happen before boot tasks and services. All of it is
/// best effort: a failure is logged and boot continues, because a half-working
/// system you can log into beats a dead one.
pub fn early() {
    mount_filesystems();
    ensure_console();
    device_links();
    if let Err(e) = set_ctrl_alt_del(false) {
        flint_warn!("cannot disable Ctrl-Alt-Del: {e}");
    }
    flint_info!("{} starting", flint_core::version::long());
    remount_root_rw();
    set_hostname();
}

fn mount_filesystems() {
    for fs in &FILESYSTEMS {
        if sys::is_mounted(fs.target) {
            continue;
        }
        let _ = fs::create_dir_all(fs.target);
        match sys::mount(fs.source, fs.target, fs.fstype, fs.flags, fs.data) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {} // already mounted
            Err(e) => flint_warn!("cannot mount {} on {}: {e}", fs.fstype, fs.target),
        }
    }
}

/// The kernel gives init /dev/console as stdio only if the node existed when it
/// started init. If stdio is closed, reopen the console now that /dev is mounted.
fn ensure_console() {
    // SAFETY: fcntl/open/dup2 on fixed descriptors with valid arguments.
    unsafe {
        if libc::fcntl(2, libc::F_GETFD) != -1 {
            return;
        }
        let fd = libc::open(b"/dev/console\0".as_ptr().cast(), libc::O_RDWR);
        if fd < 0 {
            return;
        }
        for target in 0..3 {
            libc::dup2(fd, target);
        }
        if fd > 2 {
            libc::close(fd);
        }
    }
}

/// /dev/fd, /dev/std{in,out,err} and /dev/ptmx -> pts/ptmx (devtmpfs does not create these).
fn device_links() {
    let links = [
        ("/proc/self/fd", "/dev/fd"),
        ("/proc/self/fd/0", "/dev/stdin"),
        ("/proc/self/fd/1", "/dev/stdout"),
        ("/proc/self/fd/2", "/dev/stderr"),
    ];
    for (target, link) in links {
        if fs::symlink_metadata(link).is_err() {
            if let Err(e) = symlink(target, link) {
                flint_warn!("cannot create {link}: {e}");
            }
        }
    }
    if Path::new("/dev/pts/ptmx").exists() {
        let is_link = fs::symlink_metadata("/dev/ptmx").map_or(false, |m| m.file_type().is_symlink());
        if !is_link {
            let _ = fs::remove_file("/dev/ptmx");
            if let Err(e) = symlink("pts/ptmx", "/dev/ptmx") {
                flint_warn!("cannot create /dev/ptmx: {e}");
            }
        }
    }
}

fn remount_root_rw() {
    match sys::remount("/", 0) {
        Ok(()) => {}
        // EINVAL: root is an initramfs/ramfs, which cannot be remounted and is already writable.
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {}
        Err(e) => flint_warn!("cannot remount / read-write: {e}"),
    }
}

fn set_hostname() {
    let Ok(text) = fs::read_to_string("/etc/hostname") else { return };
    let name = text.lines().next().unwrap_or("").trim();
    if name.is_empty() {
        return;
    }
    if let Err(e) = sys::set_hostname(name) {
        flint_warn!("cannot set hostname to '{name}': {e}");
    }
}

/// Run boot tasks in order, waiting for each (killed after `TASK_TIMEOUT`).
pub fn run_tasks(tasks: &[Vec<String>]) {
    for argv in tasks {
        let name = argv.join(" ");
        if name == "@persist" {
            // Built in: only PID 1 can change its own root.
            match flint_dev::persist::setup() {
                Ok(Some(dev)) => flint_info!("files and settings are kept on {dev}"),
                Ok(None) => flint_info!("live session: changes are not kept after a reboot"),
                Err(e) => flint_warn!("{e}; changes are not kept after a reboot"),
            }
            continue;
        }
        let mut child = match spawn::command(argv, None).and_then(|mut c| c.spawn()) {
            Ok(c) => c,
            Err(e) => {
                flint_err!("cannot run boot task `{name}`: {e}");
                continue;
            }
        };
        let deadline = Instant::now() + TASK_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        flint_warn!("boot task `{name}` failed: {status}");
                    }
                    break;
                }
                Ok(None) if Instant::now() >= deadline => {
                    flint_err!("boot task `{name}` timed out after {TASK_TIMEOUT:?}, killing it");
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    flint_err!("waiting for boot task `{name}`: {e}");
                    break;
                }
            }
        }
    }
}
