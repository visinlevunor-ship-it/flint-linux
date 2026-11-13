//! Time: time zones (TZif files and POSIX TZ rules), localtime/gmtime/mktime/timegm,
//! strftime/strptime, ctime/asctime, and the variables tzset sets (timezone, daylight,
//! tzname, glibc's __ names for them).

use super::locale::{day_name, month_name};
use crate::fl;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;
type c_long = i64;
type time_t = i64;
type size_t = usize;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Tm {
    pub tm_sec: c_int,
    pub tm_min: c_int,
    pub tm_hour: c_int,
    pub tm_mday: c_int,
    pub tm_mon: c_int,
    pub tm_year: c_int,
    pub tm_wday: c_int,
    pub tm_yday: c_int,
    pub tm_isdst: c_int,
    pub tm_gmtoff: c_long,
    pub tm_zone: *const c_char,
}

// --- the variables ------------------------------------------------------------------------

#[no_mangle]
pub static mut __timezone: c_long = 0;
#[no_mangle]
pub static mut __daylight: c_int = 0;
#[no_mangle]
pub static mut __tzname: [*const c_char; 2] = [b"UTC\0".as_ptr(), b"UTC\0".as_ptr()];

// the program's copies, if it made them (bound after loading)
static mut TIMEZONE: *mut c_long = ptr::addr_of_mut!(__timezone);
static mut DAYLIGHT: *mut c_int = ptr::addr_of_mut!(__daylight);
static mut TZNAME: *mut [*const c_char; 2] = ptr::addr_of_mut!(__tzname);

pub unsafe fn bind(scope: &[*mut crate::loader::Dso]) {
    use super::start::program_copy;
    if let Some(a) = program_copy(scope, &[b"timezone", b"__timezone"]) {
        TIMEZONE = a as *mut c_long;
    }
    if let Some(a) = program_copy(scope, &[b"daylight", b"__daylight"]) {
        DAYLIGHT = a as *mut c_int;
    }
    if let Some(a) = program_copy(scope, &[b"tzname", b"__tzname"]) {
        TZNAME = a as *mut [*const c_char; 2];
    }
}

// --- calendar arithmetic ------------------------------------------------------------------

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    // m 1..12; days since 1970-01-01 (Howard Hinnant's algorithm)
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Fill `tm` for UTC time `t` shifted by `off` seconds.
fn break_down(t: i64, off: i64, tm: &mut Tm) -> bool {
    let t = match t.checked_add(off) {
        Some(x) => x,
        None => return false,
    };
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    if y - 1900 > i32::MAX as i64 || y - 1900 < i32::MIN as i64 {
        return false;
    }
    tm.tm_year = (y - 1900) as c_int;
    tm.tm_mon = (m - 1) as c_int;
    tm.tm_mday = d as c_int;
    tm.tm_hour = (secs / 3600) as c_int;
    tm.tm_min = (secs / 60 % 60) as c_int;
    tm.tm_sec = (secs % 60) as c_int;
    tm.tm_wday = (days + 4).rem_euclid(7) as c_int;
    tm.tm_yday = (days - days_from_civil(y, 1, 1)) as c_int;
    true
}

/// Seconds since the epoch for a (possibly denormalised) broken-down UTC time.
fn tm_to_secs(tm: &Tm) -> i64 {
    let mut y = tm.tm_year as i64 + 1900;
    let mut m = tm.tm_mon as i64;
    y += m.div_euclid(12);
    m = m.rem_euclid(12);
    let days = days_from_civil(y, m + 1, 1) + tm.tm_mday as i64 - 1;
    days * 86400 + tm.tm_hour as i64 * 3600 + tm.tm_min as i64 * 60 + tm.tm_sec as i64
}

// --- time zones ----------------------------------------------------------------------------

#[derive(Clone)]
struct TType {
    off: i64,
    dst: bool,
    abbr: [u8; 8],
}

/// A POSIX TZ rule: "CET-1CEST,M3.5.0,M10.5.0/3"
#[derive(Clone)]
struct Rule {
    std: TType,
    dst: Option<(TType, RuleDate, i64, RuleDate, i64)>, // start date/time, end date/time (local seconds)
}

#[derive(Clone, Copy)]
enum RuleDate {
    Julian1(i64),        // Jn: 1..365, Feb 29 never counted
    Julian0(i64),        // n: 0..365
    Mwd(i64, i64, i64),  // month, week (1..5), weekday
}

struct Zone {
    times: Vec<i64>,
    idx: Vec<u8>,
    types: Vec<TType>,
    rule: Option<Rule>,
}

static mut ZONE: Option<Zone> = None;
static mut TZ_SEEN: Vec<u8> = Vec::new();
static TZ_LOCK: fl::lock::Lock = fl::lock::Lock::new();

fn abbr(s: &[u8]) -> [u8; 8] {
    let mut a = [0u8; 8];
    let n = s.len().min(7);
    a[..n].copy_from_slice(&s[..n]);
    a
}

