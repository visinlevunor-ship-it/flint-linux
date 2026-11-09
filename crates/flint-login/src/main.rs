//! flint-login: getty + login in one program.
//!
//! flint-init starts it on a terminal (controlling tty and stdio already set up).
//! It shows a banner, asks for a user name and password, checks them against
//! /etc/passwd and /etc/shadow, then becomes the user's login shell. When that shell
//! exits, flint-init sees the exit and starts flint-login again.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use flint_auth::files::{groups_of, parse_group, User};
use flint_auth::login::authenticate;
use flint_core::version;

const MAX_ATTEMPTS: u32 = 3;
const PROMPT_TIMEOUT_SECS: u32 = 120;
const FAIL_DELAY: Duration = Duration::from_secs(2);
const MAX_LINE: usize = 256;

const ROOT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const USER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

fn main() -> ExitCode {
    // SAFETY: plain libc query.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("flint-login: must run as root");
        return ExitCode::FAILURE;
    }

    set_sane_tty();
    banner();

    let hostname = hostname();
    for _ in 0..MAX_ATTEMPTS {
        let Some(name) = prompt_line(&format!("{hostname} login: "), true) else {
            return ExitCode::SUCCESS; // EOF or timeout: init starts a fresh login
        };
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let Some(password) = prompt_line("Password: ", false) else {
            return ExitCode::SUCCESS;
        };

        let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
        let shadow = fs::read_to_string("/etc/shadow").unwrap_or_default();
        match authenticate(&passwd, &shadow, &name, &password) {
            Some(user) => start_session(&user), // never returns
            None => {
                thread::sleep(FAIL_DELAY);
                println!("Login incorrect\n");
            }
        }
    }
    ExitCode::FAILURE
}

fn banner() {
    let tty = tty_name().unwrap_or_else(|| "?".to_string());
    println!("\n{} on {tty}\n", version::long());
}

fn tty_name() -> Option<String> {
    // SAFETY: ttyname returns a pointer to a static buffer or null.
    let p = unsafe { libc::ttyname(0) };
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null, NUL-terminated.
    let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
    Some(s.strip_prefix("/dev/").unwrap_or(&s).to_string())
}

fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "flint".to_string())
}

/// Known-good terminal settings: line editing, echo, signals, CR to NL, NL to CRNL.
fn set_sane_tty() {
    // SAFETY: tcgetattr/tcsetattr on fd 0 with a zeroed, then filled, termios.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return; // not a terminal
        }
        t.c_iflag |= libc::ICRNL;
        t.c_oflag |= libc::OPOST | libc::ONLCR;
        t.c_lflag |= libc::ECHO | libc::ECHOE | libc::ECHOK | libc::ICANON | libc::ISIG;
        libc::tcsetattr(0, libc::TCSANOW, &t);
    }
}

/// Disables echo on stdin until dropped.
struct EchoOff {
    saved: Option<libc::termios>,
}

impl EchoOff {
    fn new() -> Self {
        // SAFETY: as in `set_sane_tty`.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return EchoOff { saved: None };
            }
            let saved = t;
            t.c_lflag &= !(libc::ECHO | libc::ECHONL);
            libc::tcsetattr(0, libc::TCSAFLUSH, &t);
            EchoOff { saved: Some(saved) }
        }
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        if let Some(t) = &self.saved {
            // SAFETY: restoring settings read earlier from the same descriptor.
            unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, t) };
        }
    }
}

/// Show `prompt`, read one line (without the newline). `None` on EOF, error or timeout.
fn prompt_line(prompt: &str, echo: bool) -> Option<String> {
    // Echo goes off *before* the prompt is shown: tcsetattr(TCSAFLUSH) discards pending
    // input, so a fast typist (or a paste) would otherwise lose the start of the password.
    let guard = (!echo).then(EchoOff::new);
    print!("{prompt}");
    let _ = io::stdout().flush();

    // SAFETY: SIGALRM's default action ends this process; init then respawns flint-login.
    unsafe { libc::alarm(PROMPT_TIMEOUT_SECS) };
    let line = read_line();
    // SAFETY: cancelling the timer.
    unsafe { libc::alarm(0) };
    drop(guard);
    if !echo {
        println!();
    }
    line
}

