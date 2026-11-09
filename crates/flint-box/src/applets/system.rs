//! System applets: mount, umount, ps.

use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;

use flint_auth::files::parse_passwd;

use crate::util::{self, error};

// ---------------------------------------------------------------- mount

/// Split a `-o` option string into mount(2) flags and the leftover filesystem data string.
pub fn parse_mount_opts(opts: &str) -> (libc::c_ulong, String) {
    let mut flags: libc::c_ulong = 0;
    let mut data: Vec<&str> = Vec::new();
    for o in opts.split(',').filter(|o| !o.is_empty()) {
        match o {
            "ro" => flags |= libc::MS_RDONLY,
            "rw" => flags &= !libc::MS_RDONLY,
            "nosuid" => flags |= libc::MS_NOSUID,
            "suid" => flags &= !libc::MS_NOSUID,
            "nodev" => flags |= libc::MS_NODEV,
            "dev" => flags &= !libc::MS_NODEV,
            "noexec" => flags |= libc::MS_NOEXEC,
            "exec" => flags &= !libc::MS_NOEXEC,
            "sync" => flags |= libc::MS_SYNCHRONOUS,
            "async" => flags &= !libc::MS_SYNCHRONOUS,
            "noatime" => flags |= libc::MS_NOATIME,
            "nodiratime" => flags |= libc::MS_NODIRATIME,
            "relatime" => flags |= libc::MS_RELATIME,
            "remount" => flags |= libc::MS_REMOUNT,
            "bind" => flags |= libc::MS_BIND,
            "rbind" => flags |= libc::MS_BIND | libc::MS_REC,
            "defaults" | "auto" | "noauto" | "user" | "nouser" => {}
            other => data.push(other),
        }
    }
    (flags, data.join(","))
}

/// `proc on /proc type proc (rw,nosuid)` from a /proc/self/mounts line.
pub fn format_mount_line(line: &str) -> Option<String> {
    let f: Vec<&str> = line.split_whitespace().collect();
    (f.len() >= 4).then(|| format!("{} on {} type {} ({})", f[0], f[1], f[2], f[3]))
}

pub fn mount(args: &[String]) -> i32 {
    // `--bind` and `--rbind` are long options; translate them before short-option parsing.
    let mut extra_opts = String::new();
    let rest: Vec<String> = args
        .iter()
        .filter(|a| match a.as_str() {
            "--bind" => {
                extra_opts.push_str(",bind");
                false
            }
            "--rbind" => {
                extra_opts.push_str(",rbind");
                false
            }
            _ => true,
        })
        .cloned()
        .collect();
    let p = match util::parse(&rest, "rwn", "to") {
        Ok(p) => p,
        Err(e) => {
            error("mount", e);
            return 1;
        }
    };

    if p.operands.is_empty() && p.value('t').is_none() && p.value('o').is_none() {
        let text = match fs::read_to_string("/proc/self/mounts") {
            Ok(t) => t,
            Err(e) => {
                error("mount", format!("cannot read /proc/self/mounts: {e}"));
                return 1;
            }
        };
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let r: io::Result<()> = text.lines().filter_map(format_mount_line).try_for_each(|l| writeln!(out, "{l}"));
        return util::finish_output("mount", r);
    }

    let mut opts = p.value('o').unwrap_or("").to_string();
    opts.push_str(&extra_opts);
    if p.has('r') {
        opts.push_str(",ro");
    }
    if p.has('w') {
        opts.push_str(",rw");
    }
    let (flags, data) = parse_mount_opts(&opts);

    let (source, target) = match p.operands.as_slice() {
        [target] if flags & libc::MS_REMOUNT != 0 => (String::new(), target.clone()),
        [source, target] => (source.clone(), target.clone()),
        _ => {
            error("mount", "usage: mount [-t TYPE] [-o OPTIONS] SOURCE TARGET   (or: mount -o remount,OPTIONS TARGET)");
            return 1;
        }
    };
    let fstype = p.value('t');
    if fstype.is_none() && flags & (libc::MS_REMOUNT | libc::MS_BIND) == 0 {
        error("mount", "the filesystem type must be given with -t");
        return 1;
    }

    let to_c = |s: &str| CString::new(s).map_err(|_| "string contains a NUL byte".to_string());
    let (Ok(src_c), Ok(tgt_c)) = (to_c(&source), to_c(&target)) else {
        error("mount", "invalid argument");
        return 1;
    };
    let fs_c = fstype.map(to_c).transpose();
    let Ok(fs_c) = fs_c else {
        error("mount", "invalid filesystem type");
        return 1;
    };
    let data_c = to_c(&data).unwrap_or_default();
    let src_ptr = if source.is_empty() { std::ptr::null() } else { src_c.as_ptr() };
    let data_ptr = if data.is_empty() { std::ptr::null() } else { data_c.as_ptr().cast() };

    // SAFETY: all pointers are valid NUL-terminated strings (or null) that outlive the call.
    let rc = unsafe { libc::mount(src_ptr, tgt_c.as_ptr(), fs_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), flags, data_ptr) };
    if rc != 0 {
        error("mount", format!("mounting {source} on {target} failed: {}", io::Error::last_os_error()));
        return 1;
    }
    0
}

