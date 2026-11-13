//! scanf (all the forms glibc has: plain, __isoc99_ and __isoc23_) and strtod/strtof/
//! strtold. Decimal numbers go through Rust's correctly rounded parser; hexadecimal floats
//! are converted here.

use super::printf::{RawVa, Va};
use super::stdio::{self, FILE};
use super::umem;
use crate::fl;
use alloc::vec::Vec;
use core::ffi::{c_void, VaList};
use core::ptr;

type c_char = u8;
type c_int = i32;
type size_t = usize;

// --- strtod -----------------------------------------------------------------------------------

fn hex_val(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

/// Parse a floating-point number at the start of `s` (after optional whitespace). Returns
/// the value and the number of bytes used (0: no number).
pub fn parse_float(s: &[u8]) -> (f64, usize) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let start_ws = i;
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let lower = |j: usize, w: &[u8]| -> bool {
        j + w.len() <= s.len() && s[j..j + w.len()].iter().zip(w).all(|(a, b)| a.to_ascii_lowercase() == *b)
    };
    let sign = |x: f64| if neg { -x } else { x };
    if lower(i, b"infinity") {
        return (sign(f64::INFINITY), i + 8);
    }
    if lower(i, b"inf") {
        return (sign(f64::INFINITY), i + 3);
    }
    if lower(i, b"nan") {
        let mut j = i + 3;
        if j < s.len() && s[j] == b'(' {
            let mut k = j + 1;
            while k < s.len() && (s[k].is_ascii_alphanumeric() || s[k] == b'_') {
                k += 1;
            }
            if k < s.len() && s[k] == b')' {
                j = k + 1;
            }
        }
        return (sign(f64::NAN), j);
    }
    // hexadecimal
    if i + 1 < s.len() && s[i] == b'0' && (s[i + 1] | 0x20) == b'x' {
        let mut j = i + 2;
        let mut mant: u64 = 0;
        let mut exp: i64 = 0;
        let mut any = false;
        let mut sticky = false;
        while j < s.len() {
            if let Some(d) = hex_val(s[j]) {
                any = true;
                if mant >> 60 == 0 {
                    mant = (mant << 4) | d as u64;
                } else {
                    exp += 4;
                    sticky |= d != 0;
                }
                j += 1;
            } else {
                break;
            }
        }
        if j < s.len() && s[j] == b'.' {
            j += 1;
            while j < s.len() {
                if let Some(d) = hex_val(s[j]) {
                    any = true;
                    if mant >> 60 == 0 {
                        mant = (mant << 4) | d as u64;
                        exp -= 4;
                    } else {
                        sticky |= d != 0;
                    }
                    j += 1;
                } else {
                    break;
                }
            }
        }
        if !any {
            // just "0": the x is not part of the number
            return (sign(0.0), i + 1);
        }
        if j < s.len() && (s[j] | 0x20) == b'p' {
            let mut k = j + 1;
            let mut eneg = false;
            if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
                eneg = s[k] == b'-';
                k += 1;
            }
            let ks = k;
            let mut e: i64 = 0;
            while k < s.len() && s[k].is_ascii_digit() {
                e = (e * 10 + (s[k] - b'0') as i64).min(1 << 20);
                k += 1;
            }
            if k > ks {
                exp += if eneg { -e } else { e };
                j = k;
            }
        }
        let _ = sticky;
        let v = (mant as f64) * pow2(exp);
        return (sign(v), j);
    }
    // decimal: find the longest valid prefix and let Rust parse it
    let ds = i;
    let mut j = i;
    let mut digits = false;
    while j < s.len() && s[j].is_ascii_digit() {
        j += 1;
        digits = true;
    }
    if j < s.len() && s[j] == b'.' {
        j += 1;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
            digits = true;
        }
    }
    if !digits {
        let _ = start_ws;
        return (0.0, 0);
    }
    if j < s.len() && (s[j] | 0x20) == b'e' {
        let mut k = j + 1;
        if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
            k += 1;
        }
        let ks = k;
        while k < s.len() && s[k].is_ascii_digit() {
            k += 1;
        }
        if k > ks {
            j = k;
        }
    }
    let txt = unsafe { core::str::from_utf8_unchecked(&s[ds..j]) };
    let v: f64 = txt.parse().unwrap_or(0.0);
    (sign(v), j)
}