/// Read bytes from stdin up to a newline, at most MAX_LINE; EOF with nothing read is `None`.
fn read_line() -> Option<String> {
    let mut stdin = io::stdin().lock();
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stdin.read(&mut byte) {
            Ok(0) => return if line.is_empty() { None } else { Some(lossy(line)) },
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Some(lossy(line));
                }
                if line.len() < MAX_LINE {
                    line.push(byte[0]);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Environment for a login shell: nothing is inherited except TERM.
fn session_env(user: &User, term: Option<String>) -> Vec<(&'static str, String)> {
    let shell = login_shell(user);
    vec![
        ("HOME", user.home.clone()),
        ("SHELL", shell),
        ("USER", user.name.clone()),
        ("LOGNAME", user.name.clone()),
        ("PATH", if user.uid == 0 { ROOT_PATH } else { USER_PATH }.to_string()),
        ("TERM", term.filter(|t| !t.is_empty()).unwrap_or_else(|| "linux".to_string())),
    ]
}

fn login_shell(user: &User) -> String {
    if user.shell.is_empty() {
        "/bin/sh".to_string()
    } else {
        user.shell.clone()
    }
}

/// argv[0] with a leading dash tells the shell it is a login shell.
fn login_argv0(shell: &str) -> String {
    let base = shell.rsplit('/').next().unwrap_or(shell);
    format!("-{base}")
}

fn start_session(user: &User) -> ! {
    let groups_text = fs::read_to_string("/etc/group").unwrap_or_default();
    let groups = parse_group(&groups_text);
    let gids = groups_of(&groups, user);

    // Hand the terminal to the user.
    // SAFETY: isatty/fchown/fchmod on fd 0.
    unsafe {
        if libc::isatty(0) == 1 {
            let tty_gid = groups.iter().find(|g| g.name == "tty").map_or(user.gid, |g| g.gid);
            libc::fchown(0, user.uid, tty_gid);
            libc::fchmod(0, 0o620);
        }
    }

    // Drop privileges: supplementary groups, then gid, then uid (order matters).
    // SAFETY: setgroups reads `gids.len()` entries from a valid slice.
    let dropped = unsafe {
        libc::setgroups(gids.len() as _, gids.as_ptr() as *const libc::gid_t) == 0
            && libc::setgid(user.gid) == 0
            && libc::setuid(user.uid) == 0
    };
    if !dropped {
        eprintln!("flint-login: cannot switch to {}: {}", user.name, io::Error::last_os_error());
        std::process::exit(1);
    }
    // SAFETY: umask has no preconditions; alarm(0) cancels any pending timer (it survives exec).
    unsafe {
        libc::umask(0o022);
        libc::alarm(0);
    }

    let term = std::env::var("TERM").ok();
    let env = session_env(user, term);

    if std::env::set_current_dir(&user.home).is_err() {
        println!("No directory {}, logging in with HOME=/", user.home);
        let _ = std::env::set_current_dir("/");
    }
    if let Ok(motd) = fs::read_to_string("/etc/motd") {
        print!("{motd}");
    }
    let _ = io::stdout().flush();

    let shell = login_shell(user);
    let mut cmd = Command::new(&shell);
    cmd.arg0(login_argv0(&shell)).env_clear();
    for (k, v) in env {
        // A directory that does not exist falls back to "/", so HOME must follow.
        let v = if k == "HOME" && !std::path::Path::new(&v).is_dir() { "/".to_string() } else { v };
        cmd.env(k, v);
    }
    let err = cmd.exec();
    eprintln!("flint-login: cannot run {shell}: {err}");
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(uid: u32, shell: &str) -> User {
        User {
            name: "bob".into(),
            passwd: "x".into(),
            uid,
            gid: uid,
            gecos: String::new(),
            home: "/home/bob".into(),
            shell: shell.into(),
        }
    }

    #[test]
    fn login_shell_gets_a_dash() {
        assert_eq!(login_argv0("/bin/sh"), "-sh");
        assert_eq!(login_argv0("/usr/local/bin/flint-sh"), "-flint-sh");
        assert_eq!(login_argv0("sh"), "-sh");
    }

    #[test]
    fn empty_shell_falls_back_to_sh() {
        assert_eq!(login_shell(&user(1000, "")), "/bin/sh");
        assert_eq!(login_shell(&user(1000, "/bin/ash")), "/bin/ash");
    }

    #[test]
    fn env_is_minimal_and_per_user() {
        let get = |env: &[(&str, String)], k: &str| env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        let root = session_env(&user(0, "/bin/sh"), None);
        assert_eq!(get(&root, "PATH").unwrap(), ROOT_PATH);
        assert_eq!(get(&root, "TERM").unwrap(), "linux");
        assert_eq!(get(&root, "USER").unwrap(), "bob");

        let normal = session_env(&user(1000, "/bin/sh"), Some("xterm".into()));
        assert_eq!(get(&normal, "PATH").unwrap(), USER_PATH);
        assert_eq!(get(&normal, "TERM").unwrap(), "xterm");
        assert_eq!(get(&normal, "HOME").unwrap(), "/home/bob");
        assert_eq!(normal.len(), 6, "nothing else is inherited");

        let empty_term = session_env(&user(1000, "/bin/sh"), Some(String::new()));
        assert_eq!(get(&empty_term, "TERM").unwrap(), "linux");
    }
}
