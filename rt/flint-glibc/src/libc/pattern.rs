//! Shell patterns: fnmatch and glob (with glibc's flags: PATHNAME, PERIOD, LEADING_DIR,
//! CASEFOLD; MARK, NOSORT, DOOFFS, NOCHECK, APPEND, NOESCAPE, BRACE, NOMAGIC, TILDE,
//! ONLYDIR, ALTDIRFUNC). Characters are decoded as UTF-8 in a UTF-8 locale.

use super::locale;
use super::umem;
use crate::fl;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;

// --- characters ---------------------------------------------------------------------------------

/// The character at s[i] and its length: a code point in a UTF-8 locale, else the byte.
pub fn decode(s: &[u8], i: usize) -> (u32, usize) {
    let b = s[i];
    if b < 0x80 || !locale::ctype_utf8() {
        return (b as u32, 1);
    }
    let (len, init) = match b {
        0xc2..=0xdf => (2, (b & 0x1f) as u32),
        0xe0..=0xef => (3, (b & 0x0f) as u32),
        0xf0..=0xf4 => (4, (b & 0x07) as u32),
        _ => return (0xdc00 | b as u32, 1),
    };
    if i + len > s.len() {
        return (0xdc00 | b as u32, 1);
    }
    let mut c = init;
    for k in 1..len {
        let x = s[i + k];
        if x & 0xc0 != 0x80 {
            return (0xdc00 | b as u32, 1);
        }
        c = (c << 6) | (x & 0x3f) as u32;
    }
    (c, len)
}

pub fn fold(c: u32) -> u32 {
    if c >= 0x80 && !locale::ctype_utf8() {
        return c;
    }
    locale::towlower(c as _) as u32
}

/// Is `c` in the class named `name` ("alpha", "digit"...)? None for an unknown class.
pub fn in_class(name: &[u8], c: u32) -> Option<bool> {
    let mut n = name.to_vec();
    n.push(0);
    // SAFETY: n is NUL-terminated.
    let t = unsafe { locale::wctype(n.as_ptr()) };
    if t == 0 {
        return None;
    }
    // bytes above ASCII belong to no class in the C locale
    if c >= 0x80 && !locale::ctype_utf8() {
        return Some(false);
    }
    Some(locale::iswctype(c as _, t) != 0)
}

// --- fnmatch -----------------------------------------------------------------------------------

const FNM_PATHNAME: c_int = 1;
const FNM_NOESCAPE: c_int = 2;
const FNM_PERIOD: c_int = 4;
const FNM_LEADING_DIR: c_int = 8;
const FNM_CASEFOLD: c_int = 16;

/// Match a bracket expression starting at p[i] == '['. Returns (matched, index after ']'),
/// or None when the expression is not closed (then '[' is an ordinary character).
fn bracket(p: &[u8], i: usize, c: u32, flags: c_int) -> Option<(bool, usize)> {
    let mut j = i + 1;
    let negate = j < p.len() && (p[j] == b'!' || p[j] == b'^');
    if negate {
        j += 1;
    }
    let casefold = flags & FNM_CASEFOLD != 0;
    let cf = if casefold { fold(c) } else { c };
    let mut matched = false;
    let mut first = true;
    loop {
        if j >= p.len() {
            return None;
        }
        if p[j] == b']' && !first {
            j += 1;
            break;
        }
        first = false;
        // [:class:], [=c=], [.c.]
        if p[j] == b'[' && j + 1 < p.len() && matches!(p[j + 1], b':' | b'=' | b'.') {
            let kind = p[j + 1];
            let st = j + 2;
            let mut e = st;
            while e + 1 < p.len() && !(p[e] == kind && p[e + 1] == b']') {
                e += 1;
            }
            if e + 1 >= p.len() {
                return None;
            }
            let body = &p[st..e];
            j = e + 2;
            match kind {
                b':' => match in_class(body, c) {
                    Some(true) => matched = true,
                    Some(false) => {
                        // case-insensitive upper/lower match either
                        if casefold && (body == b"upper" || body == b"lower") && in_class(b"alpha", c) == Some(true) {
                            matched = true;
                        }
                    }
                    None => return Some((false, j)),
                },
                _ => {
                    if !body.is_empty() {
                        let (x, _) = decode(body, 0);
                        if x == c || (casefold && fold(x) == cf) {
                            matched = true;
                        }
                    }
                }
            }
            continue;
        }
        let mut lo_i = j;
        if p[j] == b'\\' && flags & FNM_NOESCAPE == 0 && j + 1 < p.len() {
            lo_i = j + 1;
        }
        let (lo, ll) = decode(p, lo_i);
        j = lo_i + ll;
        // a range?
        if j + 1 < p.len() && p[j] == b'-' && p[j + 1] != b']' {
            let mut hi_i = j + 1;
            if p[hi_i] == b'\\' && flags & FNM_NOESCAPE == 0 && hi_i + 1 < p.len() {
                hi_i += 1;
            }
            let (hi, hl) = decode(p, hi_i);
            j = hi_i + hl;
            if (lo <= c && c <= hi) || (casefold && ((lo <= cf && cf <= hi) || (fold(lo) <= cf && cf <= fold(hi)))) {
                matched = true;
            }
        } else if lo == c || (casefold && fold(lo) == cf) {
            matched = true;
        }
    }
    Some((matched != negate, j))
}

