//! find, du, df, stat, which, readlink, basename, dirname, mknod, mkfifo, date, dmesg, free.

use std::collections::HashSet;
use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use flint_auth::files::{parse_group, parse_passwd};

use crate::util::{error, finish_output, parse};

// ---------------------------------------------------------------------------------------
// Shared helpers

pub fn human(bytes: u64) -> String {
    let units = ["", "K", "M", "G", "T", "P"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < units.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}")
    } else if v < 10.0 {
        format!("{:.1}{}", (v * 10.0).ceil() / 10.0, units[u])
    } else {
        format!("{:.0}{}", v.ceil(), units[u])
    }
}

fn user_name(uid: u32) -> String {
    let text = fs::read_to_string("/etc/passwd").unwrap_or_default();
    parse_passwd(&text).into_iter().find(|u| u.uid == uid).map_or(uid.to_string(), |u| u.name)
}

fn group_name(gid: u32) -> String {
    let text = fs::read_to_string("/etc/group").unwrap_or_default();
    parse_group(&text).into_iter().find(|g| g.gid == gid).map_or(gid.to_string(), |g| g.name)
}

pub fn mode_string(mode: u32) -> String {
    let t = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o020000 => 'c',
        0o060000 => 'b',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(t);
    let bits = [(0o400, 'r'), (0o200, 'w'), (0o100, 'x'), (0o040, 'r'), (0o020, 'w'), (0o010, 'x'), (0o004, 'r'), (0o002, 'w'), (0o001, 'x')];
    for (k, (b, c)) in bits.iter().enumerate() {
        let mut ch = if mode & b != 0 { *c } else { '-' };
        if k == 2 && mode & 0o4000 != 0 {
            ch = if ch == 'x' { 's' } else { 'S' };
        }
        if k == 5 && mode & 0o2000 != 0 {
            ch = if ch == 'x' { 's' } else { 'S' };
        }
        if k == 8 && mode & 0o1000 != 0 {
            ch = if ch == 'x' { 't' } else { 'T' };
        }
        s.push(ch);
    }
    s
}

// ---------------------------------------------------------------------------------------
// Time: civil dates without a C library

pub fn days_to_civil(z: i64) -> (i64, u32, u32) {
    // Howard Hinnant's algorithm
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn civil_to_days(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = u64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// The local time zone: (offset from UTC in seconds, abbreviation) at time `t`.
/// From TZ (`UTC`, `UTC+2`, `CET-1` POSIX-style fixed offsets, or a TZif file path with
/// a leading ':') or /etc/localtime (TZif); UTC when neither is there.
pub fn local_zone(t: i64) -> (i64, String) {
    if let Ok(tz) = std::env::var("TZ") {
        if let Some(path) = tz.strip_prefix(':') {
            if let Some(z) = fs::read(path).ok().and_then(|d| tzif_lookup(&d, t)) {
                return z;
            }
        }
        if let Some(z) = posix_fixed(&tz) {
            return z;
        }
        if let Some(z) = fs::read(format!("/usr/share/zoneinfo/{tz}")).ok().and_then(|d| tzif_lookup(&d, t)) {
            return z;
        }
        return (0, "UTC".into());
    }
    fs::read("/etc/localtime").ok().and_then(|d| tzif_lookup(&d, t)).unwrap_or((0, "UTC".into()))
}

/// `NAME[+-]H[:MM]` (POSIX: the sign is west-positive, so CET-1 is UTC+1).
fn posix_fixed(tz: &str) -> Option<(i64, String)> {
    let name_end = tz.find(|c: char| c == '+' || c == '-' || c.is_ascii_digit()).unwrap_or(tz.len());
    let name = &tz[..name_end];
    if name.len() < 3 || !name.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let rest = &tz[name_end..];
    if rest.is_empty() {
        return Some((0, name.to_string()));
    }
    let off_end = rest.find(|c: char| c.is_ascii_alphabetic() || c == ',').unwrap_or(rest.len());
    let off = &rest[..off_end];
    let (sign, num) = match off.as_bytes()[0] {
        b'-' => (1, &off[1..]),
        b'+' => (-1, &off[1..]),
        _ => (-1, off),
    };
    let mut parts = num.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next().map_or(Some(0), |x| x.parse().ok())?;
    Some((sign * (h * 3600 + m * 60), name.to_string()))
}

/// Offset and abbreviation at time `t` from TZif data (version 1 section is enough for
/// dates up to 2037; later versions add 64-bit data, which we use when present).
fn tzif_lookup(d: &[u8], t: i64) -> Option<(i64, String)> {
    if d.len() < 44 || &d[0..4] != b"TZif" {
        return None;
    }
    let be32 = |o: usize| -> Option<u32> { d.get(o..o + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap())) };
    let counts = |o: usize| -> Option<[usize; 6]> {
        let mut c = [0usize; 6];
        for (k, x) in c.iter_mut().enumerate() {
            *x = be32(o + 20 + 4 * k)? as usize;
        }
        Some(c)
    };
    let [isutc, isstd, leap, timecnt, typecnt, charcnt] = counts(0)?;
    let v1_len = 44 + timecnt * 5 + typecnt * 6 + charcnt + leap * 8 + isstd + isutc;
    let (base, size, c) = if d[4] >= b'2' && d.len() > v1_len + 44 {
        (v1_len, 8usize, counts(v1_len)?)
    } else {
        (0, 4usize, [isutc, isstd, leap, timecnt, typecnt, charcnt])
    };
    let [_, _, _, timecnt, typecnt, _charcnt] = c;
    let times_at = base + 44;
    let idx_at = times_at + timecnt * size;
    let types_at = idx_at + timecnt;
    let chars_at = types_at + typecnt * 6;
    let time = |k: usize| -> Option<i64> {
        let o = times_at + k * size;
        Some(if size == 8 { i64::from_be_bytes(d.get(o..o + 8)?.try_into().ok()?) } else { i64::from(be32(o)? as i32) })
    };
    let mut ty = 0usize;
    // First standard-time type if before all transitions.
    for k in 0..timecnt {
        if time(k)? <= t {
            ty = *d.get(idx_at + k)? as usize;
        } else {
            break;
        }
    }
    if timecnt == 0 || time(0)? > t {
        ty = (0..typecnt).find(|&k| d.get(types_at + k * 6 + 4) == Some(&0)).unwrap_or(0);
    }
    let rec = types_at + ty * 6;
    let off = i64::from(i32::from_be_bytes(d.get(rec..rec + 4)?.try_into().ok()?));
    let abbr_i = *d.get(rec + 5)? as usize;
    let abbr: String = d[chars_at + abbr_i..].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
    Some((off, abbr))
}

const DAYS: [&str; 7] = ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

pub struct Tm {
    pub year: i64,
    pub mon: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    pub wday: usize, // 0 = Thursday (1970-01-01), see DAYS
    pub yday: u32,
    pub off: i64,
    pub zone: String,
    pub epoch: i64,
    pub nanos: u32,
}

pub fn broken_down(epoch: i64, nanos: u32, utc: bool) -> Tm {
    let (off, zone) = if utc { (0, "UTC".to_string()) } else { local_zone(epoch) };
    let t = epoch + off;
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400) as u32;
    let (year, mon, day) = days_to_civil(days);
    Tm {
        year,
        mon,
        day,
        hour: secs / 3600,
        min: secs / 60 % 60,
        sec: secs % 60,
        wday: days.rem_euclid(7) as usize,
        yday: (days - civil_to_days(year, 1, 1)) as u32,
        off,
        zone,
        epoch,
        nanos,
    }
}

