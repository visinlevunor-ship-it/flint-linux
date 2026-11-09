//! Small applets: true false echo pwd sleep clear sync uname hostname whoami id env kill.

use std::ffi::CStr;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use flint_auth::files::{find_user_by_uid, parse_group, parse_passwd};

use crate::util::{self, error};

pub fn true_(_: &[String]) -> i32 {
    0
}

pub fn false_(_: &[String]) -> i32 {
    1
}

// ---------------------------------------------------------------- echo

/// Expand backslash escapes. Returns the bytes and whether `\c` (stop output) was seen.
pub fn unescape(s: &str) -> (Vec<u8>, bool) {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match b[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'e' => out.push(0x1b),
            b'f' => out.push(0x0c),
            b'v' => out.push(0x0b),
            b'\\' => out.push(b'\\'),
            b'c' => return (out, true),
            b'0' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 3 && i + 1 < b.len() && (b'0'..=b'7').contains(&b[i + 1]) {
                    v = v * 8 + u32::from(b[i + 1] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
            }
            b'x' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 2 && i + 1 < b.len() && b[i + 1].is_ascii_hexdigit() {
                    v = v * 16 + (b[i + 1] as char).to_digit(16).unwrap_or(0);
                    i += 1;
                    n += 1;
                }
                if n == 0 {
                    out.extend_from_slice(b"\\x");
                } else {
                    out.push(v as u8);
                }
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
        i += 1;
    }
    (out, false)
}

pub fn echo(args: &[String]) -> i32 {
    let (mut newline, mut escapes) = (true, false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => newline = false,
            "-e" => escapes = true,
            "-E" => escapes = false,
            "-ne" | "-en" => {
                newline = false;
                escapes = true;
            }
            _ => break,
        }
        i += 1;
    }
    let text = args[i..].join(" ");
    let mut out = Vec::new();
    let mut stop = false;
    if escapes {
        let (bytes, s) = unescape(&text);
        out = bytes;
        stop = s;
    } else {
        out.extend_from_slice(text.as_bytes());
    }
    if newline && !stop {
        out.push(b'\n');
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    util::finish_output("echo", lock.write_all(&out).and_then(|()| lock.flush()))
}

// ---------------------------------------------------------------- pwd, clear, sync

pub fn pwd(_: &[String]) -> i32 {
    match std::env::current_dir() {
        Ok(p) => util::finish_output("pwd", writeln!(std::io::stdout(), "{}", p.display())),
        Err(e) => {
            error("pwd", e);
            1
        }
    }
}

pub fn clear(_: &[String]) -> i32 {
    util::finish_output("clear", write!(std::io::stdout(), "\x1b[H\x1b[2J"))
}

pub fn sync(_: &[String]) -> i32 {
    // SAFETY: sync(2) has no preconditions.
    unsafe { libc::sync() };
    0
}

// ---------------------------------------------------------------- sleep

/// `1.5`, `30s`, `2m`, `1h`, `1d`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let (num, mult) = match s.chars().last()? {
        's' => (&s[..s.len() - 1], 1.0),
        'm' => (&s[..s.len() - 1], 60.0),
        'h' => (&s[..s.len() - 1], 3600.0),
        'd' => (&s[..s.len() - 1], 86400.0),
        _ => (s, 1.0),
    };
    let v: f64 = num.parse().ok()?;
    (v.is_finite() && v >= 0.0).then(|| Duration::from_secs_f64(v * mult))
}

pub fn sleep(args: &[String]) -> i32 {
    if args.is_empty() {
        error("sleep", "missing operand");
        return 1;
    }
    let mut total = Duration::ZERO;
    for a in args {
        match parse_duration(a) {
            Some(d) => total += d,
            None => {
                error("sleep", format!("invalid time interval '{a}'"));
                return 1;
            }
        }
    }
    std::thread::sleep(total);
    0
}

// ---------------------------------------------------------------- uname, hostname

fn field(chars: &[libc::c_char]) -> String {
    // SAFETY: utsname fields are NUL-terminated arrays filled by the kernel.
    unsafe { CStr::from_ptr(chars.as_ptr()) }.to_string_lossy().into_owned()
}