fn pow2(e: i64) -> f64 {
    let mut v = 1.0f64;
    let mut e = e;
    while e > 1000 {
        v *= f64::from_bits(((1000 + 1023) as u64) << 52);
        e -= 1000;
    }
    while e < -1000 {
        v *= f64::from_bits(((-1000i64 + 1023) as u64) << 52);
        e += 1000;
    }
    v * f64::from_bits(((e + 1023) as u64) << 52)
}

pub unsafe fn strtod_impl(s: *const c_char, end: *mut *mut c_char) -> f64 {
    let n = fl::mem::cstrlen(s);
    let (v, used) = parse_float(core::slice::from_raw_parts(s, n));
    if !end.is_null() {
        *end = s.add(used) as *mut c_char;
    }
    if used > 0 && (v.is_infinite() || (v == 0.0 && {
        // an underflow: digits that were not all zero
        let t = core::slice::from_raw_parts(s, used);
        t.iter().take_while(|&&c| c != b'e' && c != b'E' && c != b'p' && c != b'P').any(|&c| (b'1'..=b'9').contains(&c))
    })) {
        let t = core::slice::from_raw_parts(s, used);
        let literal_inf = t.iter().any(|c| c.to_ascii_lowercase() == b'i');
        if !literal_inf {
            fl::errno::set(fl::errno::ERANGE);
        }
    }
    v
}

#[no_mangle]
pub unsafe extern "C" fn strtod(s: *const c_char, end: *mut *mut c_char) -> f64 {
    strtod_impl(s, end)
}
#[no_mangle]
pub unsafe extern "C" fn strtof(s: *const c_char, end: *mut *mut c_char) -> f32 {
    let v = strtod_impl(s, end);
    let f = v as f32;
    if f.is_infinite() && v.is_finite() {
        fl::errno::set(fl::errno::ERANGE);
    }
    f
}
#[no_mangle]
pub unsafe extern "C" fn strtod_l(s: *const c_char, end: *mut *mut c_char, _l: *mut c_void) -> f64 {
    strtod_impl(s, end)
}
#[no_mangle]
pub unsafe extern "C" fn __strtod_l(s: *const c_char, end: *mut *mut c_char, _l: *mut c_void) -> f64 {
    strtod_impl(s, end)
}
#[no_mangle]
pub unsafe extern "C" fn strtof_l(s: *const c_char, end: *mut *mut c_char, _l: *mut c_void) -> f32 {
    strtof(s, end)
}
#[no_mangle]
pub unsafe extern "C" fn __strtof_l(s: *const c_char, end: *mut *mut c_char, _l: *mut c_void) -> f32 {
    strtof(s, end)
}
#[no_mangle]
pub unsafe extern "C" fn __strtod_internal(s: *const c_char, end: *mut *mut c_char, _g: c_int) -> f64 {
    strtod_impl(s, end)
}
#[no_mangle]
pub unsafe extern "C" fn atof(s: *const c_char) -> f64 {
    strtod_impl(s, ptr::null_mut())
}

// long double results come back in the x87 register st(0)
core::arch::global_asm!(
    ".globl strtold",
    ".type strtold,@function",
    "strtold:",
    ".globl strtold_l",
    ".type strtold_l,@function",
    "strtold_l:",
    ".globl __strtold_l",
    ".type __strtold_l,@function",
    "__strtold_l:",
    "sub rsp, 24",
    "call {f}",
    "movsd qword ptr [rsp], xmm0",
    "fld qword ptr [rsp]",
    "add rsp, 24",
    "ret",
    f = sym strtod_impl_c,
);

unsafe extern "C" fn strtod_impl_c(s: *const c_char, end: *mut *mut c_char) -> f64 {
    strtod_impl(s, end)
}

/// _Float128 is passed and returned in xmm0 as 128 bits.
fn f64_to_f128_bits(x: f64) -> u128 {
    let b = x.to_bits();
    let sign = (b >> 63) as u128;
    let e = ((b >> 52) & 0x7ff) as i64;
    let f = (b & ((1 << 52) - 1)) as u128;
    if e == 0x7ff {
        return (sign << 127) | (0x7fffu128 << 112) | (f << 60);
    }
    if e == 0 && f == 0 {
        return sign << 127;
    }
    let (e, f) = if e == 0 {
        let lz = (f as u64).leading_zeros() as i64 - 11;
        (-1022 - lz, (f << (lz + 1)) & ((1u128 << 52) - 1))
    } else {
        (e - 1023, f)
    };
    (sign << 127) | (((e + 16383) as u128) << 112) | (f << 60)
}