pub fn strftime(fmt: &str, tm: &Tm) -> String {
    let mut out = String::new();
    let mut it = fmt.chars().peekable();
    // weekday with Sunday = 0
    let wd_sun = (tm.wday + 4) % 7;
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // GNU padding flags: '-' no padding, '_' spaces, '0' zeros
        let mut pad: Option<char> = None;
        if let Some(&f) = it.peek() {
            if f == '-' || f == '_' || f == '0' {
                pad = Some(f);
                it.next();
            }
        }
        let num = |v: i64, w: usize, def: char| -> String {
            match pad.unwrap_or(def) {
                '-' => v.to_string(),
                '_' => format!("{v:>w$}"),
                _ => format!("{v:0w$}"),
            }
        };
        let Some(spec) = it.next() else {
            out.push('%');
            break;
        };
        let s = match spec {
            'Y' => tm.year.to_string(),
            'C' => num(tm.year / 100, 2, '0'),
            'y' => num(tm.year.rem_euclid(100), 2, '0'),
            'm' => num(i64::from(tm.mon), 2, '0'),
            'd' => num(i64::from(tm.day), 2, '0'),
            'e' => num(i64::from(tm.day), 2, '_'),
            'H' => num(i64::from(tm.hour), 2, '0'),
            'k' => num(i64::from(tm.hour), 2, '_'),
            'I' => num(i64::from((tm.hour + 11) % 12 + 1), 2, '0'),
            'l' => num(i64::from((tm.hour + 11) % 12 + 1), 2, '_'),
            'M' => num(i64::from(tm.min), 2, '0'),
            'S' => num(i64::from(tm.sec), 2, '0'),
            'N' => format!("{:09}", tm.nanos),
            'p' => if tm.hour < 12 { "AM" } else { "PM" }.into(),
            'P' => if tm.hour < 12 { "am" } else { "pm" }.into(),
            'j' => num(i64::from(tm.yday + 1), 3, '0'),
            'a' => DAYS[tm.wday][..3].into(),
            'A' => DAYS[tm.wday].into(),
            'b' | 'h' => MONTHS[tm.mon as usize - 1][..3].into(),
            'B' => MONTHS[tm.mon as usize - 1].into(),
            'u' => (if wd_sun == 0 { 7 } else { wd_sun }).to_string(),
            'w' => wd_sun.to_string(),
            's' => tm.epoch.to_string(),
            'Z' => tm.zone.clone(),
            'z' => {
                let o = tm.off.abs();
                format!("{}{:02}{:02}", if tm.off < 0 { '-' } else { '+' }, o / 3600, o / 60 % 60)
            }
            'D' => strftime("%m/%d/%y", tm),
            'F' => strftime("%Y-%m-%d", tm),
            'T' => strftime("%H:%M:%S", tm),
            'R' => strftime("%H:%M", tm),
            'r' => strftime("%I:%M:%S %p", tm),
            'c' => strftime("%a %b %e %H:%M:%S %Y", tm),
            'x' => strftime("%m/%d/%y", tm),
            'X' => strftime("%H:%M:%S", tm),
            'n' => "\n".into(),
            't' => "\t".into(),
            '%' => "%".into(),
            'U' => num(((tm.yday as i64) + 7 - wd_sun as i64) / 7, 2, '0'),
            'V' => {
                // ISO 8601 week number
                let wd_mon = (wd_sun + 6) % 7; // Monday = 0
                let w = (i64::from(tm.yday) - wd_mon as i64 + 10) / 7;
                num(w.max(1), 2, '0')
            }
            other => format!("%{other}"),
        };
        out.push_str(&s);
    }
    out
}

/// Parse a date for `date -d` / `date -s`: `@EPOCH`, `YYYY-MM-DD[ HH:MM[:SS]]`,
/// `HH:MM[:SS]` (today), `now`.
pub fn parse_date(s: &str, now: i64, utc: bool) -> Option<i64> {
    let s = s.trim();
    if s == "now" {
        return Some(now);
    }
    if let Some(e) = s.strip_prefix('@') {
        return e.parse().ok();
    }
    let (date, time) = match s.split_once([' ', 'T']) {
        Some((d, t)) => (Some(d), Some(t)),
        None if s.contains('-') => (Some(s), None),
        None => (None, Some(s)),
    };
    let today = broken_down(now, 0, utc);
    let (y, m, d) = match date {
        Some(d) => {
            let p: Vec<&str> = d.split('-').collect();
            if p.len() != 3 {
                return None;
            }
            (p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?)
        }
        None => (today.year, today.mon, today.day),
    };
    let (hh, mm, ss) = match time {
        Some(t) => {
            let t = t.trim_end_matches('Z');
            let p: Vec<&str> = t.split(':').collect();
            if p.len() < 2 || p.len() > 3 {
                return None;
            }
            (p[0].parse::<i64>().ok()?, p[1].parse::<i64>().ok()?, p.get(2).map_or(Some(0), |x| x.parse::<i64>().ok())?)
        }
        None => (0, 0, 0),
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let local = civil_to_days(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss;
    if utc {
        return Some(local);
    }
    // Local time: subtract the zone's offset at that moment (twice, to settle DST edges).
    let off = local_zone(local).0;
    let off = local_zone(local - off).0;
    Some(local - off)
}

pub fn date(args: &[String]) -> i32 {
    let mut utc = false;
    let mut fmt: Option<String> = None;
    let mut when: Option<String> = None;
    let mut set: Option<String> = None;
    let mut iso: Option<String> = None;
    let mut rfc = false;
    let mut file_time: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        match a.as_str() {
            "-u" | "--utc" | "--universal" => utc = true,
            "-R" | "--rfc-email" => rfc = true,
            "-d" | "--date" => {
                when = args.get(i).cloned();
                i += 1;
            }
            "-s" | "--set" => {
                set = args.get(i).cloned();
                i += 1;
            }
            "-r" | "--reference" => {
                file_time = args.get(i).cloned();
                i += 1;
            }
            s if s.starts_with("-I") => iso = Some(s[2..].to_string()),
            s if s.starts_with("--date=") => when = Some(s[7..].to_string()),
            s if s.starts_with("--iso-8601") => iso = Some(s.strip_prefix("--iso-8601=").unwrap_or("").to_string()),
            s if s.starts_with('+') => fmt = Some(s[1..].to_string()),
            _ => {
                error("date", format!("invalid argument '{a}'\nusage: date [-u] [-d DATE | -r FILE] [-s DATE] [+FORMAT]"));
                return 1;
            }
        }
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let (mut epoch, mut nanos) = (now.as_secs() as i64, now.subsec_nanos());
    if let Some(f) = &file_time {
        match fs::metadata(f) {
            Ok(m) => {
                epoch = m.mtime();
                nanos = m.mtime_nsec() as u32;
            }
            Err(e) => {
                error("date", format!("{f}: {e}"));
                return 1;
            }
        }
    }
    if let Some(w) = when.as_ref().or(set.as_ref()) {
        match parse_date(w, epoch, utc) {
            Some(t) => {
                epoch = t;
                nanos = 0;
            }
            None => {
                error("date", format!("invalid date '{w}' (use @SECONDS or YYYY-MM-DD [HH:MM[:SS]])"));
                return 1;
            }
        }
    }
    if set.is_some() {
        let tv = libc::timespec { tv_sec: epoch as libc::time_t, tv_nsec: 0 };
        // SAFETY: clock_settime with a valid timespec.
        if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &tv) } != 0 {
            error("date", format!("cannot set date: {}", io::Error::last_os_error()));
            return 1;
        }
    }
    let tm = broken_down(epoch, nanos, utc);
    let f = if let Some(f) = fmt {
        f
    } else if rfc {
        "%a, %d %b %Y %H:%M:%S %z".into()
    } else if let Some(p) = iso {
        match p.as_str() {
            "" | "date" => "%Y-%m-%d".into(),
            "hours" => "%Y-%m-%dT%H%:z".into(),
            "minutes" => "%Y-%m-%dT%H:%M%:z".into(),
            _ => "%Y-%m-%dT%H:%M:%S%:z".into(),
        }
    } else {
        "%a %b %e %H:%M:%S %Z %Y".into()
    };
    let mut s = strftime(&f.replace("%:z", "\u{1}"), &tm);
    if s.contains('\u{1}') {
        let o = tm.off.abs();
        s = s.replace('\u{1}', &format!("{}{:02}:{:02}", if tm.off < 0 { '-' } else { '+' }, o / 3600, o / 60 % 60));
    }
    println!("{s}");
    0
}

// ---------------------------------------------------------------------------------------
// dmesg, free, df, du