fn parse_rule(s: &[u8]) -> Option<Rule> {
    let mut i = 0;
    let name = |i: &mut usize| -> Option<Vec<u8>> {
        if *i < s.len() && s[*i] == b'<' {
            let st = *i + 1;
            while *i < s.len() && s[*i] != b'>' {
                *i += 1;
            }
            let n = s[st..*i].to_vec();
            *i += 1;
            Some(n)
        } else {
            let st = *i;
            while *i < s.len() && s[*i].is_ascii_alphabetic() {
                *i += 1;
            }
            if *i - st < 3 { None } else { Some(s[st..*i].to_vec()) }
        }
    };
    let offset = |i: &mut usize| -> Option<i64> {
        let mut neg = false;
        if *i < s.len() && (s[*i] == b'+' || s[*i] == b'-') {
            neg = s[*i] == b'-';
            *i += 1;
        }
        let mut parts = [0i64; 3];
        for (k, p) in parts.iter_mut().enumerate() {
            if k > 0 {
                if *i < s.len() && s[*i] == b':' {
                    *i += 1;
                } else {
                    break;
                }
            }
            let st = *i;
            while *i < s.len() && s[*i].is_ascii_digit() {
                *p = *p * 10 + (s[*i] - b'0') as i64;
                *i += 1;
            }
            if *i == st {
                return None;
            }
        }
        let v = parts[0] * 3600 + parts[1] * 60 + parts[2];
        Some(if neg { -v } else { v })
    };
    let date = |i: &mut usize| -> Option<RuleDate> {
        let num = |i: &mut usize| -> i64 {
            let mut v = 0;
            while *i < s.len() && s[*i].is_ascii_digit() {
                v = v * 10 + (s[*i] - b'0') as i64;
                *i += 1;
            }
            v
        };
        if *i < s.len() && s[*i] == b'M' {
            *i += 1;
            let m = num(i);
            *i += 1;
            let w = num(i);
            *i += 1;
            let d = num(i);
            Some(RuleDate::Mwd(m, w, d))
        } else if *i < s.len() && s[*i] == b'J' {
            *i += 1;
            Some(RuleDate::Julian1(num(i)))
        } else {
            Some(RuleDate::Julian0(num(i)))
        }
    };
    let sname = name(&mut i)?;
    // POSIX offsets are west-positive
    let soff = -offset(&mut i)?;
    let std = TType { off: soff, dst: false, abbr: abbr(&sname) };
    if i >= s.len() {
        return Some(Rule { std, dst: None });
    }
    let dname = name(&mut i)?;
    let doff = if i < s.len() && s[i] != b',' { -offset(&mut i)? } else { soff + 3600 };
    let dst_t = TType { off: doff, dst: true, abbr: abbr(&dname) };
    let (mut sd, mut st_, mut ed, mut et) = (RuleDate::Mwd(3, 2, 0), 7200, RuleDate::Mwd(11, 1, 0), 7200);
    if i < s.len() && s[i] == b',' {
        i += 1;
        sd = date(&mut i)?;
        if i < s.len() && s[i] == b'/' {
            i += 1;
            st_ = offset(&mut i)?;
        }
        if i < s.len() && s[i] == b',' {
            i += 1;
        }
        ed = date(&mut i)?;
        if i < s.len() && s[i] == b'/' {
            i += 1;
            et = offset(&mut i)?;
        }
    }
    Some(Rule { std, dst: Some((dst_t, sd, st_, ed, et)) })
}

/// The UTC second a rule date (plus local time) falls on in `year`, given the offset in force.
fn rule_time(year: i64, d: RuleDate, secs: i64, off: i64) -> i64 {
    let jan1 = days_from_civil(year, 1, 1);
    let day = match d {
        RuleDate::Julian1(n) => {
            let mut n = n - 1;
            if is_leap(year) && n >= 59 {
                n += 1;
            }
            jan1 + n
        }
        RuleDate::Julian0(n) => jan1 + n,
        RuleDate::Mwd(m, w, wd) => {
            let first = days_from_civil(year, m, 1);
            let first_wd = (first + 4).rem_euclid(7);
            let mut d = first + (wd - first_wd).rem_euclid(7) + (w - 1) * 7;
            let next = if m == 12 { days_from_civil(year + 1, 1, 1) } else { days_from_civil(year, m + 1, 1) };
            while d >= next {
                d -= 7;
            }
            d
        }
    };
    day * 86400 + secs - off
}

fn rule_type(r: &Rule, t: i64) -> TType {
    match &r.dst {
        None => r.std.clone(),
        Some((dt, sd, st, ed, et)) => {
            let (y, _, _) = civil_from_days((t + r.std.off).div_euclid(86400));
            let start = rule_time(y, *sd, *st, r.std.off);
            let end = rule_time(y, *ed, *et, dt.off);
            let in_dst = if start < end { t >= start && t < end } else { t >= start || t < end };
            if in_dst { dt.clone() } else { r.std.clone() }
        }
    }
}

fn parse_tzif(b: &[u8]) -> Option<Zone> {
    if b.len() < 44 || &b[..4] != b"TZif" {
        return None;
    }
    let be32 = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize;
    let version = b[4];
    let counts = |o: usize| (be32(o + 20), be32(o + 24), be32(o + 28), be32(o + 32), be32(o + 36), be32(o + 40));
    let (isutc, isstd, leap, timecnt, typecnt, charcnt) = counts(0);
    let mut o = 44;
    let mut tsize = 4;
    if version >= b'2' {
        // skip the v1 block, use the 64-bit one
        o += timecnt * 4 + timecnt + typecnt * 6 + charcnt + leap * 8 + isstd + isutc;
        if b.len() < o + 44 {
            return None;
        }
        let c = counts(o);
        o += 44;
        tsize = 8;
        return parse_block(b, o, tsize, c, true);
    }
    parse_block(b, o, tsize, (isutc, isstd, leap, timecnt, typecnt, charcnt), false)
}

