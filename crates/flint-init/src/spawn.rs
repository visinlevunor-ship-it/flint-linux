//! Building child processes for boot tasks and services.

use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// A command that, in the child, unblocks all signals (init blocks them to use
/// sigtimedwait) and optionally becomes session leader on `tty` with that tty
/// as its controlling terminal and stdin/stdout/stderr.
pub fn command(argv: &[String], tty: Option<&str>) -> io::Result<Command> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let tty = tty
        .map(CString::new)
        .transpose()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "tty path contains a NUL byte"))?;

    let mut cmd = Command::new(program);
    cmd.args(args);
    // SAFETY: the closure only calls async-signal-safe libc functions and does not allocate.
    unsafe {
        cmd.pre_exec(move || child_setup(tty.as_deref()));
    }
    Ok(cmd)
}

fn child_setup(tty: Option<&CStr>) -> io::Result<()> {
    // SAFETY: plain libc calls on locally owned data; runs between fork and exec.
    unsafe {
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());

        if let Some(tty) = tty {
            libc::setsid();
            let fd = libc::open(tty.as_ptr(), libc::O_RDWR);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            libc::ioctl(fd, libc::TIOCSCTTY as _, 1);
            for target in 0..3 {
                libc::dup2(fd, target);
            }
            if fd > 2 {
                libc::close(fd);
            }
        }
    }
    Ok(())
}

/// Tests that spawn children serialise on this: `Supervisor::reap` uses waitpid(-1),
/// which would otherwise steal exit statuses from other tests' children.
#[cfg(test)]
pub static CHILD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_command() {
        assert!(command(&[], None).is_err());
    }

    #[test]
    fn rejects_nul_in_tty() {
        assert!(command(&["/bin/true".to_string()], Some("/dev/tty\0")).is_err());
    }

    #[test]
    fn runs_a_child_with_signals_unblocked() {
        let _guard = CHILD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Block SIGUSR1 here, then check the child does not inherit the block.
        // SAFETY: single-threaded test body touching only this thread's mask via sigprocmask.
        let status = unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGUSR1);
            let mut old: libc::sigset_t = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
            let argv: Vec<String> = ["/bin/sh", "-c", "grep -Eq '^SigBlk:[[:space:]]+0+$' /proc/self/status"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let st = command(&argv, None).unwrap().status();
            libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
            st
        };
        assert!(status.unwrap().success());
    }
}