pub fn fnmatch_bytes(p: &[u8], s: &[u8], flags: c_int) -> bool {
    let pathname = flags & FNM_PATHNAME != 0;
    let casefold = flags & FNM_CASEFOLD != 0;
    let period_at = |si: usize| -> bool {
        flags & FNM_PERIOD != 0 && si < s.len() && s[si] == b'.' && (si == 0 || (pathname && s[si - 1] == b'/'))
    };
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        let mut ok = false;
        if pi < p.len() {
            match p[pi] {
                b'*' => {
                    while pi < p.len() && p[pi] == b'*' {
                        pi += 1;
                    }
                    if !period_at(si) {
                        star = Some((pi, si));
                        continue;
                    }
                }
                b'?' => {
                    if si < s.len() && !(pathname && s[si] == b'/') && !period_at(si) {
                        let (_, l) = decode(s, si);
                        si += l;
                        pi += 1;
                        ok = true;
                    }
                }
                b'[' if si < s.len() => {
                    let (c, l) = decode(s, si);
                    match bracket(p, pi, c, flags) {
                        Some((m, next)) => {
                            if m && !(pathname && c == b'/' as u32) && !period_at(si) {
                                si += l;
                                pi = next;
                                ok = true;
                            }
                        }
                        // an unterminated bracket matches nothing (as in glibc)
                        None => return false,
                    }
                }
                _ => {
                    let mut q = pi;
                    if p[q] == b'\\' && flags & FNM_NOESCAPE == 0 && q + 1 < p.len() {
                        q += 1;
                    }
                    if si < s.len() {
                        let (pc, pl) = decode(p, q);
                        let (sc, sl) = decode(s, si);
                        if pc == sc || (casefold && fold(pc) == fold(sc)) {
                            pi = q + pl;
                            si += sl;
                            ok = true;
                        }
                    }
                }
            }
        } else if si == s.len() || (flags & FNM_LEADING_DIR != 0 && s[si] == b'/') {
            return true;
        }
        if ok {
            continue;
        }
        // backtrack: let the last star take one more character
        match star {
            Some((sp, ss)) if ss < s.len() && !(pathname && s[ss] == b'/') => {
                let (_, l) = decode(s, ss);
                star = Some((sp, ss + l));
                pi = sp;
                si = ss + l;
            }
            _ => {
                // "dir*" with LEADING_DIR: the star may stop at a slash
                return false;
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn fnmatch(pattern: *const c_char, s: *const c_char, flags: c_int) -> c_int {
    if fnmatch_bytes(fl::mem::cstr(pattern), fl::mem::cstr(s), flags) { 0 } else { 1 }
}

// --- glob ---------------------------------------------------------------------------------------

const GLOB_ERR: c_int = 1;
const GLOB_MARK: c_int = 2;
const GLOB_NOSORT: c_int = 4;
const GLOB_DOOFFS: c_int = 8;
const GLOB_NOCHECK: c_int = 16;
const GLOB_APPEND: c_int = 32;
const GLOB_NOESCAPE: c_int = 64;
const GLOB_PERIOD: c_int = 128;
const GLOB_ALTDIRFUNC: c_int = 512;
const GLOB_BRACE: c_int = 1024;
const GLOB_NOMAGIC: c_int = 2048;
const GLOB_TILDE: c_int = 4096;
const GLOB_ONLYDIR: c_int = 8192;
const GLOB_TILDE_CHECK: c_int = 16384;

const GLOB_NOSPACE: c_int = 1;
const GLOB_ABORTED: c_int = 2;
const GLOB_NOMATCH: c_int = 3;

#[repr(C)]
pub struct Glob {
    pathc: usize,
    pathv: *mut *mut c_char,
    offs: usize,
    flags: c_int,
    closedir: Option<unsafe extern "C" fn(*mut c_void)>,
    readdir: Option<unsafe extern "C" fn(*mut c_void) -> *mut u8>,
    opendir: Option<unsafe extern "C" fn(*const c_char) -> *mut c_void>,
    lstat: Option<unsafe extern "C" fn(*const c_char, *mut c_void) -> c_int>,
    stat: Option<unsafe extern "C" fn(*const c_char, *mut c_void) -> c_int>,
}

type ErrFn = Option<unsafe extern "C" fn(*const c_char, c_int) -> c_int>;

fn has_magic(s: &[u8], noescape: bool) -> bool {
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'\\' if !noescape => i += 1,
            b'*' | b'?' | b'[' => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn unescape(s: &[u8], noescape: bool) -> Vec<u8> {
    if noescape {
        return s.to_vec();
    }
    let mut v = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' && i + 1 < s.len() {
            i += 1;
        }
        v.push(s[i]);
        i += 1;
    }
    v
}

/// {a,b}c -> ac, bc (nested braces too). A pattern without a well-formed brace is itself.
fn brace_expand(p: &[u8], out: &mut Vec<Vec<u8>>) {
    let mut depth = 0;
    let mut open = None;
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            b'\\' => i += 1,
            b'{' => {
                if depth == 0 {
                    open = Some(i);
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let o = open.unwrap();
                    // split the body at top-level commas
                    let body = &p[o + 1..i];
                    let mut parts = Vec::new();
                    let (mut d, mut st, mut k) = (0, 0, 0);
                    while k < body.len() {
                        match body[k] {
                            b'\\' => k += 1,
                            b'{' => d += 1,
                            b'}' => d -= 1,
                            b',' if d == 0 => {
                                parts.push(&body[st..k]);
                                st = k + 1;
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    parts.push(&body[st..]);
                    if parts.len() == 1 {
                        // "{x}" is not an alternative: keep the braces, expand what follows
                        let mut rest = Vec::new();
                        brace_expand(&p[i + 1..], &mut rest);
                        for r in rest {
                            let mut v = p[..i + 1].to_vec();
                            v.extend_from_slice(&r);
                            out.push(v);
                        }
                        return;
                    }
                    for part in parts {
                        let mut v = p[..o].to_vec();
                        v.extend_from_slice(part);
                        v.extend_from_slice(&p[i + 1..]);
                        brace_expand(&v, out);
                    }
                    return;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.push(p.to_vec());
}

struct Ctx<'a> {
    flags: c_int,
    errfn: ErrFn,
    g: &'a Glob,
    aborted: bool,
}

impl Ctx<'_> {
    unsafe fn list_dir(&mut self, dir: &[u8]) -> Option<Vec<(Vec<u8>, u8)>> {
        let mut path = if dir.is_empty() { b".".to_vec() } else { dir.to_vec() };
        path.push(0);
        let alt = self.flags & GLOB_ALTDIRFUNC != 0;
        let d = if alt {
            match self.g.opendir {
                Some(f) => f(path.as_ptr()),
                None => ptr::null_mut(),
            }
        } else {
            fl::dir::opendir(path.as_ptr() as *const _) as *mut c_void
        };
        if d.is_null() {
            let e = fl::errno::get();
            if e != 20 /* ENOTDIR */ && e != 2 /* ENOENT */ || self.flags & GLOB_ERR != 0 {
                let stop = match self.errfn {
                    Some(f) => f(path.as_ptr(), e) != 0,
                    None => false,
                };
                if stop || self.flags & GLOB_ERR != 0 {
                    self.aborted = true;
                }
            }
            return None;
        }
        let mut v = Vec::new();
        loop {
            let e = if alt {
                match self.g.readdir {
                    Some(f) => f(d),
                    None => ptr::null_mut(),
                }
            } else {
                fl::dir::readdir(d as *mut _) as *mut u8
            };
            if e.is_null() {
                break;
            }
            // struct dirent64: d_type at 18, d_name at 19
            v.push((fl::mem::cstr(e.add(19)).to_vec(), *e.add(18)));
        }
        if alt {
            if let Some(f) = self.g.closedir {
                f(d);
            }
        } else {
            fl::dir::closedir(d as *mut _);
        }
        Some(v)
    }

    unsafe fn exists(&self, path: &[u8]) -> bool {
        let mut p = path.to_vec();
        p.push(0);
        let mut st = [0u8; 256];
        if self.flags & GLOB_ALTDIRFUNC != 0 {
            if let Some(f) = self.g.lstat {
                return f(p.as_ptr(), st.as_mut_ptr() as *mut c_void) == 0;
            }
        }
        fl::file::lstat(p.as_ptr() as *const _, st.as_mut_ptr() as *mut c_void) == 0
    }

    unsafe fn is_dir(&self, path: &[u8]) -> bool {
        let mut p = path.to_vec();
        p.push(0);
        let mut st = [0u8; 256];
        let r = if self.flags & GLOB_ALTDIRFUNC != 0 && self.g.stat.is_some() {
            (self.g.stat.unwrap())(p.as_ptr(), st.as_mut_ptr() as *mut c_void)
        } else {
            fl::file::stat(p.as_ptr() as *const _, st.as_mut_ptr() as *mut c_void)
        };
        // st_mode at 24
        r == 0 && u32::from_ne_bytes([st[24], st[25], st[26], st[27]]) & 0o170000 == 0o040000
    }

    /// Expand the components `comps` below `prefix`, appending results to `out`.
    unsafe fn expand(&mut self, prefix: Vec<u8>, comps: &[&[u8]], out: &mut Vec<Vec<u8>>) {
        if self.aborted {
            return;
        }
        let Some((first, rest)) = comps.split_first() else {
            out.push(prefix);
            return;
        };
        let noescape = self.flags & GLOB_NOESCAPE != 0;
        let join = |name: &[u8]| -> Vec<u8> {
            let mut p = prefix.clone();
            if !p.is_empty() && p.last() != Some(&b'/') {
                p.push(b'/');
            }
            p.extend_from_slice(name);
            p
        };
        if first.is_empty() {
            // "a//b": keep going
            self.expand(prefix.clone(), rest, out);
            return;
        }
        if !has_magic(first, noescape) {
            let p = join(&unescape(first, noescape));
            if rest.is_empty() {
                if self.exists(&p) {
                    out.push(p);
                }
            } else {
                self.expand(p, rest, out);
            }
            return;
        }
        let Some(entries) = self.list_dir(&prefix) else { return };
        let mut fl_ = FNM_PERIOD;
        if noescape {
            fl_ |= FNM_NOESCAPE;
        }
        let dot_pattern = first.first() == Some(&b'.');
        for (name, ty) in entries {
            if (name == b"." || name == b"..") && !dot_pattern && self.flags & GLOB_PERIOD == 0 {
                continue;
            }
            let pf = if self.flags & GLOB_PERIOD != 0 { fl_ & !FNM_PERIOD } else { fl_ };
            if !fnmatch_bytes(first, &name, pf) {
                continue;
            }
            let p = join(&name);
            if rest.is_empty() {
                out.push(p);
            } else if ty == 4 /* DT_DIR */ || ((ty == 0 || ty == 10) && self.is_dir(&p)) {
                self.expand(p, rest, out);
            }
            if self.aborted {
                return;
            }
        }
    }
}

unsafe fn tilde(p: &[u8]) -> Option<Vec<u8>> {
    if p.first() != Some(&b'~') {
        return None;
    }
    let end = p.iter().position(|&c| c == b'/').unwrap_or(p.len());
    let user = &p[1..end];
    let home: Vec<u8> = if user.is_empty() {
        let h = fl::env::getenv(b"HOME\0".as_ptr());
        if !h.is_null() {
            fl::mem::cstr(h).to_vec()
        } else {
            let pw = super::users::getpwuid(fl::sys::sc0(102 /* getuid */) as u32);
            if pw.is_null() {
                return None;
            }
            fl::mem::cstr(*(pw as *const *const u8).add(5)).to_vec()
        }
    } else {
        let mut u = user.to_vec();
        u.push(0);
        let pw = super::users::getpwnam(u.as_ptr());
        if pw.is_null() {
            return None;
        }
        fl::mem::cstr(*(pw as *const *const u8).add(5)).to_vec()
    };
    let mut v = home;
    v.extend_from_slice(&p[end..]);
    Some(v)
}

unsafe fn glob_one(pat: &[u8], ctx: &mut Ctx, out: &mut Vec<Vec<u8>>) -> Result<(), c_int> {
    let mut pat = pat.to_vec();
    if ctx.flags & (GLOB_TILDE | GLOB_TILDE_CHECK) != 0 && pat.first() == Some(&b'~') {
        match tilde(&pat) {
            Some(p) => pat = p,
            None if ctx.flags & GLOB_TILDE_CHECK != 0 => return Err(GLOB_NOMATCH),
            None => {}
        }
    }
    let absolute = pat.first() == Some(&b'/');
    let comps: Vec<&[u8]> = pat.split(|&c| c == b'/').collect();
    let comps = if absolute { &comps[1..] } else { &comps[..] };
    let mut found = Vec::new();
    ctx.expand(if absolute { b"/".to_vec() } else { Vec::new() }, comps, &mut found);
    if ctx.aborted {
        return Err(GLOB_ABORTED);
    }
    // a trailing slash in the pattern only matches directories
    let want_dir = pat.last() == Some(&b'/') || ctx.flags & GLOB_ONLYDIR != 0 && pat.last() == Some(&b'/');
    if want_dir {
        found.retain(|p| ctx.is_dir(p));
        for p in found.iter_mut() {
            if p.last() != Some(&b'/') {
                p.push(b'/');
            }
        }
    }
    if found.is_empty() {
        let magic = has_magic(&pat, ctx.flags & GLOB_NOESCAPE != 0);
        if ctx.flags & GLOB_NOCHECK != 0 || (ctx.flags & GLOB_NOMAGIC != 0 && !magic) {
            out.push(pat);
            return Ok(());
        }
        return Err(GLOB_NOMATCH);
    }
    if ctx.flags & GLOB_MARK != 0 {
        for p in found.iter_mut() {
            if p.last() != Some(&b'/') && ctx.is_dir(p) {
                p.push(b'/');
            }
        }
    }
    if ctx.flags & GLOB_NOSORT == 0 {
        found.sort();
    }
    out.extend(found);
    Ok(())
}

#[no_mangle]
pub unsafe extern "C" fn glob(pattern: *const c_char, flags: c_int, errfn: ErrFn, g: *mut Glob) -> c_int {
    let g = &mut *g;
    if flags & GLOB_APPEND == 0 {
        g.pathc = 0;
        g.pathv = ptr::null_mut();
        if flags & GLOB_DOOFFS == 0 {
            g.offs = 0;
        }
    }
    let offs = if flags & GLOB_DOOFFS != 0 { g.offs } else { 0 };
    let p = fl::mem::cstr(pattern);
    let mut pats = Vec::new();
    if flags & GLOB_BRACE != 0 {
        brace_expand(p, &mut pats);
    } else {
        pats.push(p.to_vec());
    }
    let mut ctx = Ctx { flags, errfn, g: &*g, aborted: false };
    let mut results = Vec::new();
    let mut err = 0;
    for pat in &pats {
        match glob_one(pat, &mut ctx, &mut results) {
            Ok(()) => {}
            Err(GLOB_NOMATCH) => {}
            Err(e) => {
                err = e;
                break;
            }
        }
    }
    if err == 0 && results.is_empty() {
        err = GLOB_NOMATCH;
    }
    // append to gl_pathv, reserving the offsets
    if !results.is_empty() {
        let old = g.pathc;
        let n = offs + old + results.len() + 1;
        let v = umem::realloc(g.pathv as *mut c_void, n * 8) as *mut *mut c_char;
        if v.is_null() {
            return GLOB_NOSPACE;
        }
        if g.pathv.is_null() {
            for i in 0..offs {
                *v.add(i) = ptr::null_mut();
            }
        }
        for (k, r) in results.iter().enumerate() {
            let s = umem::malloc(r.len() + 1) as *mut u8;
            if s.is_null() {
                return GLOB_NOSPACE;
            }
            ptr::copy_nonoverlapping(r.as_ptr(), s, r.len());
            *s.add(r.len()) = 0;
            *v.add(offs + old + k) = s;
        }
        *v.add(offs + old + results.len()) = ptr::null_mut();
        g.pathv = v;
        g.pathc = old + results.len();
    } else if g.pathv.is_null() && flags & GLOB_DOOFFS != 0 {
        let v = umem::malloc((offs + 1) * 8) as *mut *mut c_char;
        if !v.is_null() {
            for i in 0..=offs {
                *v.add(i) = ptr::null_mut();
            }
            g.pathv = v;
        }
    }
    g.flags = flags;
    err
}

#[no_mangle]
pub unsafe extern "C" fn globfree(g: *mut Glob) {
    let g = &mut *g;
    if g.pathv.is_null() {
        return;
    }
    for i in 0..g.pathc {
        umem::free(*g.pathv.add(g.offs + i) as *mut c_void);
    }
    umem::free(g.pathv as *mut c_void);
    g.pathv = ptr::null_mut();
    g.pathc = 0;
}

#[no_mangle]
pub unsafe extern "C" fn glob64(pattern: *const c_char, flags: c_int, errfn: ErrFn, g: *mut Glob) -> c_int {
    glob(pattern, flags, errfn, g)
}
#[no_mangle]
pub unsafe extern "C" fn globfree64(g: *mut Glob) {
    globfree(g)
}
#[no_mangle]
pub unsafe extern "C" fn glob_pattern_p(p: *const c_char, quote: c_int) -> c_int {
    has_magic(fl::mem::cstr(p), quote == 0) as c_int
}