fn parse_block(b: &[u8], mut o: usize, tsize: usize, c: (usize, usize, usize, usize, usize, usize), footer: bool) -> Option<Zone> {
    let (isutc, isstd, leap, timecnt, typecnt, charcnt) = c;
    let need = o + timecnt * tsize + timecnt + typecnt * 6 + charcnt + leap * (tsize + 4) + isstd + isutc;
    if b.len() < need {
        return None;
    }
    let mut times = Vec::with_capacity(timecnt);
    for k in 0..timecnt {
        let p = o + k * tsize;
        let t = if tsize == 8 {
            i64::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3], b[p + 4], b[p + 5], b[p + 6], b[p + 7]])
        } else {
            i32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]) as i64
        };
        times.push(t);
    }
    o += timecnt * tsize;
    let idx = b[o..o + timecnt].to_vec();
    o += timecnt;
    let tbase = o;
    o += typecnt * 6;
    let chars = &b[o..o + charcnt];
    let mut types = Vec::new();
    for k in 0..typecnt {
        let p = tbase + k * 6;
        let off = i32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]) as i64;
        let dst = b[p + 4] != 0;
        let ai = b[p + 5] as usize;
        let a = if ai < chars.len() {
            let e = chars[ai..].iter().position(|&c| c == 0).map_or(chars.len(), |x| ai + x);
            &chars[ai..e]
        } else {
            b"UTC"
        };
        types.push(TType { off, dst, abbr: abbr(a) });
    }
    o = need;
    let mut rule = None;
    if footer && o < b.len() && b[o] == b'\n' {
        let st = o + 1;
        if let Some(e) = b[st..].iter().position(|&c| c == b'\n') {
            rule = parse_rule(&b[st..st + e]);
        }
    }
    if types.is_empty() {
        return None;
    }
    Some(Zone { times, idx, types, rule })
}

unsafe fn read_file(path: &[u8]) -> Option<Vec<u8>> {
    let mut p = path.to_vec();
    p.push(0);
    let fd = fl::file::open(p.as_ptr() as *const _, 0o2000000, 0);
    if fd < 0 {
        return None;
    }
    let mut v = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let r = fl::file::read(fd, buf.as_mut_ptr() as *mut c_void, buf.len());
        if r <= 0 {
            break;
        }
        v.extend_from_slice(&buf[..r as usize]);
        if v.len() > 1 << 20 {
            break;
        }
    }
    fl::sys::sc1(fl::sys::CLOSE, fd as usize);
    Some(v)
}

unsafe fn load_zone() -> Zone {
    let tz = fl::env::getenv(b"TZ\0".as_ptr());
    let utc = || Zone { times: Vec::new(), idx: Vec::new(), types: alloc::vec![TType { off: 0, dst: false, abbr: abbr(b"UTC") }], rule: None };
    let file: Option<Vec<u8>> = if tz.is_null() {
        read_file(b"/etc/localtime")
    } else {
        let t = fl::mem::cstr(tz);
        let t = if t.first() == Some(&b':') { &t[1..] } else { t };
        if t.is_empty() {
            None
        } else if t.first() == Some(&b'/') {
            read_file(t)
        } else {
            let mut p: Vec<u8> = b"/usr/share/zoneinfo/".to_vec();
            p.extend_from_slice(t);
            match read_file(&p) {
                Some(f) => Some(f),
                None => {
                    if let Some(r) = parse_rule(t) {
                        return Zone { times: Vec::new(), idx: Vec::new(), types: alloc::vec![r.std.clone()], rule: Some(r) };
                    }
                    return utc();
                }
            }
        }
    };
    file.and_then(|f| parse_tzif(&f)).unwrap_or_else(utc)
}

/// Make sure the zone matches $TZ (it is re-read when TZ changes, as glibc does).
unsafe fn zone() -> &'static Zone {
    let tz = fl::env::getenv(b"TZ\0".as_ptr());
    let cur: Vec<u8> = if tz.is_null() { b"\x01".to_vec() } else { fl::mem::cstr(tz).to_vec() };
    let _g = TZ_LOCK.guard();
    let seen = &mut *ptr::addr_of_mut!(TZ_SEEN);
    if (*ptr::addr_of!(ZONE)).is_none() || *seen != cur {
        let z = load_zone();
        *seen = cur;
        set_vars(&z);
        ZONE = Some(z);
    }
    (*ptr::addr_of!(ZONE)).as_ref().unwrap_unchecked()
}

static mut NAMES: [[u8; 8]; 2] = [[0; 8]; 2];

