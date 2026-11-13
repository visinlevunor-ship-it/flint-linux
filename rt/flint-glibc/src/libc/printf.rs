//! printf: the formatter and every function built on it (printf, fprintf, sprintf,
//! snprintf, asprintf, dprintf, their v- forms and glibc's _chk variants).
//!
//! Arguments are read straight from the x86-64 va_list (so long double works), positional
//! arguments (%2$s, used by translated messages) included. Floating point goes through
//! Rust's exact float formatting.

use super::stdio::{self, FILE};
use super::umem;
use crate::fl;
use alloc::vec::Vec;
use core::ffi::{c_void, VaList};
use core::fmt::Write;
use core::ptr;

type c_char = u8;
type c_int = i32;
type size_t = usize;

// --- reading arguments ---------------------------------------------------------------------

#[repr(C)]
pub struct RawVa {
    gp_offset: u32,
    fp_offset: u32,
    overflow: *mut u8,
    reg_save: *mut u8,
}

pub struct Va(pub *mut RawVa);

impl Va {
    pub unsafe fn from(mut ap: VaList) -> Va {
        Va(core::mem::transmute_copy::<VaList, *mut RawVa>(&ap))
    }
    pub unsafe fn int(&mut self) -> u64 {
        let v = &mut *self.0;
        if v.gp_offset < 48 {
            let x = *(v.reg_save.add(v.gp_offset as usize) as *const u64);
            v.gp_offset += 8;
            x
        } else {
            let x = *(v.overflow as *const u64);
            v.overflow = v.overflow.add(8);
            x
        }
    }
    pub unsafe fn dbl(&mut self) -> f64 {
        let v = &mut *self.0;
        if v.fp_offset < 176 {
            let x = *(v.reg_save.add(v.fp_offset as usize) as *const f64);
            v.fp_offset += 16;
            x
        } else {
            let x = *(v.overflow as *const f64);
            v.overflow = v.overflow.add(8);
            x
        }
    }
    /// long double: always in memory, 16-byte aligned
    pub unsafe fn ld(&mut self) -> f64 {
        let v = &mut *self.0;
        let p = ((v.overflow as usize + 15) & !15) as *const u8;
        let x = ld_to_f64(p);
        v.overflow = (p as *mut u8).add(16);
        x
    }
}

/// x87 80-bit extended to double (rounding toward zero in the last bits).
pub unsafe fn ld_to_f64(p: *const u8) -> f64 {
    let m = ptr::read_unaligned(p as *const u64);
    let se = ptr::read_unaligned(p.add(8) as *const u16);
    let sign = (se >> 15) as u64;
    let e = (se & 0x7fff) as i64;
    let bits = if e == 0x7fff {
        let frac = if m << 1 != 0 { 1u64 << 51 } else { 0 };
        (sign << 63) | (0x7ff << 52) | frac
    } else if m == 0 {
        sign << 63
    } else {
        // normalise
        let lz = m.leading_zeros() as i64;
        let m = m << lz;
        let exp = e - 16383 - lz;
        if exp > 1023 {
            (sign << 63) | (0x7ff << 52)
        } else if exp < -1074 {
            sign << 63
        } else if exp < -1022 {
            let shift = (-1022 - exp) as u32;
            (sign << 63) | ((m >> 11) >> shift)
        } else {
            (sign << 63) | (((exp + 1023) as u64) << 52) | ((m << 1) >> 12)
        }
    };
    f64::from_bits(bits)
}

pub unsafe fn f64_to_ld(x: f64, p: *mut u8) {
    let b = x.to_bits();
    let sign = ((b >> 63) as u16) << 15;
    let e = ((b >> 52) & 0x7ff) as i64;
    let f = b & ((1 << 52) - 1);
    let (m, ex): (u64, u16) = if e == 0x7ff {
        ((1 << 63) | (f << 11), 0x7fff)
    } else if e == 0 && f == 0 {
        (0, 0)
    } else if e == 0 {
        let lz = f.leading_zeros() as i64 - 11;
        ((f << (11 + lz)), (-1022 - lz + 16383) as u16)
    } else {
        ((1 << 63) | (f << 11), (e - 1023 + 16383) as u16)
    };
    ptr::write_unaligned(p as *mut u64, m);
    ptr::write_unaligned(p.add(8) as *mut u16, sign | ex);
    ptr::write_bytes(p.add(10), 0, 6);
}

// --- output sinks ------------------------------------------------------------------------------