pub fn uname(args: &[String]) -> i32 {
    let p = match util::parse(args, "snrvma", "") {
        Ok(p) => p,
        Err(e) => {
            error("uname", e);
            return 1;
        }
    };
    // SAFETY: uname fills a zeroed utsname.
    let u = unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            error("uname", std::io::Error::last_os_error());
            return 1;
        }
        u
    };
    let all = p.has('a');
    let any = ['s', 'n', 'r', 'v', 'm'].iter().any(|&c| p.has(c));
    let mut parts = Vec::new();
    if p.has('s') || all || !any {
        parts.push(field(&u.sysname));
    }
    if p.has('n') || all {
        parts.push(field(&u.nodename));
    }
    if p.has('r') || all {
        parts.push(field(&u.release));
    }
    if p.has('v') || all {
        parts.push(field(&u.version));
    }
    if p.has('m') || all {
        parts.push(field(&u.machine));
    }
    util::finish_output("uname", writeln!(std::io::stdout(), "{}", parts.join(" ")))
}

pub fn hostname(args: &[String]) -> i32 {
    match args {
        [] => {
            let mut buf = [0u8; 256];
            // SAFETY: the buffer is valid for `buf.len()` bytes.
            let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) };
            if rc != 0 {
                error("hostname", std::io::Error::last_os_error());
                return 1;
            }
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            util::finish_output("hostname", writeln!(std::io::stdout(), "{}", String::from_utf8_lossy(&buf[..end])))
        }
        [name] => {
            // SAFETY: pointer and length describe the valid string.
            let rc = unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) };
            if rc != 0 {
                error("hostname", std::io::Error::last_os_error());
                return 1;
            }
            0
        }
        _ => {
            error("hostname", "too many arguments");
            1
        }
    }
}

// ---------------------------------------------------------------- whoami, id

fn passwd_text() -> String {
    std::fs::read_to_string("/etc/passwd").unwrap_or_default()
}

pub fn whoami(_: &[String]) -> i32 {
    // SAFETY: plain libc query.
    let uid = unsafe { libc::geteuid() };
    let users = parse_passwd(&passwd_text());
    let name = find_user_by_uid(&users, uid).map_or_else(|| uid.to_string(), |u| u.name.clone());
    util::finish_output("whoami", writeln!(std::io::stdout(), "{name}"))
}

pub fn id(_: &[String]) -> i32 {
    let users = parse_passwd(&passwd_text());
    let groups = parse_group(&std::fs::read_to_string("/etc/group").unwrap_or_default());
    // SAFETY: plain libc queries; getgroups fills a buffer of the size we pass.
    let (uid, gid, gids) = unsafe {
        let mut buf = [0 as libc::gid_t; 64];
        let n = libc::getgroups(buf.len() as _, buf.as_mut_ptr());
        let gids: Vec<u32> = if n > 0 { buf[..n as usize].to_vec() } else { Vec::new() };
        (libc::getuid(), libc::getgid(), gids)
    };
    let uname = find_user_by_uid(&users, uid).map(|u| u.name.clone());
    let gname = |g: u32| groups.iter().find(|x| x.gid == g).map(|x| x.name.clone());
    let fmt = |n: u32, name: Option<String>| match name {
        Some(s) => format!("{n}({s})"),
        None => n.to_string(),
    };
    let mut line = format!("uid={} gid={}", fmt(uid, uname), fmt(gid, gname(gid)));
    if !gids.is_empty() {
        let list: Vec<String> = gids.iter().map(|&g| fmt(g, gname(g))).collect();
        line.push_str(&format!(" groups={}", list.join(",")));
    }
    util::finish_output("id", writeln!(std::io::stdout(), "{line}"))
}

// ---------------------------------------------------------------- env