unsafe fn set_vars(z: &Zone) {
    // the standard and daylight types now in force (or last defined)
    let (mut std, mut dst): (Option<TType>, Option<TType>) = (None, None);
    if let Some(r) = &z.rule {
        std = Some(r.std.clone());
        dst = r.dst.as_ref().map(|d| d.0.clone());
    } else {
        for &i in z.idx.iter().rev() {
            let t = &z.types[i as usize];
            if t.dst && dst.is_none() {
                dst = Some(t.clone());
            }
            if !t.dst && std.is_none() {
                std = Some(t.clone());
            }
            if std.is_some() && dst.is_some() {
                break;
            }
        }
        if std.is_none() {
            std = Some(z.types[0].clone());
        }
    }
    let s = std.unwrap();
    let names = &mut *ptr::addr_of_mut!(NAMES);
    names[0] = s.abbr;
    names[1] = dst.as_ref().map_or(s.abbr, |d| d.abbr);
    *TIMEZONE = -s.off;
    *DAYLIGHT = dst.is_some() as c_int;
    *TZNAME = [names[0].as_ptr(), names[1].as_ptr()];
}

/// The type in force at UTC time t.
unsafe fn type_at(t: i64) -> TType {
    let z = zone();
    if z.times.is_empty() {
        return match &z.rule {
            Some(r) => rule_type(r, t),
            None => z.types[0].clone(),
        };
    }
    if t < z.times[0] {
        // before the first transition: the first standard type
        return z.types.iter().find(|x| !x.dst).cloned().unwrap_or_else(|| z.types[0].clone());
    }
    let i = match z.times.binary_search(&t) {
        Ok(i) => i,
        Err(i) => i - 1,
    };
    if i == z.times.len() - 1 {
        if let Some(r) = &z.rule {
            return rule_type(r, t);
        }
    }
    z.types[z.idx[i] as usize].clone()
}

#[no_mangle]
pub unsafe extern "C" fn tzset() {
    zone();
}

static mut TM_BUF: Tm = Tm {
    tm_sec: 0,
    tm_min: 0,
    tm_hour: 0,
    tm_mday: 0,
    tm_mon: 0,
    tm_year: 0,
    tm_wday: 0,
    tm_yday: 0,
    tm_isdst: 0,
    tm_gmtoff: 0,
    tm_zone: ptr::null(),
};
static mut ZONE_ABBR: [[u8; 8]; 16] = [[0; 8]; 16];
static mut ZONE_NEXT: usize = 0;

