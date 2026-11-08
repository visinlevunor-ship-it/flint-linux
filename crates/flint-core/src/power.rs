//! Power actions: the one place that maps poweroff/reboot/halt to the signal
//! sent to PID 1 and to the reboot(2) command the kernel executes.

use std::io;

use libc::c_int;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    PowerOff,
    Reboot,
    Halt,
}

impl PowerAction {
    pub const ALL: [PowerAction; 3] = [PowerAction::PowerOff, PowerAction::Reboot, PowerAction::Halt];

    /// Signal that asks init to perform this action.
    pub const fn signal(self) -> c_int {
        match self {
            PowerAction::PowerOff => libc::SIGUSR2,
            PowerAction::Reboot => libc::SIGTERM,
            PowerAction::Halt => libc::SIGUSR1,
        }
    }

    /// Inverse of [`signal`](Self::signal). SIGINT (Ctrl-Alt-Del) also means reboot.
    pub fn from_signal(sig: c_int) -> Option<Self> {
        match sig {
            libc::SIGUSR2 => Some(PowerAction::PowerOff),
            libc::SIGTERM | libc::SIGINT => Some(PowerAction::Reboot),
            libc::SIGUSR1 => Some(PowerAction::Halt),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            PowerAction::PowerOff => "poweroff",
            PowerAction::Reboot => "reboot",
            PowerAction::Halt => "halt",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }

    const fn kernel_cmd(self) -> c_int {
        match self {
            PowerAction::PowerOff => libc::LINUX_REBOOT_CMD_POWER_OFF,
            PowerAction::Reboot => libc::LINUX_REBOOT_CMD_RESTART,
            PowerAction::Halt => libc::LINUX_REBOOT_CMD_HALT,
        }
    }

    /// Ask the kernel to carry out the action *now* (no sync, no unmount).
    /// Only returns on failure.
    pub fn execute(self) -> io::Error {
        // SAFETY: reboot(2) takes a plain integer command.
        unsafe { libc::reboot(self.kernel_cmd()) };
        io::Error::last_os_error()
    }
}

/// With Ctrl-Alt-Del disabled the kernel sends SIGINT to init instead of
/// rebooting immediately, which lets init shut down cleanly.
pub fn set_ctrl_alt_del(enabled: bool) -> io::Result<()> {
    let cmd = if enabled { libc::LINUX_REBOOT_CMD_CAD_ON } else { libc::LINUX_REBOOT_CMD_CAD_OFF };
    // SAFETY: reboot(2) takes a plain integer command.
    if unsafe { libc::reboot(cmd) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_round_trips() {
        for a in PowerAction::ALL {
            assert_eq!(PowerAction::from_signal(a.signal()), Some(a));
        }
    }

    #[test]
    fn signals_are_distinct() {
        let mut sigs: Vec<c_int> = PowerAction::ALL.iter().map(|a| a.signal()).collect();
        sigs.sort_unstable();
        sigs.dedup();
        assert_eq!(sigs.len(), PowerAction::ALL.len());
    }

    #[test]
    fn ctrl_alt_del_means_reboot() {
        assert_eq!(PowerAction::from_signal(libc::SIGINT), Some(PowerAction::Reboot));
        assert_eq!(PowerAction::from_signal(libc::SIGHUP), None);
    }

    #[test]
    fn names_round_trip() {
        for a in PowerAction::ALL {
            assert_eq!(PowerAction::from_name(a.name()), Some(a));
        }
        assert_eq!(PowerAction::from_name("shutdown"), None);
    }
}