pub fn dmesg(args: &[String]) -> i32 {
    let p = match parse(args, "crH", "n") {
        Ok(p) => p,
        Err(e) => {
            error("dmesg", format!("{e}\nusage: dmesg [-c] [-r] [-n LEVEL]"));
            return 1;
        }
    };
    if let Some(level) = p.value('n') {
        let Ok(l) = level.parse::<i32>() else {
            error("dmesg", "-n needs a number 1-8");
            return 1;
        };
        // SAFETY: SYSLOG_ACTION_CONSOLE_LEVEL takes the level as `len`.
        if unsafe { libc::klogctl(8, std::ptr::null_mut(), l) } < 0 {
            error("dmesg", io::Error::last_os_error());
            return 1;
        }
        return 0;
    }
    // SAFETY: SYSLOG_ACTION_SIZE_BUFFER takes no buffer.
    let size = unsafe { libc::klogctl(10, std::ptr::null_mut(), 0) }.max(1 << 16) as usize;
    let mut buf = vec![0u8; size];
    let action = if p.has('c') { 4 } else { 3 };
    // SAFETY: read into our buffer of `size` bytes.
    let n = unsafe { libc::klogctl(action, buf.as_mut_ptr().cast(), size as i32) };
    if n < 0 {
        let e = io::Error::last_os_error();
        error("dmesg", if e.raw_os_error() == Some(libc::EPERM) { "reading the kernel log needs root (or kernel.dmesg_restrict=0)".to_string() } else { e.to_string() });
        return 1;
    }
    buf.truncate(n as usize);
    let text = String::from_utf8_lossy(&buf);
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if p.has('r') {
            out.push_str(line);
        } else {
            // strip the "<6>" priority prefix
            let l = if line.starts_with('<') { line.split_once('>').map_or(line, |(_, r)| r) } else { line };
            out.push_str(l);
        }
        out.push('\n');
    }
    let mut o = io::stdout().lock();
    finish_output("dmesg", o.write_all(out.as_bytes()).and_then(|_| o.flush()))
}

fn meminfo() -> io::Result<std::collections::HashMap<String, u64>> {
    let text = fs::read_to_string("/proc/meminfo")?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            let kb: u64 = v.split_whitespace().next()?.parse().ok()?;
            Some((k.to_string(), kb * 1024))
        })
        .collect())
}

pub fn free(args: &[String]) -> i32 {
    let p = match parse(args, "hbkmgw", "") {
        Ok(p) => p,
        Err(e) => {
            error("free", format!("{e}\nusage: free [-h | -b | -k | -m | -g]"));
            return 1;
        }
    };
    let m = match meminfo() {
        Ok(m) => m,
        Err(e) => {
            error("free", format!("/proc/meminfo: {e}"));
            return 1;
        }
    };
    let g = |k: &str| m.get(k).copied().unwrap_or(0);
    let total = g("MemTotal");
    let free = g("MemFree");
    let buffcache = g("Buffers") + g("Cached") + g("SReclaimable");
    let avail = m.get("MemAvailable").copied().unwrap_or(free + buffcache);
    let used = total.saturating_sub(free + buffcache);
    let shared = g("Shmem");
    let (st, sf) = (g("SwapTotal"), g("SwapFree"));
    let fmt = |v: u64| -> String {
        if p.has('h') {
            human(v)
        } else if p.has('b') {
            v.to_string()
        } else if p.has('m') {
            (v >> 20).to_string()
        } else if p.has('g') {
            (v >> 30).to_string()
        } else {
            (v >> 10).to_string()
        }
    };
    println!("{:>14} {:>11} {:>11} {:>11} {:>11} {:>11}", "total", "used", "free", "shared", "buff/cache", "available");
    println!("{:<6}{:>9} {:>11} {:>11} {:>11} {:>11} {:>11}", "Mem:", fmt(total), fmt(used), fmt(free), fmt(shared), fmt(buffcache), fmt(avail));
    println!("{:<6}{:>9} {:>11} {:>11}", "Swap:", fmt(st), fmt(st - sf.min(st)), fmt(sf));
    0
}

fn statvfs(path: &str) -> io::Result<libc::statvfs> {
    let c = CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: zeroed statvfs filled by the kernel.
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(s)
    }
}

const PSEUDO_FS: &[&str] = &["proc", "sysfs", "devpts", "cgroup", "cgroup2", "securityfs", "debugfs", "tracefs", "pstore", "bpf", "configfs", "fusectl", "mqueue", "hugetlbfs", "binfmt_misc", "autofs", "efivarfs", "nsfs", "rpc_pipefs", "selinuxfs"];

pub fn df(args: &[String]) -> i32 {
    let p = match parse(args, "ahkTPm", "t") {
        Ok(p) => p,
        Err(e) => {
            error("df", format!("{e}\nusage: df [-ahkmPT] [-t TYPE] [PATH...]"));
            return 1;
        }
    };
    let mounts = match flint_core::sys::read_mounts() {
        Ok(m) => m,
        Err(e) => {
            error("df", format!("/proc/mounts: {e}"));
            return 1;
        }
    };
    // Paths given: the mount holding each (the longest matching mount point).
    let selected: Vec<_> = if p.operands.is_empty() {
        mounts.iter().filter(|m| p.has('a') || !PSEUDO_FS.contains(&m.fstype.as_str())).filter(|m| p.value('t').map_or(true, |t| m.fstype == t)).cloned().collect()
    } else {
        let mut v = Vec::new();
        for path in &p.operands {
            let abs = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
            let best = mounts.iter().filter(|m| abs.starts_with(&m.target)).max_by_key(|m| m.target.len());
            match best {
                Some(m) => v.push(m.clone()),
                None => error("df", format!("{path}: no file system")),
            }
        }
        v
    };
    let unit_name = if p.has('h') { "Size" } else if p.has('m') { "1M-blocks" } else { "1K-blocks" };
    let mut rows = vec![{
        let mut r = vec!["Filesystem".to_string()];
        if p.has('T') {
            r.push("Type".into());
        }
        r.extend([unit_name.into(), "Used".into(), if p.has('h') { "Avail" } else { "Available" }.into(), "Use%".into(), "Mounted on".into()]);
        r
    }];
    let mut seen = HashSet::new();
    for m in selected {
        if p.operands.is_empty() && !seen.insert(m.target.clone()) {
            continue;
        }
        let Ok(s) = statvfs(&m.target) else { continue };
        let bs = s.f_frsize as u64;
        let total = s.f_blocks as u64 * bs;
        if total == 0 && !p.has('a') && p.operands.is_empty() {
            continue;
        }
        let free = s.f_bfree as u64 * bs;
        let avail = s.f_bavail as u64 * bs;
        let used = total - free;
        let pct = if used + avail == 0 { "-".to_string() } else { format!("{}%", (used * 100).div_ceil(used + avail)) };
        let f = |v: u64| if p.has('h') { human(v) } else if p.has('m') { (v >> 20).to_string() } else { (v >> 10).to_string() };
        let mut r = vec![m.device.clone()];
        if p.has('T') {
            r.push(m.fstype.clone());
        }
        r.extend([f(total), f(used), f(avail), pct, m.target.clone()]);
        rows.push(r);
    }
    let cols = rows[0].len();
    let widths: Vec<usize> = (0..cols).map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for r in &rows {
        for (c, cell) in r.iter().enumerate() {
            if c == 0 || c == cols - 1 {
                out.push_str(&format!("{cell:<w$}", w = if c == cols - 1 { 0 } else { widths[c] }));
            } else {
                out.push_str(&format!("{cell:>w$}", w = widths[c]));
            }
            if c + 1 < cols {
                out.push(' ');
            }
        }
        out.push('\n');
    }
    let mut o = io::stdout().lock();
    finish_output("df", o.write_all(out.as_bytes()).and_then(|_| o.flush()))
}

struct DuOpts {
    all: bool,
    summarize: bool,
    human: bool,
    unit: u64,
    max_depth: Option<usize>,
    one_fs: bool,
    apparent: bool,
}