pub trait Sink {
    fn put(&mut self, b: &[u8]);
    fn pad(&mut self, c: u8, n: usize) {
        let buf = [c; 64];
        let mut n = n;
        while n > 0 {
            let k = n.min(64);
            self.put(&buf[..k]);
            n -= k;
        }
    }
}

pub struct FileSink(pub *mut FILE, pub bool);
impl Sink for FileSink {
    fn put(&mut self, b: &[u8]) {
        // SAFETY: the stream is locked by the caller.
        if unsafe { stdio::put_bytes(self.0, b) } < b.len() {
            self.1 = true;
        }
    }
}

pub struct BufSink {
    pub buf: *mut u8,
    pub cap: usize, // bytes available including the NUL
    pub len: usize,
}
impl Sink for BufSink {
    fn put(&mut self, b: &[u8]) {
        if self.cap > 0 && self.len + 1 < self.cap {
            let k = b.len().min(self.cap - 1 - self.len);
            // SAFETY: k bytes fit before the reserved NUL.
            unsafe { ptr::copy_nonoverlapping(b.as_ptr(), self.buf.add(self.len), k) };
        }
        self.len += b.len();
    }
}

pub struct VecSink(pub Vec<u8>);
impl Sink for VecSink {
    fn put(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
}

struct Counter<'a, S: Sink + ?Sized> {
    sink: &'a mut S,
    n: usize,
}
impl<'a, S: Sink + ?Sized> Counter<'a, S> {
    fn put(&mut self, b: &[u8]) {
        self.sink.put(b);
        self.n += b.len();
    }
    fn pad(&mut self, c: u8, n: usize) {
        self.sink.pad(c, n);
        self.n += n;
    }
}

// --- the formatter -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Len {
    None,
    HH,
    H,
    L,
    LL,
    LD, // L
    J,
    Z,
    T,
}

#[derive(Clone, Copy)]
enum Arg {
    I(u64),
    D(f64),
}

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    Int,
    Dbl,
    LDbl,
}

struct Spec {
    minus: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    prec: Option<usize>,
    len: Len,
    conv: u8,
}

/// A small fixed buffer for core::fmt output.
struct FmtBuf {
    b: Vec<u8>,
}
impl Write for FmtBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.b.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

unsafe fn get_arg(va: &mut Va, pos: &Option<Vec<Arg>>, idx: &mut usize, ty: Ty) -> Arg {
    if let Some(p) = pos {
        let a = p.get(*idx).copied().unwrap_or(Arg::I(0));
        *idx += 1;
        return a;
    }
    match ty {
        Ty::Int => Arg::I(va.int()),
        Ty::Dbl => Arg::D(va.dbl()),
        Ty::LDbl => Arg::D(va.ld()),
    }
}

fn as_int(a: Arg) -> u64 {
    match a {
        Arg::I(x) => x,
        Arg::D(d) => d as i64 as u64,
    }
}
fn as_dbl(a: Arg) -> f64 {
    match a {
        Arg::D(d) => d,
        Arg::I(x) => f64::from_bits(x),
    }
}

fn conv_type(conv: u8, len: Len) -> Ty {
    match conv {
        b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
            if len == Len::LD {
                Ty::LDbl
            } else {
                Ty::Dbl
            }
        }
        _ => Ty::Int,
    }
}