/// A stable pointer for a zone abbreviation (tm_zone must stay valid).
unsafe fn intern(a: [u8; 8]) -> *const c_char {
    let t = &mut *ptr::addr_of_mut!(ZONE_ABBR);
    for x in t.iter() {
        if *x == a {
            return x.as_ptr();
        }
    }
    let i = ZONE_NEXT % 16;
    ZONE_NEXT += 1;
    t[i] = a;
    t[i].as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn gmtime_r(t: *const time_t, tm: *mut Tm) -> *mut Tm {
    if !break_down(*t, 0, &mut *tm) {
        fl::errno::set(75);
        return ptr::null_mut();
    }
    (*tm).tm_isdst = 0;
    (*tm).tm_gmtoff = 0;
    (*tm).tm_zone = b"GMT\0".as_ptr();
    tm
}
#[no_mangle]
pub unsafe extern "C" fn gmtime(t: *const time_t) -> *mut Tm {
    gmtime_r(t, ptr::addr_of_mut!(TM_BUF))
}
#[no_mangle]
pub unsafe extern "C" fn localtime_r(t: *const time_t, tm: *mut Tm) -> *mut Tm {
    let ty = type_at(*t);
    if !break_down(*t, ty.off, &mut *tm) {
        fl::errno::set(75);
        return ptr::null_mut();
    }
    (*tm).tm_isdst = ty.dst as c_int;
    (*tm).tm_gmtoff = ty.off;
    (*tm).tm_zone = intern(ty.abbr);
    tm
}
#[no_mangle]
pub unsafe extern "C" fn localtime(t: *const time_t) -> *mut Tm {
    localtime_r(t, ptr::addr_of_mut!(TM_BUF))
}

#[no_mangle]
pub unsafe extern "C" fn timegm(tm: *mut Tm) -> time_t {
    let t = tm_to_secs(&*tm);
    gmtime_r(&t, tm);
    t
}
#[no_mangle]
pub unsafe extern "C" fn timelocal(tm: *mut Tm) -> time_t {
    mktime(tm)
}

#[no_mangle]
pub unsafe extern "C" fn mktime(tm: *mut Tm) -> time_t {
    let local = tm_to_secs(&*tm);
    // try the offsets around this time; prefer the one matching tm_isdst
    let o1 = type_at(local - 86400 / 2).off;
    let mut t = local - type_at(local - o1).off;
    let ty = type_at(t);
    t = local - ty.off;
    if (*tm).tm_isdst >= 0 && ((*tm).tm_isdst > 0) != ty.dst {
        // the other interpretation, if it exists (ambiguous or non-existent hour)
        for probe in [t - 7200, t + 7200] {
            let o = type_at(probe);
            if o.dst == ((*tm).tm_isdst > 0) {
                t = local - o.off;
                break;
            }
        }
    }
    if localtime_r(&t, tm).is_null() {
        return -1;
    }
    t
}

// --- textual forms ------------------------------------------------------------------------------

static mut ASC_BUF: [u8; 32] = [0; 32];

unsafe fn asctime_into(tm: *const Tm, buf: *mut c_char) -> *mut c_char {
    let tm = &*tm;
    let mut v: Vec<u8> = Vec::new();
    v.extend_from_slice(day_name(tm.tm_wday.rem_euclid(7) as usize, true));
    v.push(b' ');
    v.extend_from_slice(month_name(tm.tm_mon.rem_euclid(12) as usize, true));
    let mut t = Vec::new();
    push_num(&mut t, tm.tm_mday as i64, 3, b' ');
    v.extend_from_slice(&t);
    v.push(b' ');
    t.clear();
    push_num(&mut t, tm.tm_hour as i64, 2, b'0');
    t.push(b':');
    push_num(&mut t, tm.tm_min as i64, 2, b'0');
    t.push(b':');
    push_num(&mut t, tm.tm_sec as i64, 2, b'0');
    t.push(b' ');
    push_num(&mut t, tm.tm_year as i64 + 1900, 1, b'0');
    t.push(b'\n');
    v.extend_from_slice(&t);
    let n = v.len().min(25);
    ptr::copy_nonoverlapping(v.as_ptr(), buf, n);
    *buf.add(n) = 0;
    buf
}

#[no_mangle]
pub unsafe extern "C" fn asctime_r(tm: *const Tm, buf: *mut c_char) -> *mut c_char {
    asctime_into(tm, buf)
}
#[no_mangle]
pub unsafe extern "C" fn asctime(tm: *const Tm) -> *mut c_char {
    asctime_into(tm, ptr::addr_of_mut!(ASC_BUF) as *mut c_char)
}
#[no_mangle]
pub unsafe extern "C" fn ctime_r(t: *const time_t, buf: *mut c_char) -> *mut c_char {
    let mut tm = TM_BUF;
    if localtime_r(t, &mut tm).is_null() {
        return ptr::null_mut();
    }
    asctime_into(&tm, buf)
}
#[no_mangle]
pub unsafe extern "C" fn ctime(t: *const time_t) -> *mut c_char {
    ctime_r(t, ptr::addr_of_mut!(ASC_BUF) as *mut c_char)
}
#[no_mangle]
pub extern "C" fn difftime(a: time_t, b: time_t) -> f64 {
    (a - b) as f64
}

#[no_mangle]
pub unsafe extern "C" fn clock() -> c_long {
    let mut ts = [0i64; 2];
    if fl::sys::sc2(228 /* clock_gettime */, 2 /* CLOCK_PROCESS_CPUTIME_ID */, ts.as_mut_ptr() as usize) < 0 {
        return -1;
    }
    ts[0] * 1_000_000 + ts[1] / 1000
}

#[no_mangle]
pub unsafe extern "C" fn gettimeofday(tv: *mut [i64; 2], _tz: *mut c_void) -> c_int {
    if tv.is_null() {
        return 0;
    }
    let mut ts = [0i64; 2];
    fl::time::clock_gettime(0, ts.as_mut_ptr() as *mut _);
    (*tv)[0] = ts[0];
    (*tv)[1] = ts[1] / 1000;
    0
}
#[no_mangle]
pub unsafe extern "C" fn settimeofday(tv: *const c_void, tz: *const c_void) -> c_int {
    super::sysfns::settimeofday_raw(tv as *mut c_void, tz as *mut c_void)
}
#[no_mangle]
pub unsafe extern "C" fn stime(t: *const time_t) -> c_int {
    let tv = [*t, 0];
    settimeofday(tv.as_ptr() as *const c_void, ptr::null())
}
#[no_mangle]
pub unsafe extern "C" fn ftime(tb: *mut c_void) -> c_int {
    let mut ts = [0i64; 2];
    fl::time::clock_gettime(0, ts.as_mut_ptr() as *mut _);
    let p = tb as *mut u8;
    *(p as *mut i64) = ts[0];
    *(p.add(8) as *mut u16) = (ts[1] / 1_000_000) as u16;
    *(p.add(10) as *mut i16) = 0;
    *(p.add(12) as *mut i16) = 0;
    0
}

// --- strftime ------------------------------------------------------------------------------------

fn push_num(out: &mut Vec<u8>, v: i64, width: usize, pad: u8) {
    let neg = v < 0;
    let mut d = [0u8; 24];
    let mut n = 0;
    let mut x = v.unsigned_abs();
    loop {
        d[n] = b'0' + (x % 10) as u8;
        n += 1;
        x /= 10;
        if x == 0 {
            break;
        }
    }
    let total = n + neg as usize;
    if neg && pad == b'0' {
        out.push(b'-');
    }
    for _ in total..width {
        if pad != 0 {
            out.push(pad);
        }
    }
    if neg && pad != b'0' {
        out.push(b'-');
    }
    for i in (0..n).rev() {
        out.push(d[i]);
    }
}

/// ISO 8601 week-based year and week number.
fn iso_week(tm: &Tm) -> (i64, i64) {
    let y = tm.tm_year as i64 + 1900;
    let wday = (tm.tm_wday as i64 + 6) % 7; // Monday 0
    let yday = tm.tm_yday as i64;
    let mut week = (yday - wday + 10) / 7;
    let mut year = y;
    if week < 1 {
        year -= 1;
        let pyd = if is_leap(year) { 366 } else { 365 };
        week = (yday + pyd - wday + 10) / 7;
    } else {
        let yd = if is_leap(y) { 366 } else { 365 };
        if yday - wday + 3 >= yd {
            // belongs to week 1 of next year
            week = 1;
            year += 1;
        }
    }
    (year, week)
}

unsafe fn strftime_into(out: &mut Vec<u8>, f: &[u8], tm: &Tm) {
    let mut i = 0;
    while i < f.len() {
        if f[i] != b'%' {
            out.push(f[i]);
            i += 1;
            continue;
        }
        i += 1;
        // flags, width, E/O
        let mut pad: Option<u8> = None;
        let mut upper = false;
        let mut swap = false;
        while i < f.len() && matches!(f[i], b'-' | b'_' | b'0' | b'^' | b'#') {
            match f[i] {
                b'-' => pad = Some(0),
                b'_' => pad = Some(b' '),
                b'0' => pad = Some(b'0'),
                b'^' => upper = true,
                _ => swap = true,
            }
            i += 1;
        }
        let mut width = 0usize;
        while i < f.len() && f[i].is_ascii_digit() {
            width = width * 10 + (f[i] - b'0') as usize;
            i += 1;
        }
        if i < f.len() && (f[i] == b'E' || f[i] == b'O') {
            i += 1;
        }
        if i >= f.len() {
            break;
        }
        let c = f[i];
        i += 1;
        let mut s: Vec<u8> = Vec::new();
        let num = |s: &mut Vec<u8>, v: i64, w: usize, dpad: u8| {
            let p = pad.unwrap_or(dpad);
            push_num(s, v, w.max(width), p);
        };
        let year = tm.tm_year as i64 + 1900;
        let hour12 = if tm.tm_hour % 12 == 0 { 12 } else { tm.tm_hour % 12 };
        match c {
            b'a' => s.extend_from_slice(day_name(tm.tm_wday.rem_euclid(7) as usize, true)),
            b'A' => s.extend_from_slice(day_name(tm.tm_wday.rem_euclid(7) as usize, false)),
            b'b' | b'h' => s.extend_from_slice(month_name(tm.tm_mon.rem_euclid(12) as usize, true)),
            b'B' => s.extend_from_slice(month_name(tm.tm_mon.rem_euclid(12) as usize, false)),
            b'c' => strftime_into(&mut s, b"%a %b %e %H:%M:%S %Y", tm),
            b'C' => num(&mut s, year.div_euclid(100), 2, b'0'),
            b'd' => num(&mut s, tm.tm_mday as i64, 2, b'0'),
            b'D' => strftime_into(&mut s, b"%m/%d/%y", tm),
            b'e' => num(&mut s, tm.tm_mday as i64, 2, b' '),
            b'F' => strftime_into(&mut s, b"%Y-%m-%d", tm),
            b'g' => num(&mut s, iso_week(tm).0.rem_euclid(100), 2, b'0'),
            b'G' => num(&mut s, iso_week(tm).0, 1, b'0'),
            b'H' => num(&mut s, tm.tm_hour as i64, 2, b'0'),
            b'I' => num(&mut s, hour12 as i64, 2, b'0'),
            b'j' => num(&mut s, tm.tm_yday as i64 + 1, 3, b'0'),
            b'k' => num(&mut s, tm.tm_hour as i64, 2, b' '),
            b'l' => num(&mut s, hour12 as i64, 2, b' '),
            b'm' => num(&mut s, tm.tm_mon as i64 + 1, 2, b'0'),
            b'M' => num(&mut s, tm.tm_min as i64, 2, b'0'),
            b'n' => s.push(b'\n'),
            b'p' => s.extend_from_slice(if tm.tm_hour >= 12 { b"PM" } else { b"AM" }),
            b'P' => s.extend_from_slice(if tm.tm_hour >= 12 { b"pm" } else { b"am" }),
            b'r' => strftime_into(&mut s, b"%I:%M:%S %p", tm),
            b'R' => strftime_into(&mut s, b"%H:%M", tm),
            b's' => {
                let mut t = *tm;
                let v = mktime(&mut t);
                num(&mut s, v, 1, b'0');
            }
            b'S' => num(&mut s, tm.tm_sec as i64, 2, b'0'),
            b't' => s.push(b'\t'),
            b'T' => strftime_into(&mut s, b"%H:%M:%S", tm),
            b'u' => num(&mut s, if tm.tm_wday == 0 { 7 } else { tm.tm_wday as i64 }, 1, b'0'),
            b'U' => num(&mut s, (tm.tm_yday as i64 + 7 - tm.tm_wday as i64) / 7, 2, b'0'),
            b'V' => num(&mut s, iso_week(tm).1, 2, b'0'),
            b'w' => num(&mut s, tm.tm_wday as i64, 1, b'0'),
            b'W' => num(&mut s, (tm.tm_yday as i64 + 7 - (tm.tm_wday as i64 + 6) % 7) / 7, 2, b'0'),
            b'x' => strftime_into(&mut s, b"%m/%d/%y", tm),
            b'X' => strftime_into(&mut s, b"%H:%M:%S", tm),
            b'y' => num(&mut s, year.rem_euclid(100), 2, b'0'),
            b'Y' => num(&mut s, year, 1, b'0'),
            b'z' => {
                let o = tm.tm_gmtoff;
                s.push(if o < 0 { b'-' } else { b'+' });
                let a = o.abs();
                push_num(&mut s, a / 3600 * 100 + a / 60 % 60, 4, b'0');
            }
            b'Z' => {
                if !tm.tm_zone.is_null() {
                    s.extend_from_slice(fl::mem::cstr(tm.tm_zone));
                }
            }
            b'%' => s.push(b'%'),
            b'+' => strftime_into(&mut s, b"%a %b %e %H:%M:%S %Z %Y", tm),
            other => {
                s.push(b'%');
                s.push(other);
            }
        }
        if upper {
            s.iter_mut().for_each(|c| *c = c.to_ascii_uppercase());
        }
        if swap {
            if s.iter().any(|c| c.is_ascii_lowercase()) {
                s.iter_mut().for_each(|c| *c = c.to_ascii_uppercase());
            } else {
                s.iter_mut().for_each(|c| *c = c.to_ascii_lowercase());
            }
        }
        if width > s.len() && !s.first().map_or(false, |c| c.is_ascii_digit() || *c == b'-' || *c == b'+' || *c == b' ') {
            for _ in s.len()..width {
                out.push(if pad == Some(b'0') { b'0' } else { b' ' });
            }
        }
        out.extend_from_slice(&s);
    }
}

#[no_mangle]
pub unsafe extern "C" fn strftime(buf: *mut c_char, max: size_t, fmt: *const c_char, tm: *const Tm) -> size_t {
    let mut out = Vec::new();
    strftime_into(&mut out, fl::mem::cstr(fmt), &*tm);
    if out.len() + 1 > max {
        if max > 0 {
            *buf = 0;
        }
        return 0;
    }
    ptr::copy_nonoverlapping(out.as_ptr(), buf, out.len());
    *buf.add(out.len()) = 0;
    out.len()
}
#[no_mangle]
pub unsafe extern "C" fn strftime_l(buf: *mut c_char, max: size_t, fmt: *const c_char, tm: *const Tm, _l: *mut c_void) -> size_t {
    strftime(buf, max, fmt, tm)
}
#[no_mangle]
pub unsafe extern "C" fn __strftime_l(buf: *mut c_char, max: size_t, fmt: *const c_char, tm: *const Tm, _l: *mut c_void) -> size_t {
    strftime(buf, max, fmt, tm)
}

#[no_mangle]
pub unsafe extern "C" fn wcsftime(buf: *mut i32, max: size_t, fmt: *const i32, tm: *const Tm) -> size_t {
    // narrow the format, format, widen the result
    let mut f = Vec::new();
    let mut p = fmt;
    while *p != 0 {
        let mut b = [0u8; 4];
        let n = super::locale::utf8_encode(*p as u32, &mut b);
        f.extend_from_slice(&b[..n]);
        p = p.add(1);
    }
    let mut out = Vec::new();
    strftime_into(&mut out, &f, &*tm);
    let s = core::str::from_utf8(&out).unwrap_or("");
    let w: Vec<i32> = s.chars().map(|c| c as i32).collect();
    if w.len() + 1 > max {
        return 0;
    }
    ptr::copy_nonoverlapping(w.as_ptr(), buf, w.len());
    *buf.add(w.len()) = 0;
    w.len()
}
#[no_mangle]
pub unsafe extern "C" fn wcsftime_l(buf: *mut i32, max: size_t, fmt: *const i32, tm: *const Tm, _l: *mut c_void) -> size_t {
    wcsftime(buf, max, fmt, tm)
}
#[no_mangle]
pub unsafe extern "C" fn __wcsftime_l(buf: *mut i32, max: size_t, fmt: *const i32, tm: *const Tm, _l: *mut c_void) -> size_t {
    wcsftime(buf, max, fmt, tm)
}

// --- strptime -------------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn strptime(s: *const c_char, fmt: *const c_char, tm: *mut Tm) -> *mut c_char {
    let f = fl::mem::cstr(fmt);
    let mut p = s;
    let tm = &mut *tm;
    let mut pm: Option<bool> = None;
    let mut i = 0;
    let skip_ws = |p: &mut *const c_char| {
        while matches!(**p, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
            *p = p.add(1);
        }
    };
    let num = |p: &mut *const c_char, max_digits: usize, lo: i64, hi: i64| -> Option<i64> {
        let mut neg = false;
        if **p == b'-' || **p == b'+' {
            neg = **p == b'-';
            *p = p.add(1);
        }
        let mut v = 0i64;
        let mut n = 0;
        while n < max_digits && (**p).is_ascii_digit() {
            v = v * 10 + (**p - b'0') as i64;
            *p = p.add(1);
            n += 1;
        }
        if n == 0 {
            return None;
        }
        let v = if neg { -v } else { v };
        if v < lo || v > hi { None } else { Some(v) }
    };
    let name = |p: &mut *const c_char, months: bool| -> Option<usize> {
        let n = if months { 12 } else { 7 };
        for k in 0..n {
            for abbr in [false, true] {
                let w = if months { month_name(k, abbr) } else { day_name(k, abbr) };
                let mut ok = true;
                for (j, &c) in w.iter().enumerate() {
                    if (*p.add(j)).to_ascii_lowercase() != c.to_ascii_lowercase() {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    *p = p.add(w.len());
                    return Some(k);
                }
            }
        }
        None
    };
    while i < f.len() {
        let c = f[i];
        if c == b' ' || c == b'\t' || c == b'\n' {
            skip_ws(&mut p);
            i += 1;
            continue;
        }
        if c != b'%' {
            if *p != c {
                return ptr::null_mut();
            }
            p = p.add(1);
            i += 1;
            continue;
        }
        i += 1;
        while i < f.len() && matches!(f[i], b'E' | b'O' | b'-' | b'_' | b'0' | b'^' | b'#') {
            i += 1;
        }
        if i >= f.len() {
            break;
        }
        let k = f[i];
        i += 1;
        if k != b'n' && k != b't' && k != b'%' {
            skip_ws(&mut p);
        }
        macro_rules! get {
            ($d:expr, $lo:expr, $hi:expr) => {
                match num(&mut p, $d, $lo, $hi) {
                    Some(v) => v,
                    None => return ptr::null_mut(),
                }
            };
        }
        match k {
            b'a' | b'A' => match name(&mut p, false) {
                Some(d) => tm.tm_wday = d as c_int,
                None => return ptr::null_mut(),
            },
            b'b' | b'B' | b'h' => match name(&mut p, true) {
                Some(m) => tm.tm_mon = m as c_int,
                None => return ptr::null_mut(),
            },
            b'd' | b'e' => tm.tm_mday = get!(2, 1, 31) as c_int,
            b'H' | b'k' => tm.tm_hour = get!(2, 0, 23) as c_int,
            b'I' | b'l' => tm.tm_hour = (get!(2, 1, 12) % 12) as c_int,
            b'j' => tm.tm_yday = get!(3, 1, 366) as c_int - 1,
            b'm' => tm.tm_mon = get!(2, 1, 12) as c_int - 1,
            b'M' => tm.tm_min = get!(2, 0, 59) as c_int,
            b'S' => tm.tm_sec = get!(2, 0, 61) as c_int,
            b'y' => {
                let y = get!(2, 0, 99);
                tm.tm_year = (if y < 69 { y + 100 } else { y }) as c_int;
            }
            b'Y' => tm.tm_year = (get!(4, -9999, 9999) - 1900) as c_int,
            b'C' => {
                let c = get!(2, 0, 99);
                tm.tm_year = (c * 100 - 1900 + (tm.tm_year as i64).rem_euclid(100)) as c_int;
            }
            b'w' => tm.tm_wday = get!(1, 0, 6) as c_int,
            b'u' => tm.tm_wday = (get!(1, 1, 7) % 7) as c_int,
            b'p' | b'P' => {
                let a = (*p).to_ascii_lowercase();
                let b = (*p.add(1)).to_ascii_lowercase();
                if b != b'm' || (a != b'a' && a != b'p') {
                    return ptr::null_mut();
                }
                pm = Some(a == b'p');
                p = p.add(2);
            }
            b'D' => {
                let r = strptime(p, b"%m/%d/%y\0".as_ptr(), tm);
                if r.is_null() {
                    return r;
                }
                p = r;
            }
            b'F' => {
                let r = strptime(p, b"%Y-%m-%d\0".as_ptr(), tm);
                if r.is_null() {
                    return r;
                }
                p = r;
            }
            b'T' => {
                let r = strptime(p, b"%H:%M:%S\0".as_ptr(), tm);
                if r.is_null() {
                    return r;
                }
                p = r;
            }
            b'R' => {
                let r = strptime(p, b"%H:%M\0".as_ptr(), tm);
                if r.is_null() {
                    return r;
                }
                p = r;
            }
            b'r' => {
                let r = strptime(p, b"%I:%M:%S %p\0".as_ptr(), tm);
                if r.is_null() {
                    return r;
                }
                p = r;
            }
            b'c' => {
                let r = strptime(p, b"%a %b %e %H:%M:%S %Y\0".as_ptr(), tm);
                if r.is_null() {
                    return r;
                }
                p = r;
            }
            b's' => {
                let v = get!(20, i64::MIN / 2, i64::MAX / 2);
                localtime_r(&v, tm);
            }
            b'z' => {
                if *p == b'Z' {
                    p = p.add(1);
                    tm.tm_gmtoff = 0;
                } else {
                    let neg = *p == b'-';
                    if *p != b'+' && *p != b'-' {
                        return ptr::null_mut();
                    }
                    p = p.add(1);
                    let mut v = 0i64;
                    let mut n = 0;
                    while n < 4 && ((*p).is_ascii_digit() || (*p == b':' && n == 2)) {
                        if *p != b':' {
                            v = v * 10 + (*p - b'0') as i64;
                            n += 1;
                        }
                        p = p.add(1);
                    }
                    let off = (v / 100) * 3600 + (v % 100) * 60;
                    tm.tm_gmtoff = if neg { -off } else { off };
                }
            }
            b'Z' => {
                while (*p).is_ascii_alphabetic() {
                    p = p.add(1);
                }
            }
            b'n' | b't' => skip_ws(&mut p),
            b'%' => {
                if *p != b'%' {
                    return ptr::null_mut();
                }
                p = p.add(1);
            }
            _ => return ptr::null_mut(),
        }
    }
    if let Some(is_pm) = pm {
        tm.tm_hour = tm.tm_hour % 12 + if is_pm { 12 } else { 0 };
    }
    p as *mut c_char
}

#[no_mangle]
pub unsafe extern "C" fn strptime_l(s: *const c_char, fmt: *const c_char, tm: *mut Tm, _l: *mut c_void) -> *mut c_char {
    strptime(s, fmt, tm)
}

#[no_mangle]
pub unsafe extern "C" fn dysize(y: c_int) -> c_int {
    if is_leap(y as i64) { 366 } else { 365 }
}