fn du_walk(path: &Path, depth: usize, o: &DuOpts, dev: Option<u64>, seen: &mut HashSet<(u64, u64)>, out: &mut String, status: &mut i32) -> u64 {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            error("du", format!("{}: {e}", path.display()));
            *status = 1;
            return 0;
        }
    };
    if m.nlink() > 1 && !m.is_dir() && !seen.insert((m.dev(), m.ino())) {
        return 0;
    }
    let mut size = if o.apparent { m.len() } else { m.blocks() * 512 };
    let show = |size: u64, out: &mut String| {
        let s = if o.human { human(size) } else { size.div_ceil(o.unit).to_string() };
        out.push_str(&format!("{s}\t{}\n", path.display()));
    };
    if m.is_dir() && dev.map_or(true, |d| !o.one_fs || d == m.dev()) {
        match fs::read_dir(path) {
            Ok(rd) => {
                let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
                entries.sort();
                for e in entries {
                    size += du_walk(&e, depth + 1, o, Some(dev.unwrap_or(m.dev())), seen, out, status);
                }
            }
            Err(e) => {
                error("du", format!("cannot read directory {}: {e}", path.display()));
                *status = 1;
            }
        }
        if !o.summarize && o.max_depth.map_or(true, |d| depth <= d) || (o.summarize && depth == 0) {
            show(size, out);
        }
    } else if (o.all && o.max_depth.map_or(true, |d| depth <= d) && !o.summarize) || depth == 0 {
        show(size, out);
    }
    size
}

pub fn du(args: &[String]) -> i32 {
    let p = match parse(args, "ashkmcxb", "d") {
        Ok(p) => p,
        Err(e) => {
            error("du", format!("{e}\nusage: du [-ahkmscxb] [-d DEPTH] [PATH...]"));
            return 1;
        }
    };
    let o = DuOpts {
        all: p.has('a'),
        summarize: p.has('s'),
        human: p.has('h'),
        unit: if p.has('b') { 1 } else if p.has('m') { 1 << 20 } else { 1024 },
        max_depth: p.value('d').and_then(|d| d.parse().ok()),
        one_fs: p.has('x'),
        apparent: p.has('b'),
    };
    let mut paths = p.operands.clone();
    if paths.is_empty() {
        paths.push(".".into());
    }
    let mut out = String::new();
    let mut status = 0;
    let mut total = 0;
    let mut seen = HashSet::new();
    for path in &paths {
        total += du_walk(Path::new(path), 0, &o, None, &mut seen, &mut out, &mut status);
    }
    if p.has('c') {
        let s = if o.human { human(total) } else { total.div_ceil(o.unit).to_string() };
        out.push_str(&format!("{s}\ttotal\n"));
    }
    let mut so = io::stdout().lock();
    let r = finish_output("du", so.write_all(out.as_bytes()).and_then(|_| so.flush()));
    if r != 0 {
        r
    } else {
        status
    }
}

// ---------------------------------------------------------------------------------------
// stat

fn file_type_name(m: &fs::Metadata) -> &'static str {
    let ft = m.file_type();
    if ft.is_symlink() {
        "symbolic link"
    } else if ft.is_dir() {
        "directory"
    } else if ft.is_file() {
        if m.len() == 0 {
            "regular empty file"
        } else {
            "regular file"
        }
    } else if ft.is_char_device() {
        "character special file"
    } else if ft.is_block_device() {
        "block special file"
    } else if ft.is_fifo() {
        "fifo"
    } else if ft.is_socket() {
        "socket"
    } else {
        "unknown"
    }
}

fn fmt_time(sec: i64, nsec: i64) -> String {
    let tm = broken_down(sec, nsec as u32, false);
    strftime("%Y-%m-%d %H:%M:%S.%N %z", &tm)
}

fn stat_format(fmt: &str, path: &str, m: &fs::Metadata) -> String {
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
            continue;
        }
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(s) = it.next() else { break };
        let v = match s {
            'n' => path.to_string(),
            'N' => match fs::read_link(path) {
                Ok(t) if m.file_type().is_symlink() => format!("'{path}' -> '{}'", t.display()),
                _ => format!("'{path}'"),
            },
            's' => m.len().to_string(),
            'b' => m.blocks().to_string(),
            'B' => "512".into(),
            'o' => m.blksize().to_string(),
            'a' => format!("{:o}", m.mode() & 0o7777),
            'A' => mode_string(m.mode()),
            'f' => format!("{:x}", m.mode()),
            'F' => file_type_name(m).into(),
            'u' => m.uid().to_string(),
            'U' => user_name(m.uid()),
            'g' => m.gid().to_string(),
            'G' => group_name(m.gid()),
            'i' => m.ino().to_string(),
            'h' => m.nlink().to_string(),
            'd' => m.dev().to_string(),
            'D' => format!("{:x}", m.dev()),
            't' => format!("{:x}", libc::major(m.rdev())),
            'T' => format!("{:x}", libc::minor(m.rdev())),
            'x' => fmt_time(m.atime(), m.atime_nsec()),
            'y' => fmt_time(m.mtime(), m.mtime_nsec()),
            'z' => fmt_time(m.ctime(), m.ctime_nsec()),
            'X' => m.atime().to_string(),
            'Y' => m.mtime().to_string(),
            'Z' => m.ctime().to_string(),
            '%' => "%".into(),
            o => format!("%{o}"),
        };
        out.push_str(&v);
    }
    out
}

pub fn stat(args: &[String]) -> i32 {
    let p = match parse(args, "Lt", "c") {
        Ok(p) => p,
        Err(e) => {
            error("stat", format!("{e}\nusage: stat [-L] [-c FORMAT] FILE..."));
            return 1;
        }
    };
    if p.operands.is_empty() {
        error("stat", "missing operand");
        return 1;
    }
    let mut status = 0;
    let mut out = String::new();
    for f in &p.operands {
        let m = if p.has('L') { fs::metadata(f) } else { fs::symlink_metadata(f) };
        let m = match m {
            Ok(m) => m,
            Err(e) => {
                error("stat", format!("cannot stat '{f}': {e}"));
                status = 1;
                continue;
            }
        };
        let fmt = match p.value('c') {
            Some(c) => format!("{c}\n"),
            None if p.has('t') => "%n %s %b %f %u %g %D %i %h %t %T %X %Y %Z %o\n".into(),
            None => {
                let dev_line = if m.file_type().is_char_device() || m.file_type().is_block_device() { "Device: %D\tInode: %i\tLinks: %h\tDevice type: %t,%T\n" } else { "Device: %D\tInode: %i\tLinks: %h\n" };
                format!("  File: %N\n  Size: %s\tBlocks: %b\tIO Block: %o\t%F\n{dev_line}Access: (%a/%A)  Uid: (%u/%U)   Gid: (%g/%G)\nAccess: %x\nModify: %y\nChange: %z\n")
            }
        };
        out.push_str(&stat_format(&fmt, f, &m));
    }
    print!("{out}");
    status
}

// ---------------------------------------------------------------------------------------
// which, readlink, basename, dirname

pub fn which(args: &[String]) -> i32 {
    let p = match parse(args, "a", "") {
        Ok(p) => p,
        Err(e) => {
            error("which", format!("{e}\nusage: which [-a] NAME..."));
            return 2;
        }
    };
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into());
    let mut status = 0;
    for name in &p.operands {
        let mut found = false;
        let candidates: Vec<PathBuf> = if name.contains('/') { vec![PathBuf::from(name)] } else { path.split(':').map(|d| Path::new(if d.is_empty() { "." } else { d }).join(name)).collect() };
        for c in candidates {
            if let Ok(m) = fs::metadata(&c) {
                if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                    println!("{}", c.display());
                    found = true;
                    if !p.has('a') {
                        break;
                    }
                }
            }
        }
        if !found {
            status = 1;
        }
    }
    status
}