/// Parse one conversion after '%' at `fmt[i]`. Returns (spec, next index, positional index,
/// width-from-arg position, precision-from-arg position).
fn parse_spec(fmt: &[u8], mut i: usize) -> (Spec, usize, Option<usize>, Option<Option<usize>>, Option<Option<usize>>) {
    let mut s = Spec { minus: false, plus: false, space: false, alt: false, zero: false, width: 0, prec: None, len: Len::None, conv: 0 };
    let num = |i: &mut usize| -> Option<usize> {
        let st = *i;
        let mut v = 0usize;
        while *i < fmt.len() && fmt[*i].is_ascii_digit() {
            v = v.saturating_mul(10).saturating_add((fmt[*i] - b'0') as usize);
            *i += 1;
        }
        if *i > st { Some(v) } else { None }
    };
    // n$
    let mut posn = None;
    {
        let mut j = i;
        if let Some(n) = num(&mut j) {
            if j < fmt.len() && fmt[j] == b'$' {
                posn = Some(n);
                i = j + 1;
            }
        }
    }
    while i < fmt.len() {
        match fmt[i] {
            b'-' => s.minus = true,
            b'+' => s.plus = true,
            b' ' => s.space = true,
            b'#' => s.alt = true,
            b'0' => s.zero = true,
            b'\'' | b'I' => {}
            _ => break,
        }
        i += 1;
    }
    let mut wstar = None;
    if i < fmt.len() && fmt[i] == b'*' {
        i += 1;
        let mut j = i;
        let p = num(&mut j).filter(|_| j < fmt.len() && fmt[j] == b'$');
        if p.is_some() {
            i = j + 1;
        }
        wstar = Some(p);
    } else if let Some(w) = num(&mut i) {
        s.width = w;
    }
    let mut pstar = None;
    if i < fmt.len() && fmt[i] == b'.' {
        i += 1;
        if i < fmt.len() && fmt[i] == b'*' {
            i += 1;
            let mut j = i;
            let p = num(&mut j).filter(|_| j < fmt.len() && fmt[j] == b'$');
            if p.is_some() {
                i = j + 1;
            }
            pstar = Some(p);
        } else {
            s.prec = Some(num(&mut i).unwrap_or(0));
        }
    }
    loop {
        if i >= fmt.len() {
            break;
        }
        match fmt[i] {
            b'h' => s.len = if s.len == Len::H { Len::HH } else { Len::H },
            b'l' => s.len = if s.len == Len::L { Len::LL } else { Len::L },
            b'L' | b'q' => s.len = if fmt[i] == b'q' { Len::LL } else { Len::LD },
            b'j' => s.len = Len::J,
            b'z' | b'Z' => s.len = Len::Z,
            b't' => s.len = Len::T,
            _ => break,
        }
        i += 1;
    }
    if i < fmt.len() {
        s.conv = fmt[i];
        i += 1;
    }
    (s, i, posn, wstar, pstar)
}

/// First pass for positional arguments: the type of each argument, in order.
unsafe fn read_positional(fmt: &[u8], va: &mut Va) -> Vec<Arg> {
    let mut types: Vec<Option<Ty>> = Vec::new();
    let mut set = |types: &mut Vec<Option<Ty>>, n: usize, t: Ty| {
        if n == 0 {
            return;
        }
        if types.len() < n {
            types.resize(n, None);
        }
        types[n - 1] = Some(t);
    };
    let mut i = 0;
    let mut next = 1;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            i += 1;
            continue;
        }
        if i + 1 < fmt.len() && fmt[i + 1] == b'%' {
            i += 2;
            continue;
        }
        let (s, ni, posn, ws, ps) = parse_spec(fmt, i + 1);
        i = ni;
        if let Some(w) = ws {
            let n = w.unwrap_or_else(|| { next += 1; next - 1 });
            set(&mut types, n, Ty::Int);
        }
        if let Some(p) = ps {
            let n = p.unwrap_or_else(|| { next += 1; next - 1 });
            set(&mut types, n, Ty::Int);
        }
        if s.conv == b'%' || s.conv == b'm' || s.conv == 0 {
            continue;
        }
        let n = posn.unwrap_or_else(|| { next += 1; next - 1 });
        set(&mut types, n, conv_type(s.conv, s.len));
    }
    types
        .iter()
        .map(|t| match t.unwrap_or(Ty::Int) {
            Ty::Int => Arg::I(va.int()),
            Ty::Dbl => Arg::D(va.dbl()),
            Ty::LDbl => Arg::D(va.ld()),
        })
        .collect()
}