fn f128_bits_to_f64(v: u128) -> f64 {
    let sign = (v >> 127) as u64;
    let e = ((v >> 112) & 0x7fff) as i64;
    let f = v & ((1u128 << 112) - 1);
    if e == 0x7fff {
        return f64::from_bits((sign << 63) | (0x7ff << 52) | if f != 0 { 1 << 51 } else { 0 });
    }
    let ex = e - 16383;
    if e == 0 || ex < -1074 {
        return f64::from_bits(sign << 63);
    }
    if ex > 1023 {
        return f64::from_bits((sign << 63) | (0x7ff << 52));
    }
    if ex < -1022 {
        let m = ((1u128 << 112) | f) >> (60 + (-1022 - ex) as u32);
        return f64::from_bits((sign << 63) | m as u64);
    }
    f64::from_bits((sign << 63) | (((ex + 1023) as u64) << 52) | (f >> 60) as u64)
}

#[no_mangle]
pub unsafe extern "C" fn strtof128(s: *const c_char, end: *mut *mut c_char) -> u128 {
    f64_to_f128_bits(strtod_impl(s, end))
}

#[no_mangle]
pub unsafe extern "C" fn strfromf128(buf: *mut c_char, n: size_t, fmt: *const c_char, v: u128) -> c_int {
    super::misc::strfrom_f64(buf, n, fmt, f128_bits_to_f64(v))
}

#[no_mangle]
pub unsafe extern "C" fn strfromd(buf: *mut c_char, n: size_t, fmt: *const c_char, v: f64) -> c_int {
    super::misc::strfrom_f64(buf, n, fmt, v)
}
#[no_mangle]
pub unsafe extern "C" fn strfromf(buf: *mut c_char, n: size_t, fmt: *const c_char, v: f32) -> c_int {
    super::misc::strfrom_f64(buf, n, fmt, v as f64)
}

// --- scanf -------------------------------------------------------------------------------------

trait Input {
    fn get(&mut self) -> Option<u8>;
    fn unget(&mut self, c: u8);
}

struct StrIn {
    p: *const u8,
}
impl Input for StrIn {
    fn get(&mut self) -> Option<u8> {
        // SAFETY: a NUL-terminated string; p never moves past the NUL.
        unsafe {
            let c = *self.p;
            if c == 0 {
                None
            } else {
                self.p = self.p.add(1);
                Some(c)
            }
        }
    }
    fn unget(&mut self, _c: u8) {
        // SAFETY: only after a successful get.
        unsafe { self.p = self.p.sub(1) }
    }
}

