//! Service supervision: start, reap, respawn with backoff, wait for signals.

use std::io;
use std::time::{Duration, Instant};

use flint_core::{flint_err, flint_info, flint_warn};
use libc::{c_int, pid_t, sigset_t};

use crate::config::{Mode, Service};
use crate::spawn;

/// A run shorter than this counts as a failed start for backoff purposes.
const FAST_EXIT: Duration = Duration::from_secs(2);
const MAX_BACKOFF_SECS: u64 = 10;
/// How long to sleep in sigtimedwait when no respawn is pending.
const IDLE_WAIT: Duration = Duration::from_secs(60);

/// Signals init handles: child exits, shutdown requests (see `PowerAction`) and SIGHUP.
const HANDLED: [c_int; 6] = [libc::SIGCHLD, libc::SIGHUP, libc::SIGINT, libc::SIGTERM, libc::SIGUSR1, libc::SIGUSR2];

/// Block the handled signals so they queue up and can be taken with `wait_signal`.
/// (The kernel never drops a *blocked* signal for PID 1, unlike an unhandled one.)
pub fn block_signals() -> io::Result<sigset_t> {
    // SAFETY: `set` is initialised by sigemptyset before use.
    unsafe {
        let mut set: sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in HANDLED {
            libc::sigaddset(&mut set, sig);
        }
        if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(set)
    }
}

/// Wait up to `timeout` for one of the blocked signals. Without a signal set
/// (blocking failed at startup) this just sleeps, capped at one second.
pub fn wait_signal(set: Option<&sigset_t>, timeout: Duration) -> Option<c_int> {
    let Some(set) = set else {
        std::thread::sleep(timeout.min(Duration::from_secs(1)));
        return None;
    };
    let ts = libc::timespec {
        tv_sec: timeout.as_secs() as _, // time_t is 32 or 64 bits depending on the libc
        tv_nsec: timeout.subsec_nanos() as _,
    };
    // SAFETY: `set` and `ts` are valid; `info` is plain zeroed memory the kernel fills in.
    let sig = unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        libc::sigtimedwait(set, &mut info, &ts)
    };
    (sig > 0).then_some(sig)
}

struct Slot {
    svc: Service,
    pid: Option<pid_t>,
    started: Instant,
    next_start: Instant,
    fast_fails: u32,
    finished: bool,
}

impl Slot {
    fn new(svc: Service, now: Instant) -> Self {
        Slot { svc, pid: None, started: now, next_start: now, fast_fails: 0, finished: false }
    }

    fn is_idle(&self) -> bool {
        self.pid.is_none() && !self.finished
    }

    fn start(&mut self, now: Instant) {
        self.started = now;
        let spawned = spawn::command(&self.svc.argv, self.svc.tty.as_deref()).and_then(|mut c| c.spawn());
        match spawned {
            Ok(child) => {
                // Dropping `Child` neither kills nor waits; `Supervisor::reap` collects it.
                self.pid = Some(child.id() as pid_t);
                flint_info!("started {} (pid {})", self.svc.name, child.id());
            }
            Err(e) => {
                flint_err!("cannot start {}: {e}", self.svc.name);
                self.schedule_after_exit(now);
            }
        }
    }

    /// Decide what happens after the process ended (or failed to start) at `now`.
    fn schedule_after_exit(&mut self, now: Instant) {
        self.pid = None;
        if self.svc.mode == Mode::Once {
            self.finished = true;
            return;
        }
        if now.saturating_duration_since(self.started) < FAST_EXIT {
            self.fast_fails = self.fast_fails.saturating_add(1);
        } else {
            self.fast_fails = 0;
        }
        self.next_start = now + backoff(self.fast_fails);
    }
}

/// 0 s after a normal long run; then 1 s, 2 s, ... capped, while it keeps dying fast.
fn backoff(fast_fails: u32) -> Duration {
    Duration::from_secs(u64::from(fast_fails).min(MAX_BACKOFF_SECS))
}

pub struct Supervisor {
    slots: Vec<Slot>,
}

impl Supervisor {
    pub fn new(services: Vec<Service>) -> Self {
        let now = Instant::now();
        Supervisor { slots: services.into_iter().map(|s| Slot::new(s, now)).collect() }
    }

