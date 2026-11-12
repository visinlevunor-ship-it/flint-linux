//! Orderly shutdown: stop processes, sync, unmount, tell the kernel.

use std::io;
use std::thread;
use std::time::{Duration, Instant};

use flint_core::{flint_err, flint_info, flint_warn, sys, PowerAction};

/// Mount points that stay until the very end.
const KEEP: [&str; 4] = ["/", "/proc", "/sys", "/dev"];

pub fn run(action: PowerAction) -> ! {
    flint_info!("{}: stopping all processes", action.name());
    // SAFETY: kill(2) with pid -1 signals every process except init itself.
    unsafe {
        libc::kill(-1, libc::SIGTERM);
        libc::kill(-1, libc::SIGCONT); // so stopped processes can see the SIGTERM
    }
    if !wait_for_children(Duration::from_secs(5)) {
        flint_warn!("some processes ignored SIGTERM, sending SIGKILL");
        // SAFETY: as above.
        unsafe { libc::kill(-1, libc::SIGKILL) };
        wait_for_children(Duration::from_secs(2));
    }

    sys::sync();
    // With a persistent root, "/" is an overlay whose writable layer is the data partition:
    // make it read-only first, or the data partition cannot be made read-only below.
    if let Err(e) = sys::remount("/", libc::MS_RDONLY) {
        if e.raw_os_error() != Some(libc::EINVAL) {
            flint_warn!("cannot remount / read-only: {e}");
        }
    }
    unmount_all();
    match sys::remount("/", libc::MS_RDONLY) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {} // initramfs root
        Err(e) => flint_warn!("cannot remount / read-only: {e}"),
    }
    sys::sync();

    flint_info!("{}", action.name());
    let err = action.execute();
    flint_err!("{} failed: {err}", action.name());
    // PID 1 must never exit: that would panic the kernel.
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

/// Reap children until none are left (true) or the timeout expires (false).
fn wait_for_children(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        loop {
            let mut status = 0;
            // SAFETY: `status` is a valid out pointer.
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid > 0 {
                continue;
            }
            if pid == 0 {
                break; // children exist but none has exited yet
            }
            match io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                _ => return true, // ECHILD: nothing left
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn unmount_all() {
    let mounts = match sys::read_mounts() {
        Ok(m) => m,
        Err(e) => {
            flint_warn!("cannot read the mount table, skipping unmount: {e}");
            return;
        }
    };
    // Reverse order so nested mounts go first.
    for m in mounts.iter().rev() {
        if KEEP.contains(&m.target.as_str()) {
            continue;
        }
        // Read-only first: unmounting the data partition only detaches it while the root
        // overlay still uses it, so the remount is what actually closes it cleanly.
        if m.device.starts_with("/dev/") {
            if let Err(e) = sys::remount(&m.target, libc::MS_RDONLY) {
                flint_warn!("cannot remount {} read-only: {e}", m.target);
            }
        }
        if let Err(e) = sys::umount(&m.target) {
            flint_warn!("cannot unmount {}: {e}; remounting read-only instead", m.target);
            if let Err(e) = sys::remount(&m.target, libc::MS_RDONLY) {
                flint_warn!("cannot remount {} read-only: {e}", m.target);
            }
        }
    }
}