struct FileIn(*mut FILE);
impl Input for FileIn {
    fn get(&mut self) -> Option<u8> {
        // SAFETY: the stream is locked by the caller.
        let c = unsafe { stdio::getc_unl(self.0) };
        if c < 0 {
            None
        } else {
            Some(c as u8)
        }
    }
    fn unget(&mut self, c: u8) {
        // SAFETY: as above (ungetc takes the lock recursively).
        unsafe { stdio::ungetc(c as c_int, self.0) };
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

#[derive(PartialEq, Clone, Copy)]
enum L {
    N,
    HH,
    H,
    L,
    LL,
    LD,
}

unsafe fn store_int(p: *mut c_void, l: L, v: u64) {
    match l {
        L::HH => *(p as *mut u8) = v as u8,
        L::H => *(p as *mut u16) = v as u16,
        L::N => *(p as *mut u32) = v as u32,
        _ => *(p as *mut u64) = v,
    }
}

unsafe fn vscan<I: Input>(inp: &mut I, fmt: *const c_char, va: &mut Va, c23: bool) -> c_int {
    let f = fl::mem::cstr(fmt);
    let mut assigned = 0;
    let mut consumed = 0usize;
    let mut i = 0;
    let mut input_failed = false;
    macro_rules! getc {
        () => {{
            let c = inp.get();
            if c.is_some() {
                consumed += 1;
            }
            c
        }};
    }
    macro_rules! ungetc {
        ($c:expr) => {{
            inp.unget($c);
            consumed -= 1;
        }};
    }
    'outer: while i < f.len() {
        let fc = f[i];
        if is_space(fc) {
            while i < f.len() && is_space(f[i]) {
                i += 1;
            }
            loop {
                match getc!() {
                    Some(c) if is_space(c) => continue,
                    Some(c) => {
                        ungetc!(c);
                        break;
                    }
                    None => break,
                }
            }
            continue;
        }
        if fc != b'%' || (i + 1 < f.len() && f[i + 1] == b'%') {
            if fc == b'%' {
                i += 1;
                // %% skips leading white space
                loop {
                    match getc!() {
                        Some(c) if is_space(c) => continue,
                        Some(c) => {
                            ungetc!(c);
                            break;
                        }
                        None => break,
                    }
                }
            }
            match getc!() {
                Some(c) if c == f[i] => {
                    i += 1;
                    continue;
                }
                Some(c) => {
                    ungetc!(c);
                    break;
                }
                None => {
                    input_failed = true;
                    break;
                }
            }
        }
        // a conversion
        i += 1;
        let mut suppress = false;
        let mut alloc = false;
        let mut posarg: Option<usize> = None;
        // n$
        {
            let mut j = i;
            let mut v = 0usize;
            while j < f.len() && f[j].is_ascii_digit() {
                v = v * 10 + (f[j] - b'0') as usize;
                j += 1;
            }
            if j > i && j < f.len() && f[j] == b'$' {
                posarg = Some(v);
                i = j + 1;
            }
        }
        if i < f.len() && f[i] == b'*' {
            suppress = true;
            i += 1;
        }
        let mut width = 0usize;
        while i < f.len() && f[i].is_ascii_digit() {
            width = width * 10 + (f[i] - b'0') as usize;
            i += 1;
        }
        if i < f.len() && f[i] == b'm' {
            alloc = true;
            i += 1;
        }
        let mut l = L::N;
        while i < f.len() {
            match f[i] {
                b'h' => l = if l == L::H { L::HH } else { L::H },
                b'l' => l = if l == L::L { L::LL } else { L::L },
                b'L' | b'q' => l = if f[i] == b'q' { L::LL } else { L::LD },
                b'j' | b'z' | b't' => l = L::LL,
                _ => break,
            }
            i += 1;
        }
        if i >= f.len() {
            break;
        }
        let conv = f[i];
        i += 1;
        let _ = posarg;
        let dest = |va: &mut Va| -> *mut c_void { va.int() as usize as *mut c_void };
        if conv != b'[' && conv != b'c' && conv != b'n' {
            loop {
                match getc!() {
                    Some(c) if is_space(c) => continue,
                    Some(c) => {
                        ungetc!(c);
                        break;
                    }
                    None => {
                        input_failed = true;
                        break 'outer;
                    }
                }
            }
        }
        let w = if width == 0 { usize::MAX } else { width };
        match conv {
            b'n' => {
                if !suppress {
                    store_int(dest(va), l, consumed as u64);
                }
            }
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'p' | b'b' => {
                let mut buf: Vec<u8> = Vec::new();
                let mut base = match conv {
                    b'd' | b'u' => 10,
                    b'o' => 8,
                    b'x' | b'X' | b'p' => 16,
                    b'b' => 2,
                    _ => 0,
                };
                let mut n = 0;
                let mut c = getc!();
                if let Some(ch) = c {
                    if (ch == b'+' || ch == b'-') && n < w {
                        buf.push(ch);
                        n += 1;
                        c = getc!();
                    }
                }
                if let Some(b'0') = c {
                    if n < w {
                        buf.push(b'0');
                        n += 1;
                        c = getc!();
                        if (base == 0 || base == 16) && matches!(c, Some(b'x') | Some(b'X')) && n < w {
                            buf.push(c.unwrap());
                            n += 1;
                            base = 16;
                            c = getc!();
                        } else if c23 && (base == 0 || base == 2) && matches!(c, Some(b'b') | Some(b'B')) && n < w {
                            buf.push(c.unwrap());
                            n += 1;
                            base = 2;
                            c = getc!();
                        } else if base == 0 {
                            base = 8;
                        }
                    }
                }
                if base == 0 {
                    base = 10;
                }
                let mut digits = buf.last() == Some(&b'0');
                while let Some(ch) = c {
                    if n >= w {
                        break;
                    }
                    let ok = match base {
                        16 => ch.is_ascii_hexdigit(),
                        8 => (b'0'..=b'7').contains(&ch),
                        2 => ch == b'0' || ch == b'1',
                        _ => ch.is_ascii_digit(),
                    };
                    if !ok {
                        break;
                    }
                    buf.push(ch);
                    n += 1;
                    digits = true;
                    c = getc!();
                }
                if let Some(ch) = c {
                    ungetc!(ch);
                }
                if !digits {
                    break 'outer;
                }
                buf.push(0);
                let signed = conv == b'd' || conv == b'i';
                let v = if signed {
                    super::string::parse_int(buf.as_ptr(), ptr::null_mut(), base, c23).map_or(0, |(m, neg, _)| if neg { m.wrapping_neg() } else { m })
                } else {
                    super::string::parse_int(buf.as_ptr(), ptr::null_mut(), base, c23).map_or(0, |(m, neg, _)| if neg { m.wrapping_neg() } else { m })
                };
                if !suppress {
                    let l = if conv == b'p' { L::LL } else { l };
                    store_int(dest(va), l, v);
                    assigned += 1;
                }
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
                let mut buf: Vec<u8> = Vec::new();
                let mut n = 0;
                while n < w {
                    match getc!() {
                        Some(ch) => {
                            buf.push(ch);
                            // keep only what still can be (the start of) a number
                            let (_, used) = parse_float(&buf);
                            let plausible = used == buf.len() || {
                                let last = ch.to_ascii_lowercase();
                                matches!(last, b'e' | b'p' | b'+' | b'-' | b'x' | b'.' | b'i' | b'n' | b'f' | b't' | b'y' | b'a')
                                    && buf.len() < 64
                            };
                            if !plausible {
                                buf.pop();
                                ungetc!(ch);
                                break;
                            }
                            n += 1;
                        }
                        None => break,
                    }
                }
                let (v, used) = parse_float(&buf);
                // give back what the number did not use
                while buf.len() > used {
                    let ch = buf.pop().unwrap();
                    ungetc!(ch);
                }
                if used == 0 {
                    break 'outer;
                }
                if !suppress {
                    let p = dest(va);
                    match l {
                        L::L | L::LL => *(p as *mut f64) = v,
                        L::LD => super::printf::f64_to_ld(v, p as *mut u8),
                        _ => *(p as *mut f32) = v as f32,
                    }
                    assigned += 1;
                }
            }
            b's' | b'c' | b'[' => {
                // the set for %[
                let mut set = [false; 256];
                if conv == b'[' {
                    let mut neg = false;
                    if i < f.len() && f[i] == b'^' {
                        neg = true;
                        i += 1;
                    }
                    let st = i;
                    while i < f.len() && (f[i] != b']' || i == st) {
                        if f[i] == b'-' && i > st && i + 1 < f.len() && f[i + 1] != b']' {
                            let (a, b) = (f[i - 1], f[i + 1]);
                            for x in a.min(b)..=a.max(b) {
                                set[x as usize] = true;
                            }
                            i += 2;
                            continue;
                        }
                        set[f[i] as usize] = true;
                        i += 1;
                    }
                    i += 1; // ']'
                    if neg {
                        for x in set.iter_mut() {
                            *x = !*x;
                        }
                    }
                }
                let w = if conv == b'c' && width == 0 { 1 } else { w };
                let mut out: Vec<u8> = Vec::new();
                let mut n = 0;
                while n < w {
                    match getc!() {
                        Some(ch) => {
                            let take = match conv {
                                b's' => !is_space(ch),
                                b'[' => set[ch as usize],
                                _ => true,
                            };
                            if !take {
                                ungetc!(ch);
                                break;
                            }
                            out.push(ch);
                            n += 1;
                        }
                        None => break,
                    }
                }
                if out.is_empty() {
                    if conv != b'[' || inp_is_eof(&out) {
                        input_failed = assigned == 0;
                    }
                    break 'outer;
                }
                if conv == b'c' && out.len() < w && width != 0 {
                    // fewer characters than asked: still stored, but a failure
                }
                if !suppress {
                    let wide = l == L::L;
                    let term = conv != b'c';
                    if wide {
                        let mut wv: Vec<i32> = Vec::new();
                        let s = core::str::from_utf8(&out).map(|s| s.chars().map(|c| c as i32).collect()).unwrap_or_else(|_| out.iter().map(|&b| b as i32).collect::<Vec<i32>>());
                        wv.extend(s);
                        if term {
                            wv.push(0);
                        }
                        let p = if alloc {
                            let m = umem::malloc(wv.len() * 4) as *mut i32;
                            *(dest(va) as *mut *mut i32) = m;
                            m
                        } else {
                            dest(va) as *mut i32
                        };
                        if !p.is_null() {
                            ptr::copy_nonoverlapping(wv.as_ptr(), p, wv.len());
                        }
                    } else {
                        if term {
                            out.push(0);
                        }
                        let p = if alloc {
                            let m = umem::malloc(out.len()) as *mut u8;
                            *(dest(va) as *mut *mut u8) = m;
                            m
                        } else {
                            dest(va) as *mut u8
                        };
                        if !p.is_null() {
                            ptr::copy_nonoverlapping(out.as_ptr(), p, out.len());
                        }
                    }
                    assigned += 1;
                }
            }
            _ => break,
        }
    }
    if input_failed && assigned == 0 {
        return -1;
    }
    assigned
}

fn inp_is_eof(_v: &Vec<u8>) -> bool {
    true
}

pub unsafe fn vsscanf_va(s: *const c_char, fmt: *const c_char, va: &mut Va, c23: bool) -> c_int {
    let mut inp = StrIn { p: s };
    vscan(&mut inp, fmt, va, c23)
}

pub unsafe fn vfscanf_va(f: *mut FILE, fmt: *const c_char, va: &mut Va, c23: bool) -> c_int {
    let _l = stdio::locked(f);
    let mut inp = FileIn(f);
    vscan(&mut inp, fmt, va, c23)
}

macro_rules! va_of {
    ($args:ident) => {
        Va(&mut $args as *mut _ as *mut RawVa)
    };
}

macro_rules! scan_family {
    ($c23:expr, $sscanf:ident, $fscanf:ident, $scanf:ident, $vsscanf:ident, $vfscanf:ident, $vscanf:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn $sscanf(s: *const c_char, fmt: *const c_char, mut args: ...) -> c_int {
            vsscanf_va(s, fmt, &mut va_of!(args), $c23)
        }
        #[no_mangle]
        pub unsafe extern "C" fn $fscanf(f: *mut FILE, fmt: *const c_char, mut args: ...) -> c_int {
            vfscanf_va(f, fmt, &mut va_of!(args), $c23)
        }
        #[no_mangle]
        pub unsafe extern "C" fn $scanf(fmt: *const c_char, mut args: ...) -> c_int {
            vfscanf_va(stdio::stdin_file(), fmt, &mut va_of!(args), $c23)
        }
        #[no_mangle]
        pub unsafe extern "C" fn $vsscanf(s: *const c_char, fmt: *const c_char, mut ap: VaList) -> c_int {
            vsscanf_va(s, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa), $c23)
        }
        #[no_mangle]
        pub unsafe extern "C" fn $vfscanf(f: *mut FILE, fmt: *const c_char, mut ap: VaList) -> c_int {
            vfscanf_va(f, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa), $c23)
        }
        #[no_mangle]
        pub unsafe extern "C" fn $vscanf(fmt: *const c_char, mut ap: VaList) -> c_int {
            vfscanf_va(stdio::stdin_file(), fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa), $c23)
        }
    };
}

scan_family!(false, sscanf, fscanf, scanf, vsscanf, vfscanf, vscanf);
scan_family!(false, __isoc99_sscanf, __isoc99_fscanf, __isoc99_scanf, __isoc99_vsscanf, __isoc99_vfscanf, __isoc99_vscanf);
scan_family!(true, __isoc23_sscanf, __isoc23_fscanf, __isoc23_scanf, __isoc23_vsscanf, __isoc23_vfscanf, __isoc23_vscanf);