fn has_positional(fmt: &[u8]) -> bool {
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] == b'%' {
            let mut j = i + 1;
            while j < fmt.len() && fmt[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < fmt.len() && fmt[j] == b'$' {
                return true;
            }
            if j < fmt.len() && fmt[j] == b'%' {
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    false
}

fn emit_padded<S: Sink + ?Sized>(o: &mut Counter<S>, s: &Spec, prefix: &[u8], body: &[u8], zero_ok: bool) {
    let len = prefix.len() + body.len();
    let pad = s.width.saturating_sub(len);
    if s.minus {
        o.put(prefix);
        o.put(body);
        o.pad(b' ', pad);
    } else if s.zero && zero_ok {
        o.put(prefix);
        o.pad(b'0', pad);
        o.put(body);
    } else {
        o.pad(b' ', pad);
        o.put(prefix);
        o.put(body);
    }
}

fn fmt_int<S: Sink + ?Sized>(o: &mut Counter<S>, s: &Spec, v: u64) {
    let signed = matches!(s.conv, b'd' | b'i');
    let (neg, mag) = if signed {
        let x = match s.len {
            Len::HH => v as i8 as i64,
            Len::H => v as i16 as i64,
            Len::None => v as i32 as i64,
            _ => v as i64,
        };
        (x < 0, x.unsigned_abs())
    } else {
        let x = match s.len {
            Len::HH => v as u8 as u64,
            Len::H => v as u16 as u64,
            Len::None => v as u32 as u64,
            _ => v,
        };
        (false, x)
    };
    let (base, upper) = match s.conv {
        b'o' => (8, false),
        b'x' => (16, false),
        b'X' => (16, true),
        b'b' | b'B' => (2, false),
        _ => (10, false),
    };
    let mut digits = [0u8; 70];
    let mut n = 0;
    let mut m = mag;
    while m > 0 {
        let d = (m % base) as u8;
        digits[n] = if d < 10 { b'0' + d } else if upper { b'A' + d - 10 } else { b'a' + d - 10 };
        n += 1;
        m /= base;
    }
    let mut body: Vec<u8> = Vec::with_capacity(n + 8);
    let prec = s.prec.unwrap_or(1);
    for _ in n..prec {
        body.push(b'0');
    }
    for i in (0..n).rev() {
        body.push(digits[i]);
    }
    if s.conv == b'o' && s.alt && body.first() != Some(&b'0') {
        body.insert(0, b'0');
    }
    let mut prefix: Vec<u8> = Vec::new();
    if neg {
        prefix.push(b'-');
    } else if signed && s.plus {
        prefix.push(b'+');
    } else if signed && s.space {
        prefix.push(b' ');
    }
    if s.alt && mag != 0 {
        match s.conv {
            b'x' => prefix.extend_from_slice(b"0x"),
            b'X' => prefix.extend_from_slice(b"0X"),
            b'b' => prefix.extend_from_slice(b"0b"),
            b'B' => prefix.extend_from_slice(b"0B"),
            _ => {}
        }
    }
    emit_padded(o, s, &prefix, &body, s.prec.is_none());
}

/// "%e"-style exponent: sign and at least two digits.
fn push_exp(out: &mut Vec<u8>, e: i32, ch: u8) {
    out.push(ch);
    out.push(if e < 0 { b'-' } else { b'+' });
    let a = e.unsigned_abs();
    if a < 10 {
        out.push(b'0');
    }
    let mut t = FmtBuf { b: Vec::new() };
    let _ = write!(t, "{a}");
    out.extend_from_slice(&t.b);
}

/// Format |x| (finite) in %e style with `prec` digits after the point: (digits, exponent).
fn exp_form(x: f64, prec: usize) -> (Vec<u8>, i32) {
    let mut t = FmtBuf { b: Vec::new() };
    let _ = write!(t, "{:.*e}", prec, x);
    let s = t.b;
    let epos = s.iter().position(|&c| c == b'e').unwrap_or(s.len());
    let e: i32 = core::str::from_utf8(&s[epos + 1..]).ok().and_then(|x| x.parse().ok()).unwrap_or(0);
    (s[..epos].to_vec(), e)
}

fn fixed_form(x: f64, prec: usize) -> Vec<u8> {
    let mut t = FmtBuf { b: Vec::new() };
    let _ = write!(t, "{:.*}", prec, x);
    t.b
}

fn hex_form(x: f64, prec: Option<usize>, upper: bool) -> Vec<u8> {
    let bits = x.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i32;
    let mut frac = bits & ((1u64 << 52) - 1);
    let (lead, exp) = if e == 0 {
        if frac == 0 { (0u64, 0) } else { (0u64, -1022) }
    } else {
        (1u64, e - 1023)
    };
    let mut lead = lead;
    // 13 hex digits of fraction
    let mut ndig = 13usize;
    if let Some(p) = prec {
        if p < 13 {
            let drop = (13 - p) * 4;
            let rem = frac & ((1u64 << drop) - 1);
            let half = 1u64 << (drop - 1);
            frac >>= drop;
            // round half to even
            if rem > half || (rem == half && frac & 1 == 1) {
                frac += 1;
                if frac >> (p * 4) != 0 {
                    frac &= (1u64 << (p * 4)) - 1;
                    lead += 1;
                }
            }
            ndig = p;
        }
    } else {
        while ndig > 0 && frac & 0xf == 0 {
            frac >>= 4;
            ndig -= 1;
        }
    }
    let hex = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    let mut out = Vec::new();
    out.extend_from_slice(if upper { b"0X" } else { b"0x" });
    out.push(hex[lead as usize & 0xf]);
    let total = prec.map_or(ndig, |p| p.max(ndig));
    if total > 0 {
        out.push(b'.');
        for i in (0..ndig).rev() {
            out.push(hex[((frac >> (i * 4)) & 0xf) as usize]);
        }
        for _ in ndig..total {
            out.push(b'0');
        }
    }
    out.push(if upper { b'P' } else { b'p' });
    out.push(if exp < 0 { b'-' } else { b'+' });
    let mut t = FmtBuf { b: Vec::new() };
    let _ = write!(t, "{}", exp.unsigned_abs());
    out.extend_from_slice(&t.b);
    out
}

fn fmt_float<S: Sink + ?Sized>(o: &mut Counter<S>, s: &Spec, x: f64) {
    let upper = s.conv.is_ascii_uppercase();
    let neg = x.is_sign_negative();
    let a = x.abs();
    let mut prefix: Vec<u8> = Vec::new();
    if neg {
        prefix.push(b'-');
    } else if s.plus {
        prefix.push(b'+');
    } else if s.space {
        prefix.push(b' ');
    }
    if !a.is_finite() {
        let body: &[u8] = match (a.is_nan(), upper) {
            (true, false) => b"nan",
            (true, true) => b"NAN",
            (false, false) => b"inf",
            (false, true) => b"INF",
        };
        emit_padded(o, s, &prefix, body, false);
        return;
    }
    let body: Vec<u8> = match s.conv | 0x20 {
        b'f' => {
            let mut b = fixed_form(a, s.prec.unwrap_or(6));
            if s.alt && s.prec == Some(0) {
                b.push(b'.');
            }
            b
        }
        b'e' => {
            let p = s.prec.unwrap_or(6);
            let (mut m, e) = exp_form(a, p);
            if s.alt && p == 0 {
                m.push(b'.');
            }
            push_exp(&mut m, e, if upper { b'E' } else { b'e' });
            m
        }
        b'g' => {
            let p = match s.prec {
                None => 6,
                Some(0) => 1,
                Some(p) => p,
            };
            let (_, x_exp) = if a == 0.0 { (Vec::new(), 0) } else { exp_form(a, p - 1) };
            let mut b = if (x_exp as i64) < p as i64 && x_exp >= -4 {
                fixed_form(a, (p as i64 - 1 - x_exp as i64) as usize)
            } else {
                let (mut m, e) = exp_form(a, p - 1);
                if !s.alt {
                    strip_zeros(&mut m);
                }
                push_exp(&mut m, e, if upper { b'E' } else { b'e' });
                m
            };
            if !s.alt && !b.contains(&b'e') && !b.contains(&b'E') {
                strip_zeros(&mut b);
            } else if s.alt && !b.contains(&b'.') {
                // '#' keeps the decimal point
                if let Some(ep) = b.iter().position(|&c| c == b'e' || c == b'E') {
                    b.insert(ep, b'.');
                } else {
                    b.push(b'.');
                }
            }
            b
        }
        _ => {
            let mut h = hex_form(a, s.prec, upper);
            if s.alt && !h.contains(&b'.') {
                let pp = h.iter().position(|&c| c == b'p' || c == b'P').unwrap_or(h.len());
                h.insert(pp, b'.');
            }
            // the 0x goes into the prefix so zero padding comes after it
            prefix.extend_from_slice(&h[..2]);
            h[2..].to_vec()
        }
    };
    let body = if upper { body.iter().map(|c| c.to_ascii_uppercase()).collect() } else { body };
    emit_padded(o, s, &prefix, &body, true);
}

fn strip_zeros(b: &mut Vec<u8>) {
    if b.contains(&b'.') {
        while b.last() == Some(&b'0') {
            b.pop();
        }
        if b.last() == Some(&b'.') {
            b.pop();
        }
    }
}

/// The formatter. Returns the number of bytes produced.
pub unsafe fn format<S: Sink + ?Sized>(sink: &mut S, fmt: *const c_char, va: &mut Va) -> usize {
    let fmt = fl::mem::cstr(fmt);
    let mut o = Counter { sink, n: 0 };
    let pos = if has_positional(fmt) { Some(read_positional(fmt, va)) } else { None };
    let mut seq = 0usize; // next argument when not positional (into `pos` when positional)
    let mut i = 0;
    while i < fmt.len() {
        // literal run
        let st = i;
        while i < fmt.len() && fmt[i] != b'%' {
            i += 1;
        }
        if i > st {
            o.put(&fmt[st..i]);
        }
        if i >= fmt.len() {
            break;
        }
        let (mut s, ni, posn, ws, ps) = parse_spec(fmt, i + 1);
        i = ni;
        let mut argidx = |p: Option<usize>, seq: &mut usize| -> usize {
            match p {
                Some(n) => n.saturating_sub(1),
                None => {
                    *seq += 1;
                    *seq - 1
                }
            }
        };
        if let Some(w) = ws {
            let mut ix = argidx(w, &mut seq);
            let v = as_int(get_arg(va, &pos, &mut ix, Ty::Int)) as i32;
            if v < 0 {
                s.minus = true;
                s.width = v.unsigned_abs() as usize;
            } else {
                s.width = v as usize;
            }
        }
        if let Some(p) = ps {
            let mut ix = argidx(p, &mut seq);
            let v = as_int(get_arg(va, &pos, &mut ix, Ty::Int)) as i32;
            s.prec = if v < 0 { None } else { Some(v as usize) };
        }
        let conv = s.conv;
        let mut take = |ty: Ty, seq: &mut usize| -> Arg {
            let mut ix = match posn {
                Some(n) => n.saturating_sub(1),
                None => {
                    *seq += 1;
                    *seq - 1
                }
            };
            get_arg(va, &pos, &mut ix, ty)
        };
        match conv {
            b'%' => o.put(b"%"),
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'b' | b'B' => {
                let v = as_int(take(Ty::Int, &mut seq));
                fmt_int(&mut o, &s, v);
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                let ty = conv_type(conv, s.len);
                let v = as_dbl(take(ty, &mut seq));
                fmt_float(&mut o, &s, v);
            }
            b'c' | b'C' => {
                let v = as_int(take(Ty::Int, &mut seq));
                let mut b = [0u8; 4];
                let n = if s.len == Len::L || conv == b'C' {
                    super::locale::utf8_encode(v as u32, &mut b)
                } else {
                    b[0] = v as u8;
                    1
                };
                emit_padded(&mut o, &s, b"", &b[..n], false);
            }
            b's' | b'S' => {
                let p = as_int(take(Ty::Int, &mut seq)) as usize;
                if s.len == Len::L || conv == b'S' {
                    let mut out: Vec<u8> = Vec::new();
                    if p == 0 {
                        out.extend_from_slice(b"(null)");
                    } else {
                        let mut w = p as *const u32;
                        while *w != 0 {
                            let mut b = [0u8; 4];
                            let n = super::locale::utf8_encode(*w, &mut b);
                            if let Some(pr) = s.prec {
                                if out.len() + n > pr {
                                    break;
                                }
                            }
                            out.extend_from_slice(&b[..n]);
                            w = w.add(1);
                        }
                    }
                    emit_padded(&mut o, &s, b"", &out, false);
                } else {
                    let st: &[u8] = if p == 0 {
                        if s.prec.map_or(true, |pr| pr >= 6) { b"(null)" } else { b"" }
                    } else {
                        let max = s.prec.unwrap_or(usize::MAX);
                        let mut n = 0;
                        while n < max && *((p + n) as *const u8) != 0 {
                            n += 1;
                        }
                        core::slice::from_raw_parts(p as *const u8, n)
                    };
                    emit_padded(&mut o, &s, b"", st, false);
                }
            }
            b'p' => {
                let v = as_int(take(Ty::Int, &mut seq));
                if v == 0 {
                    emit_padded(&mut o, &s, b"", b"(nil)", false);
                } else {
                    let mut s2 = Spec { alt: true, conv: b'x', len: Len::LL, ..s };
                    s2.prec = s.prec;
                    fmt_int(&mut o, &s2, v);
                }
            }
            b'n' => {
                let p = as_int(take(Ty::Int, &mut seq)) as usize;
                if p != 0 {
                    match s.len {
                        Len::HH => *(p as *mut i8) = o.n as i8,
                        Len::H => *(p as *mut i16) = o.n as i16,
                        Len::L | Len::LL | Len::J | Len::Z | Len::T => *(p as *mut i64) = o.n as i64,
                        _ => *(p as *mut i32) = o.n as i32,
                    }
                }
            }
            b'm' => {
                let e = fl::errno::get();
                let m = fl::mem::cstr(fl::strerror::strerror(e) as *const u8);
                emit_padded(&mut o, &s, b"", m, false);
            }
            0 => break,
            other => {
                // unknown conversion: print it as written
                o.put(b"%");
                o.put(&[other]);
            }
        }
    }
    o.n
}

// --- the functions --------------------------------------------------------------------------

fn ret(n: usize) -> c_int {
    if n > i32::MAX as usize {
        fl::errno::set(75 /* EOVERFLOW */);
        -1
    } else {
        n as c_int
    }
}

pub unsafe fn vfprintf_va(f: *mut FILE, fmt: *const c_char, va: &mut Va) -> c_int {
    let _l = stdio::locked(f);
    let mut s = FileSink(f, false);
    let n = format(&mut s, fmt, va);
    if s.1 || (*f).flags & stdio::F_ERR != 0 && s.1 {
        return -1;
    }
    ret(n)
}

pub unsafe fn vsnprintf_va(buf: *mut c_char, cap: size_t, fmt: *const c_char, va: &mut Va) -> c_int {
    let mut s = BufSink { buf, cap, len: 0 };
    let n = format(&mut s, fmt, va);
    if cap > 0 {
        *buf.add(n.min(cap - 1)) = 0;
    }
    ret(n)
}

pub unsafe fn vasprintf_va(out: *mut *mut c_char, fmt: *const c_char, va: &mut Va) -> c_int {
    let mut s = VecSink(Vec::new());
    let n = format(&mut s, fmt, va);
    let p = umem::malloc(n + 1) as *mut u8;
    if p.is_null() {
        *out = ptr::null_mut();
        return -1;
    }
    ptr::copy_nonoverlapping(s.0.as_ptr(), p, n);
    *p.add(n) = 0;
    *out = p;
    ret(n)
}

pub unsafe fn vdprintf_va(fd: c_int, fmt: *const c_char, va: &mut Va) -> c_int {
    let mut s = VecSink(Vec::new());
    let n = format(&mut s, fmt, va);
    let mut done = 0;
    while done < n {
        let r = fl::sys::sc3(fl::sys::WRITE, fd as usize, s.0.as_ptr() as usize + done, n - done);
        if r == -4 {
            continue;
        }
        if r < 0 {
            fl::errno::set(-r as i32);
            return -1;
        }
        done += r as usize;
    }
    ret(n)
}

macro_rules! va_of {
    ($args:ident) => {
        Va(&mut $args as *mut _ as *mut RawVa)
    };
}

#[no_mangle]
pub unsafe extern "C" fn printf(fmt: *const c_char, mut args: ...) -> c_int {
    vfprintf_va(stdio::stdout_file(), fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn fprintf(f: *mut FILE, fmt: *const c_char, mut args: ...) -> c_int {
    vfprintf_va(f, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn sprintf(buf: *mut c_char, fmt: *const c_char, mut args: ...) -> c_int {
    vsnprintf_va(buf, usize::MAX / 2, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn snprintf(buf: *mut c_char, n: size_t, fmt: *const c_char, mut args: ...) -> c_int {
    vsnprintf_va(buf, n, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn asprintf(out: *mut *mut c_char, fmt: *const c_char, mut args: ...) -> c_int {
    vasprintf_va(out, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn dprintf(fd: c_int, fmt: *const c_char, mut args: ...) -> c_int {
    vdprintf_va(fd, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn vprintf(fmt: *const c_char, mut ap: VaList) -> c_int {
    vfprintf_va(stdio::stdout_file(), fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn vfprintf(f: *mut FILE, fmt: *const c_char, mut ap: VaList) -> c_int {
    vfprintf_va(f, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn vsprintf(buf: *mut c_char, fmt: *const c_char, mut ap: VaList) -> c_int {
    vsnprintf_va(buf, usize::MAX / 2, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn vsnprintf(buf: *mut c_char, n: size_t, fmt: *const c_char, mut ap: VaList) -> c_int {
    vsnprintf_va(buf, n, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn vasprintf(out: *mut *mut c_char, fmt: *const c_char, mut ap: VaList) -> c_int {
    vasprintf_va(out, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn vdprintf(fd: c_int, fmt: *const c_char, mut ap: VaList) -> c_int {
    vdprintf_va(fd, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}

// _FORTIFY_SOURCE versions: the flag asks for %n checks; the sizes are the real buffer's.
#[no_mangle]
pub unsafe extern "C" fn __printf_chk(_flag: c_int, fmt: *const c_char, mut args: ...) -> c_int {
    vfprintf_va(stdio::stdout_file(), fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn __fprintf_chk(f: *mut FILE, _flag: c_int, fmt: *const c_char, mut args: ...) -> c_int {
    vfprintf_va(f, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn __sprintf_chk(buf: *mut c_char, _flag: c_int, slen: size_t, fmt: *const c_char, mut args: ...) -> c_int {
    let n = vsnprintf_va(buf, slen, fmt, &mut va_of!(args));
    if n >= 0 && n as usize >= slen {
        super::start::__chk_fail();
    }
    n
}
#[no_mangle]
pub unsafe extern "C" fn __snprintf_chk(buf: *mut c_char, n: size_t, _flag: c_int, slen: size_t, fmt: *const c_char, mut args: ...) -> c_int {
    if n > slen {
        super::start::__chk_fail();
    }
    vsnprintf_va(buf, n, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn __asprintf_chk(out: *mut *mut c_char, _flag: c_int, fmt: *const c_char, mut args: ...) -> c_int {
    vasprintf_va(out, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn __dprintf_chk(fd: c_int, _flag: c_int, fmt: *const c_char, mut args: ...) -> c_int {
    vdprintf_va(fd, fmt, &mut va_of!(args))
}
#[no_mangle]
pub unsafe extern "C" fn __vprintf_chk(_flag: c_int, fmt: *const c_char, mut ap: VaList) -> c_int {
    vfprintf_va(stdio::stdout_file(), fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn __vfprintf_chk(f: *mut FILE, _flag: c_int, fmt: *const c_char, mut ap: VaList) -> c_int {
    vfprintf_va(f, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn __vsprintf_chk(buf: *mut c_char, _flag: c_int, slen: size_t, fmt: *const c_char, mut ap: VaList) -> c_int {
    let n = vsnprintf_va(buf, slen, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa));
    if n >= 0 && n as usize >= slen {
        super::start::__chk_fail();
    }
    n
}
#[no_mangle]
pub unsafe extern "C" fn __vsnprintf_chk(buf: *mut c_char, n: size_t, _flag: c_int, slen: size_t, fmt: *const c_char, mut ap: VaList) -> c_int {
    if n > slen {
        super::start::__chk_fail();
    }
    vsnprintf_va(buf, n, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn __vasprintf_chk(out: *mut *mut c_char, _flag: c_int, fmt: *const c_char, mut ap: VaList) -> c_int {
    vasprintf_va(out, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}
#[no_mangle]
pub unsafe extern "C" fn __vdprintf_chk(fd: c_int, _flag: c_int, fmt: *const c_char, mut ap: VaList) -> c_int {
    vdprintf_va(fd, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa))
}

/// glibc's parse_printf_format: the types of a format's arguments (PA_INT and friends).
#[no_mangle]
pub unsafe extern "C" fn parse_printf_format(fmt: *const c_char, n: size_t, types: *mut c_int) -> size_t {
    const PA_INT: c_int = 0;
    const PA_CHAR: c_int = 1;
    const PA_STRING: c_int = 3;
    const PA_POINTER: c_int = 5;
    const PA_DOUBLE: c_int = 7;
    const PA_FLAG_LONG: c_int = 1 << 9;
    const PA_FLAG_LONG_LONG: c_int = 1 << 8;
    const PA_FLAG_LONG_DOUBLE: c_int = PA_FLAG_LONG_LONG;
    let f = fl::mem::cstr(fmt);
    let mut count = 0usize;
    let mut i = 0;
    let mut push = |t: c_int, count: &mut usize| {
        if *count < n {
            *types.add(*count) = t;
        }
        *count += 1;
    };
    while i < f.len() {
        if f[i] != b'%' {
            i += 1;
            continue;
        }
        if i + 1 < f.len() && f[i + 1] == b'%' {
            i += 2;
            continue;
        }
        let (s, ni, _, ws, ps) = parse_spec(f, i + 1);
        i = ni;
        if ws.is_some() {
            push(PA_INT, &mut count);
        }
        if ps.is_some() {
            push(PA_INT, &mut count);
        }
        let t = match s.conv {
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' => match s.len {
                Len::L | Len::Z | Len::T | Len::J => PA_INT | PA_FLAG_LONG,
                Len::LL => PA_INT | PA_FLAG_LONG_LONG,
                _ => PA_INT,
            },
            b'c' => PA_CHAR,
            b's' => PA_STRING,
            b'p' | b'n' => PA_POINTER,
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                if s.len == Len::LD { PA_DOUBLE | PA_FLAG_LONG_DOUBLE } else { PA_DOUBLE }
            }
            _ => continue,
        };
        push(t, &mut count);
    }
    count
}

#[allow(dead_code)]
fn _unused(_: *mut c_void) {}