pub fn umount(args: &[String]) -> i32 {
    let Ok(p) = util::parse(args, "lfr", "") else {
        error("umount", "usage: umount [-l] [-f] TARGET...");
        return 1;
    };
    if p.operands.is_empty() {
        error("umount", "usage: umount [-l] [-f] TARGET...");
        return 1;
    }
    let mut flags = 0;
    if p.has('l') {
        flags |= libc::MNT_DETACH;
    }
    if p.has('f') {
        flags |= libc::MNT_FORCE;
    }
    let mut status = 0;
    for t in &p.operands {
        let Ok(c) = CString::new(t.as_str()) else {
            error("umount", format!("{t}: invalid name"));
            status = 1;
            continue;
        };
        // SAFETY: valid C string.
        if unsafe { libc::umount2(c.as_ptr(), flags) } != 0 {
            error("umount", format!("{t}: {}", io::Error::last_os_error()));
            status = 1;
        }
    }
    status
}

// ---------------------------------------------------------------- ps

#[derive(Debug, PartialEq, Eq)]
pub struct Stat {
    pub pid: u32,
    pub comm: String,
    pub state: char,
    pub ppid: u32,
    /// utime + stime in clock ticks
    pub ticks: u64,
}

/// Parse one /proc/PID/stat line. The command name sits in parentheses and may itself
/// contain spaces and parentheses, so the fields after it start at the *last* `)`.
pub fn parse_stat(line: &str) -> Option<Stat> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let pid = line[..open].trim().parse().ok()?;
    let rest: Vec<&str> = line[close + 1..].split_whitespace().collect();
    // rest[0] = state, rest[1] = ppid, ..., rest[11] = utime, rest[12] = stime
    if rest.len() < 13 {
        return None;
    }
    Some(Stat {
        pid,
        comm: line[open + 1..close].to_string(),
        state: rest[0].chars().next()?,
        ppid: rest[1].parse().ok()?,
        ticks: rest[11].parse::<u64>().ok()? + rest[12].parse::<u64>().ok()?,
    })
}

pub fn ps(_args: &[String]) -> i32 {
    let users: std::collections::HashMap<u32, String> =
        parse_passwd(&fs::read_to_string("/etc/passwd").unwrap_or_default()).into_iter().map(|u| (u.uid, u.name)).collect();
    // SAFETY: sysconf has no preconditions.
    let hz = match unsafe { libc::sysconf(libc::_SC_CLK_TCK) } {
        n if n > 0 => n as u64,
        _ => 100,
    };

    let mut rows: Vec<(u32, String, u64, String)> = Vec::new();
    let Ok(rd) = fs::read_dir("/proc") else {
        error("ps", "cannot read /proc (is it mounted?)");
        return 1;
    };
    for entry in rd.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let dir = format!("/proc/{pid}");
        let Ok(stat_text) = fs::read_to_string(format!("{dir}/stat")) else { continue }; // exited meanwhile
        let Some(stat) = parse_stat(&stat_text) else { continue };
        let uid = fs::metadata(&dir).map(|m| m.uid()).unwrap_or(0);
        let user = users.get(&uid).cloned().unwrap_or_else(|| uid.to_string());
        let cmdline = fs::read(format!("{dir}/cmdline")).unwrap_or_default();
        let command = if cmdline.is_empty() {
            format!("[{}]", stat.comm)
        } else {
            let text = String::from_utf8_lossy(&cmdline);
            text.trim_end_matches('\0').replace('\0', " ")
        };
        rows.push((pid, user, stat.ticks / hz, command));
    }
    rows.sort_by_key(|r| r.0);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let r: io::Result<()> = (|| {
        writeln!(out, "{:>5} {:<8} {:>5}  COMMAND", "PID", "USER", "TIME")?;
        for (pid, user, secs, command) in &rows {
            writeln!(out, "{pid:>5} {user:<8.8} {:>2}:{:02}  {command}", secs / 60, secs % 60)?;
        }
        Ok(())
    })();
    util::finish_output("ps", r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_options() {
        assert_eq!(parse_mount_opts(""), (0, String::new()));
        assert_eq!(parse_mount_opts("ro,nosuid,nodev,noexec"), (libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC, String::new()));
        assert_eq!(parse_mount_opts("remount,rw").0, libc::MS_REMOUNT);
        assert_eq!(parse_mount_opts("remount,ro").0, libc::MS_REMOUNT | libc::MS_RDONLY);
        assert_eq!(parse_mount_opts("bind").0, libc::MS_BIND);
        assert_eq!(parse_mount_opts("rbind").0, libc::MS_BIND | libc::MS_REC);
        assert_eq!(parse_mount_opts("defaults"), (0, String::new()));
        let (flags, data) = parse_mount_opts("nosuid,size=10m,mode=1777,noexec");
        assert_eq!(flags, libc::MS_NOSUID | libc::MS_NOEXEC);
        assert_eq!(data, "size=10m,mode=1777");
        assert_eq!(parse_mount_opts("ro,rw").0, 0, "later options win");
    }

    #[test]
    fn mount_lines() {
        assert_eq!(
            format_mount_line("proc /proc proc rw,nosuid,nodev 0 0").as_deref(),
            Some("proc on /proc type proc (rw,nosuid,nodev)")
        );
        assert_eq!(format_mount_line("short line"), None);
    }

    #[test]
    fn stat_parsing() {
        let line = "42 (my (weird) cmd) S 1 42 42 0 -1 4194560 100 0 0 0 7 5 0 0 20 0 1 0 12345 1000000 100 18446744073709551615";
        let s = parse_stat(line).unwrap();
        assert_eq!(s, Stat { pid: 42, comm: "my (weird) cmd".into(), state: 'S', ppid: 1, ticks: 12 });
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_stat("1 (init) S 0"), None);
        assert_eq!(parse_stat("x (init) S 0 1 1 0 -1 0 0 0 0 0 1 1"), None);
    }
}