/// Resolve every symlink and `..` in `p`. With `must_exist` every component must exist;
/// otherwise (readlink -m) missing parts are kept as they are.
pub fn canonicalize(p: &Path, must_exist: bool, last_may_miss: bool) -> io::Result<PathBuf> {
    let start = if p.is_absolute() { PathBuf::from("/") } else { std::env::current_dir()? };
    let mut todo: Vec<std::ffi::OsString> = p.components().rev().filter_map(|c| match c {
        std::path::Component::Normal(s) => Some(s.to_os_string()),
        std::path::Component::ParentDir => Some("..".into()),
        _ => None,
    }).collect();
    let mut cur = start;
    let mut links = 0;
    while let Some(part) = todo.pop() {
        if part == ".." {
            cur.pop();
            continue;
        }
        let next = cur.join(&part);
        match fs::symlink_metadata(&next) {
            Ok(m) if m.file_type().is_symlink() => {
                links += 1;
                if links > 40 {
                    return Err(io::Error::new(io::ErrorKind::Other, "too many levels of symbolic links"));
                }
                let target = fs::read_link(&next)?;
                if target.is_absolute() {
                    cur = PathBuf::from("/");
                }
                for c in target.components().rev() {
                    match c {
                        std::path::Component::Normal(s) => todo.push(s.to_os_string()),
                        std::path::Component::ParentDir => todo.push("..".into()),
                        _ => {}
                    }
                }
            }
            Ok(_) => cur = next,
            Err(e) => {
                let is_last = todo.is_empty();
                if must_exist && !(last_may_miss && is_last) {
                    return Err(e);
                }
                cur = next;
            }
        }
    }
    Ok(cur)
}

pub fn readlink(args: &[String]) -> i32 {
    let p = match parse(args, "femnqsvz", "") {
        Ok(p) => p,
        Err(e) => {
            error("readlink", format!("{e}\nusage: readlink [-f | -e | -m] [-n] FILE..."));
            return 1;
        }
    };
    let mut status = 0;
    let end = if p.has('z') { "\0" } else if p.has('n') { "" } else { "\n" };
    for f in &p.operands {
        let r = if p.has('f') {
            canonicalize(Path::new(f), true, true)
        } else if p.has('e') {
            canonicalize(Path::new(f), true, false)
        } else if p.has('m') {
            canonicalize(Path::new(f), false, true)
        } else {
            fs::read_link(f)
        };
        match r {
            Ok(t) => print!("{}{end}", t.display()),
            Err(e) => {
                if !(p.has('q') || p.has('s')) && (p.has('v') || p.has('f') || p.has('e') || p.has('m')) {
                    error("readlink", format!("{f}: {e}"));
                }
                status = 1;
            }
        }
    }
    status
}

pub fn base_name(s: &str, suffix: Option<&str>) -> String {
    let t = s.trim_end_matches('/');
    if t.is_empty() {
        return if s.is_empty() { String::new() } else { "/".into() };
    }
    let b = t.rsplit('/').next().unwrap_or(t);
    match suffix {
        Some(x) if !x.is_empty() && b != x && b.ends_with(x) => b[..b.len() - x.len()].to_string(),
        _ => b.to_string(),
    }
}

pub fn dir_name(s: &str) -> String {
    let t = s.trim_end_matches('/');
    if t.is_empty() {
        return if s.starts_with('/') { "/".into() } else { ".".into() };
    }
    match t.rfind('/') {
        None => ".".into(),
        Some(k) => {
            let d = t[..k].trim_end_matches('/');
            if d.is_empty() {
                "/".into()
            } else {
                d.to_string()
            }
        }
    }
}

pub fn basename(args: &[String]) -> i32 {
    let p = match parse(args, "az", "s") {
        Ok(p) => p,
        Err(e) => {
            error("basename", format!("{e}\nusage: basename NAME [SUFFIX] | basename -a [-s SUFFIX] NAME..."));
            return 1;
        }
    };
    let end = if p.has('z') { "\0" } else { "\n" };
    if p.has('a') || p.value('s').is_some() {
        for n in &p.operands {
            print!("{}{end}", base_name(n, p.value('s')));
        }
        return 0;
    }
    match p.operands.as_slice() {
        [n] => print!("{}{end}", base_name(n, None)),
        [n, s] => print!("{}{end}", base_name(n, Some(s))),
        _ => {
            error("basename", "usage: basename NAME [SUFFIX]");
            return 1;
        }
    }
    0
}

pub fn dirname(args: &[String]) -> i32 {
    let p = match parse(args, "z", "") {
        Ok(p) => p,
        Err(e) => {
            error("dirname", format!("{e}\nusage: dirname NAME..."));
            return 1;
        }
    };
    if p.operands.is_empty() {
        error("dirname", "missing operand");
        return 1;
    }
    let end = if p.has('z') { "\0" } else { "\n" };
    for n in &p.operands {
        print!("{}{end}", dir_name(n));
    }
    0
}

// ---------------------------------------------------------------------------------------
// mknod, mkfifo

fn mode_arg(m: Option<&str>, default: u32) -> Result<u32, String> {
    match m {
        None => Ok(default),
        Some(s) => super::files::apply_mode(s, default, false),
    }
}

pub fn mknod(args: &[String]) -> i32 {
    let p = match parse(args, "", "m") {
        Ok(p) => p,
        Err(e) => {
            error("mknod", format!("{e}\nusage: mknod [-m MODE] NAME TYPE [MAJOR MINOR]   (TYPE: b, c, u or p)"));
            return 1;
        }
    };
    let mode = match mode_arg(p.value('m'), 0o666) {
        Ok(m) => m,
        Err(e) => {
            error("mknod", e);
            return 1;
        }
    };
    let ops = &p.operands;
    let (name, kind) = match (ops.first(), ops.get(1)) {
        (Some(n), Some(t)) => (n, t.as_str()),
        _ => {
            error("mknod", "usage: mknod [-m MODE] NAME TYPE [MAJOR MINOR]");
            return 1;
        }
    };
    let (fmt, dev) = match kind {
        "p" => {
            if ops.len() != 2 {
                error("mknod", "fifos do not have major and minor numbers");
                return 1;
            }
            (libc::S_IFIFO, 0)
        }
        "b" | "c" | "u" => {
            let (Some(Ok(ma)), Some(Ok(mi))) = (ops.get(2).map(|x| x.parse::<u32>()), ops.get(3).map(|x| x.parse::<u32>())) else {
                error("mknod", "block and character devices need MAJOR and MINOR numbers");
                return 1;
            };
            (if kind == "b" { libc::S_IFBLK } else { libc::S_IFCHR }, libc::makedev(ma, mi))
        }
        _ => {
            error("mknod", format!("invalid device type '{kind}'"));
            return 1;
        }
    };
    let Ok(c) = CString::new(name.as_bytes()) else { return 1 };
    // SAFETY: mknod with a valid path; umask is cleared so -m is exact.
    let r = unsafe {
        let old = libc::umask(0);
        let r = libc::mknod(c.as_ptr(), fmt | mode, dev);
        libc::umask(old);
        if p.value('m').is_none() {
            libc::chmod(c.as_ptr(), mode & !old);
        }
        r
    };
    if r != 0 {
        error("mknod", format!("{name}: {}", io::Error::last_os_error()));
        return 1;
    }
    0
}

pub fn mkfifo(args: &[String]) -> i32 {
    let p = match parse(args, "", "m") {
        Ok(p) => p,
        Err(e) => {
            error("mkfifo", format!("{e}\nusage: mkfifo [-m MODE] NAME..."));
            return 1;
        }
    };
    let mode = match mode_arg(p.value('m'), 0o666) {
        Ok(m) => m,
        Err(e) => {
            error("mkfifo", e);
            return 1;
        }
    };
    let mut status = 0;
    for n in &p.operands {
        let Ok(c) = CString::new(n.as_bytes()) else { continue };
        // SAFETY: mkfifo with a valid path.
        let r = unsafe {
            if p.value('m').is_some() {
                let old = libc::umask(0);
                let r = libc::mkfifo(c.as_ptr(), mode);
                libc::umask(old);
                r
            } else {
                libc::mkfifo(c.as_ptr(), mode)
            }
        };
        if r != 0 {
            error("mkfifo", format!("{n}: {}", io::Error::last_os_error()));
            status = 1;
        }
    }
    status
}

// ---------------------------------------------------------------------------------------
// find

#[derive(Debug, Clone)]
enum Cmp {
    Less(i64),
    More(i64),
    Exactly(i64),
}

impl Cmp {
    fn parse(s: &str) -> Option<Cmp> {
        if let Some(r) = s.strip_prefix('+') {
            r.parse().ok().map(Cmp::More)
        } else if let Some(r) = s.strip_prefix('-') {
            r.parse().ok().map(Cmp::Less)
        } else {
            s.parse().ok().map(Cmp::Exactly)
        }
    }

