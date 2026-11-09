//! ls: -a -A -l -d -1 -h (always one entry per line, sorted bytewise).

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use flint_auth::files::{parse_group, parse_passwd};

use crate::util::{self, error};

/// `drwxr-xr-x`, with setuid/setgid/sticky shown as s/S/t/T.
pub fn mode_string(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o020000 => 'c',
        0o060000 => 'b',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(kind);
    let triplet = |r: u32, w: u32, x: u32, special: bool, lower: char, upper: char, s: &mut String| {
        s.push(if mode & r != 0 { 'r' } else { '-' });
        s.push(if mode & w != 0 { 'w' } else { '-' });
        s.push(match (mode & x != 0, special) {
            (true, true) => lower,
            (false, true) => upper,
            (true, false) => 'x',
            (false, false) => '-',
        });
    };
    triplet(0o400, 0o200, 0o100, mode & 0o4000 != 0, 's', 'S', &mut s);
    triplet(0o040, 0o020, 0o010, mode & 0o2000 != 0, 's', 'S', &mut s);
    triplet(0o004, 0o002, 0o001, mode & 0o1000 != 0, 't', 'T', &mut s);
    s
}

/// UTC `YYYY-MM-DD HH:MM` for seconds since the Unix epoch.
pub fn format_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

/// Days since 1970-01-01 to (year, month, day); proleptic Gregorian (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe as i64 + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `4.0K`, `1.5M`, ... (powers of 1024, one decimal; plain bytes below 1024).
pub fn human_size(n: u64) -> String {
    if n < 1024 {
        return n.to_string();
    }
    let units = ['K', 'M', 'G', 'T', 'P'];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1}{}", units[u - 1])
}

struct Names {
    users: HashMap<u32, String>,
    groups: HashMap<u32, String>,
}

impl Names {
    fn load() -> Self {
        let users = parse_passwd(&fs::read_to_string("/etc/passwd").unwrap_or_default())
            .into_iter()
            .map(|u| (u.uid, u.name))
            .collect();
        let groups = parse_group(&fs::read_to_string("/etc/group").unwrap_or_default())
            .into_iter()
            .map(|g| (g.gid, g.name))
            .collect();
        Names { users, groups }
    }

    fn user(&self, uid: u32) -> String {
        self.users.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
    }

    fn group(&self, gid: u32) -> String {
        self.groups.get(&gid).cloned().unwrap_or_else(|| gid.to_string())
    }
}

struct Options {
    long: bool,
    human: bool,
}

fn entry_line(name: &str, path: &Path, opts: &Options, names: &Names) -> io::Result<String> {
    if !opts.long {
        return Ok(name.to_string());
    }
    let m = fs::symlink_metadata(path)?;
    let size = if opts.human { human_size(m.size()) } else { m.size().to_string() };
    let mut line = format!(
        "{} {:>3} {:<8} {:<8} {:>8} {} {}",
        mode_string(m.mode()),
        m.nlink(),
        names.user(m.uid()),
        names.group(m.gid()),
        size,
        format_time(m.mtime()),
        name
    );
    if m.file_type().is_symlink() {
        if let Ok(t) = fs::read_link(path) {
            line.push_str(&format!(" -> {}", t.display()));
        }
    }
    Ok(line)
}

pub fn ls(args: &[String]) -> i32 {
    let p = match util::parse(args, "aAld1h", "") {
        Ok(p) => p,
        Err(e) => {
            error("ls", e);
            return 2;
        }
    };
    let opts = Options { long: p.has('l'), human: p.has('h') };
    let names = Names::load();
    let operands = if p.operands.is_empty() { vec![".".to_string()] } else { p.operands.clone() };

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut status = 0;
    let mut first = true;

    // Files and `-d` directories are listed first, then each directory's contents.
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for op in &operands {
        match fs::metadata(op).or_else(|_| fs::symlink_metadata(op)) {
            Err(e) => {
                error("ls", format!("cannot access '{op}': {e}"));
                status = 2;
            }
            Ok(m) if m.is_dir() && !p.has('d') => dirs.push(op.clone()),
            Ok(_) => files.push(op.clone()),
        }
    }
    files.sort();
    for f in &files {
        match entry_line(f, Path::new(f), &opts, &names) {
            Ok(l) => {
                if let Err(e) = writeln!(out, "{l}") {
                    return util::finish_output("ls", Err(e));
                }
            }
            Err(e) => {
                error("ls", format!("cannot access '{f}': {e}"));
                status = 2;
            }
        }
        first = false;
    }
    dirs.sort();
    for d in &dirs {
        let entries = match fs::read_dir(d) {
            Ok(rd) => rd,
            Err(e) => {
                error("ls", format!("cannot open directory '{d}': {e}"));
                status = 2;
                continue;
            }
        };
        let mut list: Vec<(Vec<u8>, String)> = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name();
            list.push((name.as_bytes().to_vec(), name.to_string_lossy().into_owned()));
        }
        if p.has('a') {
            list.push((b".".to_vec(), ".".to_string()));
            list.push((b"..".to_vec(), "..".to_string()));
        }
        if !(p.has('a') || p.has('A')) {
            list.retain(|(raw, _)| !raw.starts_with(b"."));
        }
        list.sort();
        let r: io::Result<()> = (|| {
            if operands.len() > 1 {
                if !first {
                    writeln!(out)?;
                }
                writeln!(out, "{d}:")?;
            }
            first = false;
            for (_, name) in &list {
                match entry_line(name, &Path::new(d).join(name), &opts, &names) {
                    Ok(l) => writeln!(out, "{l}")?,
                    Err(e) => {
                        error("ls", format!("cannot access '{name}': {e}"));
                        status = 2;
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = r {
            return util::finish_output("ls", Err(e));
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_strings() {
        assert_eq!(mode_string(0o100644), "-rw-r--r--");
        assert_eq!(mode_string(0o040755), "drwxr-xr-x");
        assert_eq!(mode_string(0o120777), "lrwxrwxrwx");
        assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
        assert_eq!(mode_string(0o104644), "-rwSr--r--");
        assert_eq!(mode_string(0o042755), "drwxr-sr-x");
        assert_eq!(mode_string(0o041777), "drwxrwxrwt");
        assert_eq!(mode_string(0o041776), "drwxrwxrwT");
        assert_eq!(mode_string(0o020620), "crw--w----");
        assert_eq!(mode_string(0o100000), "----------");
    }

    // Expected strings come from Python's datetime.
    #[test]
    fn times() {
        assert_eq!(format_time(0), "1970-01-01 00:00");
        assert_eq!(format_time(951_782_400), "2000-02-29 00:00");
        assert_eq!(format_time(1_794_149_588), "2026-11-08 14:53");
        assert_eq!(format_time(1_709_164_799), "2024-02-28 23:59");
        assert_eq!(format_time(-86_400), "1969-12-31 00:00");
        assert_eq!(format_time(4_102_444_800), "2100-01-01 00:00");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0");
        assert_eq!(human_size(1023), "1023");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(4096), "4.0K");
        assert_eq!(human_size(1024 * 1024), "1.0M");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0G");
    }
}