    /// Start every idle service whose start time has come.
    pub fn start_due(&mut self) {
        let now = Instant::now();
        for slot in &mut self.slots {
            if slot.is_idle() && now >= slot.next_start {
                slot.start(now);
            }
        }
    }

    /// How long the main loop may sleep before a respawn is due.
    pub fn timeout(&self) -> Duration {
        let now = Instant::now();
        self.slots
            .iter()
            .filter(|s| s.is_idle())
            .map(|s| s.next_start.saturating_duration_since(now))
            .min()
            .unwrap_or(IDLE_WAIT)
            .max(Duration::from_millis(1))
    }

    /// Collect every exited child, including orphans re-parented to init.
    pub fn reap(&mut self) {
        loop {
            let mut status: c_int = 0;
            // SAFETY: `status` is a valid out pointer.
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid > 0 {
                self.exited(pid, status);
            } else if pid == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break; // nothing ready, or no children at all (ECHILD)
            }
        }
    }

    fn exited(&mut self, pid: pid_t, status: c_int) {
        let now = Instant::now();
        let Some(slot) = self.slots.iter_mut().find(|s| s.pid == Some(pid)) else {
            return; // an orphan or a finished boot task: reaped, nothing to track
        };
        let how = if libc::WIFEXITED(status) {
            format!("exited with status {}", libc::WEXITSTATUS(status))
        } else if libc::WIFSIGNALED(status) {
            format!("killed by signal {}", libc::WTERMSIG(status))
        } else {
            format!("ended (raw status {status:#x})")
        };
        slot.schedule_after_exit(now);
        if slot.finished {
            flint_info!("{} {how}", slot.svc.name);
        } else if slot.fast_fails > 0 {
            flint_warn!("{} {how} after a short run; restarting in {:?}", slot.svc.name, backoff(slot.fast_fails));
        } else {
            flint_info!("{} {how}; restarting", slot.svc.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(mode: Mode) -> Service {
        Service { name: "t".into(), mode, tty: None, argv: vec!["/bin/true".into()] }
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(3));
        assert_eq!(backoff(10), Duration::from_secs(10));
        assert_eq!(backoff(500), Duration::from_secs(10));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(10));
    }

    #[test]
    fn fast_exits_back_off_and_long_runs_reset() {
        let base = Instant::now();
        let mut s = Slot::new(svc(Mode::Respawn), base);

        s.started = base;
        s.schedule_after_exit(base + Duration::from_millis(100));
        assert_eq!(s.fast_fails, 1);
        assert_eq!(s.next_start, base + Duration::from_millis(100) + Duration::from_secs(1));

        s.started = base + Duration::from_secs(2);
        s.schedule_after_exit(base + Duration::from_secs(2));
        assert_eq!(s.fast_fails, 2);

        s.started = base;
        s.schedule_after_exit(base + Duration::from_secs(60));
        assert_eq!(s.fast_fails, 0);
        assert_eq!(s.next_start, base + Duration::from_secs(60));
        assert!(s.is_idle());
    }

    #[test]
    fn once_services_finish() {
        let base = Instant::now();
        let mut s = Slot::new(svc(Mode::Once), base);
        s.pid = Some(42);
        s.schedule_after_exit(base + Duration::from_secs(5));
        assert!(s.finished);
        assert!(!s.is_idle());
    }

    #[test]
    fn timeout_defaults_when_nothing_pending() {
        let sup = Supervisor::new(vec![]);
        assert_eq!(sup.timeout(), IDLE_WAIT);
    }

    #[test]
    fn supervisor_starts_reaps_and_respawns_a_real_child() {
        let _guard = spawn::CHILD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut sup = Supervisor::new(vec![Service {
            name: "quick".into(),
            mode: Mode::Respawn,
            tty: None,
            argv: vec!["/bin/true".into()],
        }]);
        sup.start_due();
        let first = sup.slots[0].pid.expect("child started");

        let deadline = Instant::now() + Duration::from_secs(5);
        while sup.slots[0].pid == Some(first) && Instant::now() < deadline {
            sup.reap();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sup.slots[0].is_idle(), "child was reaped and the slot is waiting to respawn");
        assert_eq!(sup.slots[0].fast_fails, 1, "true exits immediately, so this is a fast exit");
        assert!(sup.timeout() <= Duration::from_secs(1));
    }
}