    fn test(&self, v: i64) -> bool {
        match self {
            Cmp::Less(n) => v < *n,
            Cmp::More(n) => v > *n,
            Cmp::Exactly(n) => v == *n,
        }
    }
}

#[derive(Debug, Clone)]
enum FExpr {
    True,
    False,
    Name(String, bool),
    Path(String, bool),
    Regex(Box<crate::regex::Regex>),
    Type(char),
    Size(Cmp, u64),
    Time(char, Cmp, i64), // a/m/c, comparison, unit seconds (86400 or 60)
    Newer(i64),
    Perm(u32, char), // exact, '-' all bits, '/' any bit
    User(u32),
    Group(u32),
    NoUser,
    Empty,
    Links(Cmp),
    Inum(Cmp),
    Access(i32),
    Print(bool),
    Printf(String),
    Ls,
    Delete,
    Exec(Vec<String>, bool, bool), // argv, plus mode, in dir
    Prune,
    Quit,
    Not(Box<FExpr>),
    And(Box<FExpr>, Box<FExpr>),
    Or(Box<FExpr>, Box<FExpr>),
}

struct FindParser<'a> {
    a: &'a [String],
    i: usize,
    has_action: bool,
    max_depth: Option<usize>,
    min_depth: usize,
    depth_first: bool,
    xdev: bool,
    follow: bool,
}