pub fn env(args: &[String]) -> i32 {
    let mut clear = false;
    let mut i = 0;
    if args.first().map(String::as_str) == Some("-i") {
        clear = true;
        i = 1;
    }
    let mut vars: Vec<(String, String)> = Vec::new();
    while i < args.len() {
        match args[i].split_once('=') {
            Some((k, v)) if !k.is_empty() => vars.push((k.to_string(), v.to_string())),
            _ => break,
        }
        i += 1;
    }
    if i == args.len() {
        let mut current: Vec<(String, String)> = if clear {
            Vec::new()
        } else {
            std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect()
        };
        for (k, v) in vars {
            current.retain(|(ck, _)| *ck != k);
            current.push((k, v));
        }
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let r: std::io::Result<()> = current.iter().try_for_each(|(k, v)| writeln!(out, "{k}={v}"));
        return util::finish_output("env", r);
    }
    let mut cmd = Command::new(&args[i]);
    cmd.args(&args[i + 1..]);
    if clear {
        cmd.env_clear();
    }
    cmd.envs(vars);
    let err = cmd.exec();
    error("env", format!("{}: {err}", args[i]));
    if err.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

// ---------------------------------------------------------------- kill

const SIGNALS: &[(&str, i32)] = &[
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("ILL", 4),
    ("ABRT", 6),
    ("BUS", 7),
    ("FPE", 8),
    ("KILL", 9),
    ("USR1", 10),
    ("SEGV", 11),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("CHLD", 17),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
    ("TTIN", 21),
    ("TTOU", 22),
];

pub fn parse_signal(s: &str) -> Option<i32> {
    if let Ok(n) = s.parse::<i32>() {
        return (0..=64).contains(&n).then_some(n);
    }
    let name = s.strip_prefix("SIG").unwrap_or(s).to_ascii_uppercase();
    SIGNALS.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

pub fn kill(args: &[String]) -> i32 {
    let mut sig = libc::SIGTERM;
    let mut i = 0;
    if let Some(first) = args.first() {
        if first == "-l" {
            let names: Vec<&str> = SIGNALS.iter().map(|(n, _)| *n).collect();
            return util::finish_output("kill", writeln!(std::io::stdout(), "{}", names.join(" ")));
        }
        if first == "-s" {
            match args.get(1).and_then(|s| parse_signal(s)) {
                Some(s) => sig = s,
                None => {
                    error("kill", "invalid signal");
                    return 1;
                }
            }
            i = 2;
        } else if let Some(spec) = first.strip_prefix('-') {
            if spec.parse::<i32>().is_ok() || SIGNALS.iter().any(|(n, _)| *n == spec.trim_start_matches("SIG")) {
                match parse_signal(spec) {
                    Some(s) => sig = s,
                    None => {
                        error("kill", format!("invalid signal '{spec}'"));
                        return 1;
                    }
                }
                i = 1;
            }
        }
    }
    if i >= args.len() {
        error("kill", "usage: kill [-SIGNAL | -s SIGNAL] pid...   (kill -l lists signals)");
        return 1;
    }
    let mut status = 0;
    for a in &args[i..] {
        match a.parse::<i32>() {
            Ok(pid) => {
                // SAFETY: kill(2) takes plain integers.
                if unsafe { libc::kill(pid, sig) } != 0 {
                    error("kill", format!("{pid}: {}", std::io::Error::last_os_error()));
                    status = 1;
                }
            }
            Err(_) => {
                error("kill", format!("invalid process id '{a}'"));
                status = 1;
            }
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_escapes() {
        assert_eq!(unescape("a\\tb\\n"), (b"a\tb\n".to_vec(), false));
        assert_eq!(unescape("x\\\\y"), (b"x\\y".to_vec(), false));
        assert_eq!(unescape("stop\\cnever"), (b"stop".to_vec(), true));
        assert_eq!(unescape("\\0101\\x41\\x4a"), (b"AAJ".to_vec(), false));
        assert_eq!(unescape("\\q trailing\\"), (b"\\q trailing\\".to_vec(), false));
        assert_eq!(unescape("\\xzz"), (b"\\xzz".to_vec(), false));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("0"), Some(Duration::ZERO));
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("-1"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn signals() {
        assert_eq!(parse_signal("TERM"), Some(15));
        assert_eq!(parse_signal("SIGKILL"), Some(9));
        assert_eq!(parse_signal("kill"), Some(9));
        assert_eq!(parse_signal("9"), Some(9));
        assert_eq!(parse_signal("0"), Some(0));
        assert_eq!(parse_signal("NOPE"), None);
        assert_eq!(parse_signal("99"), None);
    }
}