impl FindParser<'_> {
    fn arg(&mut self, what: &str) -> Result<String, String> {
        let v = self.a.get(self.i).cloned().ok_or(format!("missing argument to `{what}'"))?;
        self.i += 1;
        Ok(v)
    }

    fn or(&mut self) -> Result<FExpr, String> {
        let mut l = self.and()?;
        while self.a.get(self.i).is_some_and(|x| x == "-o" || x == "-or") {
            self.i += 1;
            let r = self.and()?;
            l = FExpr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<FExpr, String> {
        let mut l = self.not()?;
        loop {
            match self.a.get(self.i).map(String::as_str) {
                None | Some("-o" | "-or" | ")") => return Ok(l),
                Some("-a" | "-and") => self.i += 1,
                _ => {}
            }
            let r = self.not()?;
            l = FExpr::And(Box::new(l), Box::new(r));
        }
    }

    fn not(&mut self) -> Result<FExpr, String> {
        if self.a.get(self.i).is_some_and(|x| x == "!" || x == "-not") {
            self.i += 1;
            return Ok(FExpr::Not(Box::new(self.not()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<FExpr, String> {
        let t = self.arg("expression")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let _ = now;
        Ok(match t.as_str() {
            "(" => {
                let e = self.or()?;
                if self.a.get(self.i).map(String::as_str) != Some(")") {
                    return Err("missing ')'".into());
                }
                self.i += 1;
                e
            }
            "-true" => FExpr::True,
            "-false" => FExpr::False,
            "-name" | "-iname" => FExpr::Name(self.arg(&t)?, t == "-iname"),
            "-path" | "-ipath" | "-wholename" => FExpr::Path(self.arg(&t)?, t == "-ipath"),
            "-regex" => {
                let r = self.arg(&t)?;
                let re = crate::regex::Regex::new(&format!("^({r})$"), crate::regex::Syntax::Extended, false).map_err(|e| e.0)?;
                FExpr::Regex(Box::new(re))
            }
            "-type" => {
                let v = self.arg(&t)?;
                let c = v.chars().next().unwrap_or('?');
                if v.len() != 1 || !"fdlpscb".contains(c) {
                    return Err(format!("unknown argument to -type: {v}"));
                }
                FExpr::Type(c)
            }
            "-size" => {
                let v = self.arg(&t)?;
                let (num, unit) = match v.chars().last() {
                    Some(u @ ('c' | 'k' | 'M' | 'G' | 'b' | 'w')) => (&v[..v.len() - 1], u),
                    _ => (v.as_str(), 'b'),
                };
                let mult = match unit {
                    'c' => 1,
                    'w' => 2,
                    'k' => 1024,
                    'M' => 1 << 20,
                    'G' => 1 << 30,
                    _ => 512,
                };
                FExpr::Size(Cmp::parse(num).ok_or(format!("invalid -size {v}"))?, mult)
            }
            "-mtime" | "-atime" | "-ctime" | "-mmin" | "-amin" | "-cmin" => {
                let v = self.arg(&t)?;
                let which = t.as_bytes()[1] as char;
                let unit = if t.ends_with("min") { 60 } else { 86400 };
                FExpr::Time(which, Cmp::parse(&v).ok_or(format!("invalid argument {v} to {t}"))?, unit)
            }
            "-newer" => {
                let f = self.arg(&t)?;
                let m = fs::metadata(&f).map_err(|e| format!("{f}: {e}"))?;
                FExpr::Newer(m.mtime())
            }
            "-perm" => {
                let v = self.arg(&t)?;
                let (kind, m) = match v.chars().next() {
                    Some('-') => ('-', &v[1..]),
                    Some('/') => ('/', &v[1..]),
                    _ => ('=', v.as_str()),
                };
                let mode = super::files::apply_mode(m, 0, false).map_err(|e| format!("invalid mode {v}: {e}"))?;
                FExpr::Perm(mode, kind)
            }
            "-user" | "-uid" => {
                let v = self.arg(&t)?;
                let uid = v.parse().ok().or_else(|| parse_passwd(&fs::read_to_string("/etc/passwd").unwrap_or_default()).into_iter().find(|u| u.name == v).map(|u| u.uid)).ok_or(format!("no such user {v}"))?;
                FExpr::User(uid)
            }
            "-group" | "-gid" => {
                let v = self.arg(&t)?;
                let gid = v.parse().ok().or_else(|| parse_group(&fs::read_to_string("/etc/group").unwrap_or_default()).into_iter().find(|g| g.name == v).map(|g| g.gid)).ok_or(format!("no such group {v}"))?;
                FExpr::Group(gid)
            }
            "-nouser" => FExpr::NoUser,
            "-empty" => FExpr::Empty,
            "-links" => {
                let v = self.arg(&t)?;
                FExpr::Links(Cmp::parse(&v).ok_or(format!("invalid -links {v}"))?)
            }
            "-inum" => {
                let v = self.arg(&t)?;
                FExpr::Inum(Cmp::parse(&v).ok_or(format!("invalid -inum {v}"))?)
            }
            "-readable" => FExpr::Access(libc::R_OK),
            "-writable" => FExpr::Access(libc::W_OK),
            "-executable" => FExpr::Access(libc::X_OK),
            "-print" | "-print0" => {
                self.has_action = true;
                FExpr::Print(t == "-print0")
            }
            "-printf" => {
                self.has_action = true;
                FExpr::Printf(self.arg(&t)?)
            }
            "-ls" => {
                self.has_action = true;
                FExpr::Ls
            }
            "-delete" => {
                self.has_action = true;
                self.depth_first = true;
                FExpr::Delete
            }
            "-exec" | "-execdir" | "-ok" => {
                self.has_action = true;
                let mut argv = Vec::new();
                let plus;
                loop {
                    let x = self.arg(&t)?;
                    if x == ";" {
                        plus = false;
                        break;
                    }
                    if x == "+" && argv.last().is_some_and(|l: &String| l == "{}") {
                        plus = true;
                        argv.pop();
                        break;
                    }
                    argv.push(x);
                }
                if argv.is_empty() {
                    return Err(format!("{t} needs a command"));
                }
                FExpr::Exec(argv, plus, t == "-execdir")
            }
            "-prune" => FExpr::Prune,
            "-quit" => FExpr::Quit,
            "-maxdepth" => {
                self.max_depth = Some(self.arg(&t)?.parse().map_err(|_| "invalid -maxdepth")?);
                FExpr::True
            }
            "-mindepth" => {
                self.min_depth = self.arg(&t)?.parse().map_err(|_| "invalid -mindepth")?;
                FExpr::True
            }
            "-depth" => {
                self.depth_first = true;
                FExpr::True
            }
            "-xdev" | "-mount" => {
                self.xdev = true;
                FExpr::True
            }
            "-follow" => {
                self.follow = true;
                FExpr::True
            }
            other => return Err(format!("unknown predicate `{other}'")),
        })
    }
}

fn glob_match(p: &[char], s: &[char], icase: bool) -> bool {
    // fnmatch with * ? [...] and backslash escapes; '/' is not special (find -name has no '/').
    let eq = |a: char, b: char| if icase { a.to_lowercase().eq(b.to_lowercase()) } else { a == b };
    let (mut pi, mut si) = (0, 0);
    let (mut star_p, mut star_s) = (usize::MAX, 0);
    while si < s.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star_p = pi;
                    star_s = si;
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                '[' => {
                    if let Some((ok, len)) = class_match(&p[pi..], s[si], icase) {
                        if ok {
                            pi += len;
                            si += 1;
                            continue;
                        }
                    } else if eq('[', s[si]) {
                        pi += 1;
                        si += 1;
                        continue;
                    }
                }
                '\\' if pi + 1 < p.len() => {
                    if eq(p[pi + 1], s[si]) {
                        pi += 2;
                        si += 1;
                        continue;
                    }
                }
                c => {
                    if eq(c, s[si]) {
                        pi += 1;
                        si += 1;
                        continue;
                    }
                }
            }
        }
        if star_p != usize::MAX {
            pi = star_p + 1;
            star_s += 1;
            si = star_s;
            continue;
        }
        return false;
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn class_match(p: &[char], c: char, icase: bool) -> Option<(bool, usize)> {
    let mut i = 1;
    let neg = matches!(p.get(1), Some('!' | '^'));
    if neg {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    let lc = |x: char| if icase { x.to_lowercase().next().unwrap_or(x) } else { x };
    while i < p.len() {
        if p[i] == ']' && !first {
            return Some((hit != neg, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if lc(p[i]) <= lc(c) && lc(c) <= lc(p[i + 2]) {
                hit = true;
            }
            i += 3;
        } else {
            if lc(p[i]) == lc(c) {
                hit = true;
            }
            i += 1;
        }
    }
    None
}

pub fn fnmatch(pattern: &str, s: &str, icase: bool) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = s.chars().collect();
    glob_match(&p, &t, icase)
}

struct FindState {
    now: i64,
    out: io::BufWriter<io::Stdout>,
    batches: Vec<(Vec<String>, Vec<String>)>,
    status: i32,
    quit: bool,
}

fn find_printf(fmt: &str, path: &Path, start: &str, depth: usize, m: &fs::Metadata) -> String {
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            },
            '%' => {
                let Some(s) = it.next() else { break };
                let p = path.to_string_lossy();
                match s {
                    'p' => out.push_str(&p),
                    'f' => out.push_str(&base_name(&p, None)),
                    'h' => out.push_str(&dir_name(&p)),
                    'P' => out.push_str(p.strip_prefix(start).map(|x| x.trim_start_matches('/')).unwrap_or(&p)),
                    's' => out.push_str(&m.len().to_string()),
                    'm' => out.push_str(&format!("{:o}", m.mode() & 0o7777)),
                    'M' => out.push_str(&mode_string(m.mode())),
                    'u' => out.push_str(&user_name(m.uid())),
                    'U' => out.push_str(&m.uid().to_string()),
                    'g' => out.push_str(&group_name(m.gid())),
                    'G' => out.push_str(&m.gid().to_string()),
                    'd' => out.push_str(&depth.to_string()),
                    'i' => out.push_str(&m.ino().to_string()),
                    'n' => out.push_str(&m.nlink().to_string()),
                    'y' => out.push(match file_type_name(m) {
                        "directory" => 'd',
                        "symbolic link" => 'l',
                        "fifo" => 'p',
                        "socket" => 's',
                        "character special file" => 'c',
                        "block special file" => 'b',
                        _ => 'f',
                    }),
                    'l' => {
                        if let Ok(t) = fs::read_link(path) {
                            out.push_str(&t.to_string_lossy());
                        }
                    }
                    't' => out.push_str(&strftime("%a %b %e %H:%M:%S %Y", &broken_down(m.mtime(), 0, false))),
                    'T' => {
                        if let Some(k) = it.next() {
                            let tm = broken_down(m.mtime(), m.mtime_nsec() as u32, false);
                            out.push_str(&if k == '@' { format!("{}", m.mtime()) } else { strftime(&format!("%{k}"), &tm) });
                        }
                    }
                    '%' => out.push('%'),
                    o => {
                        out.push('%');
                        out.push(o);
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn run_exec(argv: &[String], files: &[String], dir: Option<&Path>) -> bool {
    let mut cmd: Vec<String> = Vec::new();
    for a in argv {
        if a.contains("{}") && files.len() == 1 {
            cmd.push(a.replace("{}", &files[0]));
        } else {
            cmd.push(a.clone());
        }
    }
    if files.len() > 1 || !argv.iter().any(|a| a.contains("{}")) {
        cmd.extend(files.iter().cloned());
    }
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    match c.status() {
        Ok(s) => s.success(),
        Err(e) => {
            error("find", format!("{}: {e}", cmd[0]));
            false
        }
    }
}

fn eval_find(e: &FExpr, path: &Path, start: &str, depth: usize, m: &fs::Metadata, st: &mut FindState, prune: &mut bool) -> bool {
    let name = || path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    match e {
        FExpr::True => true,
        FExpr::False => false,
        FExpr::Name(p, ic) => fnmatch(p, &name(), *ic),
        FExpr::Path(p, ic) => fnmatch(p, &path.to_string_lossy(), *ic),
        FExpr::Regex(re) => re.is_match(path.as_os_str().as_bytes()),
        FExpr::Type(t) => {
            let ft = m.file_type();
            match t {
                'f' => ft.is_file(),
                'd' => ft.is_dir(),
                'l' => ft.is_symlink(),
                'p' => ft.is_fifo(),
                's' => ft.is_socket(),
                'c' => ft.is_char_device(),
                _ => ft.is_block_device(),
            }
        }
        FExpr::Size(c, unit) => c.test(m.len().div_ceil(*unit) as i64),
        FExpr::Time(which, c, unit) => {
            let t = match which {
                'a' => m.atime(),
                'c' => m.ctime(),
                _ => m.mtime(),
            };
            let age = (st.now - t).max(0);
            // -mtime N: N full days ago (rounded down); -mmin likewise in minutes
            c.test(if *unit == 60 { (age + 59) / 60 } else { age / unit })
        }
        FExpr::Newer(t) => m.mtime() > *t,
        FExpr::Perm(mode, kind) => {
            let p = m.mode() & 0o7777;
            match kind {
                '-' => p & mode == *mode,
                '/' => *mode == 0 || p & mode != 0,
                _ => p == *mode,
            }
        }
        FExpr::User(u) => m.uid() == *u,
        FExpr::Group(g) => m.gid() == *g,
        FExpr::NoUser => !parse_passwd(&fs::read_to_string("/etc/passwd").unwrap_or_default()).iter().any(|u| u.uid == m.uid()),
        FExpr::Empty => {
            if m.is_dir() {
                fs::read_dir(path).map(|mut d| d.next().is_none()).unwrap_or(false)
            } else {
                m.is_file() && m.len() == 0
            }
        }
        FExpr::Links(c) => c.test(m.nlink() as i64),
        FExpr::Inum(c) => c.test(m.ino() as i64),
        FExpr::Access(mode) => CString::new(path.as_os_str().as_bytes()).map(|c| unsafe { libc::access(c.as_ptr(), *mode) } == 0).unwrap_or(false),
        FExpr::Print(nul) => {
            let _ = st.out.write_all(path.as_os_str().as_bytes());
            let _ = st.out.write_all(if *nul { b"\0" } else { b"\n" });
            true
        }
        FExpr::Printf(f) => {
            let s = find_printf(f, path, start, depth, m);
            let _ = st.out.write_all(s.as_bytes());
            true
        }
        FExpr::Ls => {
            let tm = broken_down(m.mtime(), 0, false);
            let line = format!("{:>7} {:>6} {} {:>3} {:<8} {:<8} {:>8} {} {}{}\n", m.ino(), m.blocks() / 2, mode_string(m.mode()), m.nlink(), user_name(m.uid()), group_name(m.gid()), m.len(), strftime("%b %e %H:%M", &tm), path.display(), fs::read_link(path).map(|t| format!(" -> {}", t.display())).unwrap_or_default());
            let _ = st.out.write_all(line.as_bytes());
            true
        }
        FExpr::Delete => {
            let r = if m.is_dir() { fs::remove_dir(path) } else { fs::remove_file(path) };
            if let Err(e) = r {
                error("find", format!("cannot delete {}: {e}", path.display()));
                st.status = 1;
                return false;
            }
            true
        }
        FExpr::Exec(argv, plus, in_dir) => {
            let _ = st.out.flush();
            let (file, dir) = if *in_dir {
                (format!("./{}", name()), path.parent().map(Path::to_path_buf))
            } else {
                (path.to_string_lossy().into_owned(), None)
            };
            if *plus {
                match st.batches.iter_mut().find(|(a, _)| a == argv) {
                    Some((_, files)) => files.push(file),
                    None => st.batches.push((argv.clone(), vec![file])),
                }
                if st.batches.iter().any(|(_, f)| f.len() >= 1000) {
                    flush_batches(st);
                }
                true
            } else {
                let ok = run_exec(argv, &[file], dir.as_deref());
                if !ok {
                    st.status = st.status.max(0);
                }
                ok
            }
        }
        FExpr::Prune => {
            *prune = true;
            true
        }
        FExpr::Quit => {
            st.quit = true;
            true
        }
        FExpr::Not(x) => !eval_find(x, path, start, depth, m, st, prune),
        FExpr::And(a, b) => eval_find(a, path, start, depth, m, st, prune) && eval_find(b, path, start, depth, m, st, prune),
        FExpr::Or(a, b) => eval_find(a, path, start, depth, m, st, prune) || eval_find(b, path, start, depth, m, st, prune),
    }
}

fn flush_batches(st: &mut FindState) {
    for (argv, files) in std::mem::take(&mut st.batches) {
        if !run_exec(&argv, &files, None) {
            st.status = 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn find_walk(path: &Path, start: &str, depth: usize, e: &FExpr, fp: &FindParser, root_dev: u64, st: &mut FindState, visited: &mut HashSet<(u64, u64)>) {
    if st.quit {
        return;
    }
    let m = if fp.follow || depth == 0 && fp.follow { fs::metadata(path).or_else(|_| fs::symlink_metadata(path)) } else { fs::symlink_metadata(path) };
    let m = match m {
        Ok(m) => m,
        Err(err) => {
            error("find", format!("{}: {err}", path.display()));
            st.status = 1;
            return;
        }
    };
    let mut prune = false;
    let applies = depth >= fp.min_depth;
    if applies && !fp.depth_first {
        eval_find(e, path, start, depth, &m, st, &mut prune);
    }
    let descend = m.is_dir() && !prune && fp.max_depth.map_or(true, |d| depth < d) && (!fp.xdev || m.dev() == root_dev) && !st.quit;
    if descend {
        if fp.follow && !visited.insert((m.dev(), m.ino())) {
            error("find", format!("{}: file system loop detected", path.display()));
            return;
        }
        match fs::read_dir(path) {
            Ok(rd) => {
                let mut entries: Vec<PathBuf> = rd.flatten().map(|x| x.path()).collect();
                entries.sort();
                for c in entries {
                    find_walk(&c, start, depth + 1, e, fp, root_dev, st, visited);
                    if st.quit {
                        break;
                    }
                }
            }
            Err(err) => {
                error("find", format!("{}: {err}", path.display()));
                st.status = 1;
            }
        }
        if fp.follow {
            visited.remove(&(m.dev(), m.ino()));
        }
    }
    if applies && fp.depth_first && !st.quit {
        eval_find(e, path, start, depth, &m, st, &mut prune);
    }
}

pub fn find(args: &[String]) -> i32 {
    let mut starts = Vec::new();
    let mut i = 0;
    let mut follow = false;
    while i < args.len() {
        let a = &args[i];
        if a == "-L" || a == "-H" {
            follow = a == "-L";
            i += 1;
            continue;
        }
        if a == "-P" {
            i += 1;
            continue;
        }
        if a.starts_with('-') || a == "(" || a == "!" {
            break;
        }
        starts.push(a.clone());
        i += 1;
    }
    if starts.is_empty() {
        starts.push(".".into());
    }
    let rest = &args[i..];
    let mut fp = FindParser { a: rest, i: 0, has_action: false, max_depth: None, min_depth: 0, depth_first: false, xdev: false, follow };
    let expr = if rest.is_empty() {
        FExpr::True
    } else {
        match fp.or() {
            Ok(e) => {
                if fp.i < rest.len() {
                    error("find", format!("unexpected `{}'", rest[fp.i]));
                    return 1;
                }
                e
            }
            Err(e) => {
                error("find", e);
                return 1;
            }
        }
    };
    let expr = if fp.has_action { expr } else { FExpr::And(Box::new(expr), Box::new(FExpr::Print(false))) };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut st = FindState { now, out: io::BufWriter::new(io::stdout()), batches: Vec::new(), status: 0, quit: false };
    for s in &starts {
        let dev = fs::metadata(s).map(|m| m.dev()).unwrap_or(0);
        let mut visited = HashSet::new();
        find_walk(Path::new(s), s, 0, &expr, &fp, dev, &mut st, &mut visited);
        if st.quit {
            break;
        }
    }
    flush_batches(&mut st);
    if let Err(e) = st.out.flush() {
        if e.kind() != io::ErrorKind::BrokenPipe {
            error("find", e);
            return 1;
        }
    }
    st.status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        assert_eq!(days_to_civil(0), (1970, 1, 1));
        assert_eq!(days_to_civil(19723), (2024, 1, 1));
        assert_eq!(civil_to_days(2026, 11, 11), 20768);
        for d in [-1000, 0, 59, 60, 10957, 20403, 50000] {
            let (y, m, dd) = days_to_civil(d);
            assert_eq!(civil_to_days(y, m, dd), d);
        }
        let tm = broken_down(1_762_869_600, 0, true); // 2025-11-11 14:00:00 UTC
        assert_eq!(strftime("%Y-%m-%d %H:%M:%S %a %j %Z %s", &tm), "2025-11-11 14:00:00 Tue 315 UTC 1762869600");
        assert_eq!(strftime("%e|%-d|%_m|%I %p", &broken_down(86400 * 4, 0, true)), " 5|5| 1|12 AM");
        assert_eq!(parse_date("2025-11-11 14:00", 0, true), Some(1_762_869_600));
        assert_eq!(parse_date("@123", 0, true), Some(123));
        assert_eq!(parse_date("2025-13-01", 0, true), None);
        assert_eq!(posix_fixed("CET-1"), Some((3600, "CET".into())));
        assert_eq!(posix_fixed("UTC"), Some((0, "UTC".into())));
        assert_eq!(posix_fixed("EST+5"), Some((-18000, "EST".into())));
    }

    #[test]
    fn names() {
        assert_eq!(base_name("/usr/lib/", None), "lib");
        assert_eq!(base_name("/", None), "/");
        assert_eq!(base_name("a.tar.gz", Some(".gz")), "a.tar");
        assert_eq!(base_name(".gz", Some(".gz")), ".gz");
        assert_eq!(dir_name("/usr/lib"), "/usr");
        assert_eq!(dir_name("/usr/"), "/");
        assert_eq!(dir_name("file"), ".");
        assert_eq!(dir_name("a//b"), "a");
        assert_eq!(dir_name("//"), "/");
        assert_eq!(human(1023), "1023");
        assert_eq!(human(1536), "1.5K");
        assert_eq!(human(10 * 1024 * 1024), "10M");
        assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
        assert_eq!(mode_string(0o41777), "drwxrwxrwt");
    }

    #[test]
    fn globbing() {
        assert!(fnmatch("*.rs", "main.rs", false));
        assert!(!fnmatch("*.rs", "main.rsx", false));
        assert!(fnmatch("a?c", "abc", false));
        assert!(fnmatch("[a-c]*", "beta", false));
        assert!(!fnmatch("[!a-c]*", "beta", false));
        assert!(fnmatch("*.TXT", "x.txt", true));
        assert!(fnmatch("\\*", "*", false));
        assert!(fnmatch("*", "", false));
        assert!(fnmatch("a*b*c", "axxbyyc", false));
    }
}
