//! Strings and memory, integer conversions, ctype (with glibc's tables, which programs
//! index directly through macros), and plain integer arithmetic helpers.

use crate::fl;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;
type c_long = i64;
type size_t = usize;

#[inline]
unsafe fn slen(s: *const c_char) -> usize {
    fl::mem::cstrlen(s)
}

// --- copying and concatenation -------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn strcpy(d: *mut c_char, s: *const c_char) -> *mut c_char {
    let n = slen(s);
    ptr::copy_nonoverlapping(s, d, n + 1);
    d
}

#[no_mangle]
pub unsafe extern "C" fn stpcpy(d: *mut c_char, s: *const c_char) -> *mut c_char {
    let n = slen(s);
    ptr::copy_nonoverlapping(s, d, n + 1);
    d.add(n)
}

#[no_mangle]
pub unsafe extern "C" fn strncpy(d: *mut c_char, s: *const c_char, n: size_t) -> *mut c_char {
    let l = strnlen(s, n);
    ptr::copy_nonoverlapping(s, d, l);
    ptr::write_bytes(d.add(l), 0, n - l);
    d
}

#[no_mangle]
pub unsafe extern "C" fn stpncpy(d: *mut c_char, s: *const c_char, n: size_t) -> *mut c_char {
    let l = strnlen(s, n);
    ptr::copy_nonoverlapping(s, d, l);
    ptr::write_bytes(d.add(l), 0, n - l);
    d.add(l)
}

#[no_mangle]
pub unsafe extern "C" fn strcat(d: *mut c_char, s: *const c_char) -> *mut c_char {
    strcpy(d.add(slen(d)), s);
    d
}

#[no_mangle]
pub unsafe extern "C" fn strncat(d: *mut c_char, s: *const c_char, n: size_t) -> *mut c_char {
    let dl = slen(d);
    let l = strnlen(s, n);
    ptr::copy_nonoverlapping(s, d.add(dl), l);
    *d.add(dl + l) = 0;
    d
}

#[no_mangle]
pub unsafe extern "C" fn strlcpy(d: *mut c_char, s: *const c_char, n: size_t) -> size_t {
    let l = slen(s);
    if n > 0 {
        let c = l.min(n - 1);
        ptr::copy_nonoverlapping(s, d, c);
        *d.add(c) = 0;
    }
    l
}

#[no_mangle]
pub unsafe extern "C" fn strlcat(d: *mut c_char, s: *const c_char, n: size_t) -> size_t {
    let dl = strnlen(d, n);
    if dl == n {
        return n + slen(s);
    }
    dl + strlcpy(d.add(dl), s, n - dl)
}

#[no_mangle]
pub unsafe extern "C" fn mempcpy(d: *mut c_void, s: *const c_void, n: size_t) -> *mut c_void {
    ptr::copy_nonoverlapping(s as *const u8, d as *mut u8, n);
    (d as *mut u8).add(n) as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn __mempcpy(d: *mut c_void, s: *const c_void, n: size_t) -> *mut c_void {
    mempcpy(d, s, n)
}

#[no_mangle]
pub unsafe extern "C" fn memccpy(d: *mut c_void, s: *const c_void, c: c_int, n: size_t) -> *mut c_void {
    let (d, s) = (d as *mut u8, s as *const u8);
    for i in 0..n {
        *d.add(i) = *s.add(i);
        if *s.add(i) == c as u8 {
            return d.add(i + 1) as *mut c_void;
        }
    }
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn bzero(d: *mut c_void, n: size_t) {
    ptr::write_bytes(d as *mut u8, 0, n);
}

#[no_mangle]
pub unsafe extern "C" fn explicit_bzero(d: *mut c_void, n: size_t) {
    for i in 0..n {
        ptr::write_volatile((d as *mut u8).add(i), 0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn bcopy(s: *const c_void, d: *mut c_void, n: size_t) {
    ptr::copy(s as *const u8, d as *mut u8, n);
}

#[no_mangle]
pub unsafe extern "C" fn swab(s: *const c_void, d: *mut c_void, n: isize) {
    let (s, d) = (s as *const u8, d as *mut u8);
    let mut i = 0;
    while i + 1 < n.max(0) as usize {
        let a = *s.add(i);
        *d.add(i) = *s.add(i + 1);
        *d.add(i + 1) = a;
        i += 2;
    }
}

// --- searching -------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn strnlen(s: *const c_char, n: size_t) -> size_t {
    let mut i = 0;
    while i < n && *s.add(i) != 0 {
        i += 1;
    }
    i
}

#[no_mangle]
pub unsafe extern "C" fn strchr(s: *const c_char, c: c_int) -> *mut c_char {
    let c = c as u8;
    let mut p = s;
    loop {
        if *p == c {
            return p as *mut c_char;
        }
        if *p == 0 {
            return ptr::null_mut();
        }
        p = p.add(1);
    }
}

#[no_mangle]
pub unsafe extern "C" fn index(s: *const c_char, c: c_int) -> *mut c_char {
    strchr(s, c)
}

#[no_mangle]
pub unsafe extern "C" fn strchrnul(s: *const c_char, c: c_int) -> *mut c_char {
    let c = c as u8;
    let mut p = s;
    while *p != 0 && *p != c {
        p = p.add(1);
    }
    p as *mut c_char
}

#[no_mangle]
pub unsafe extern "C" fn strrchr(s: *const c_char, c: c_int) -> *mut c_char {
    let c = c as u8;
    let mut last = ptr::null_mut();
    let mut p = s;
    loop {
        if *p == c {
            last = p as *mut c_char;
        }
        if *p == 0 {
            return last;
        }
        p = p.add(1);
    }
}

#[no_mangle]
pub unsafe extern "C" fn rindex(s: *const c_char, c: c_int) -> *mut c_char {
    strrchr(s, c)
}

#[no_mangle]
pub unsafe extern "C" fn memchr(s: *const c_void, c: c_int, n: size_t) -> *mut c_void {
    let s = s as *const u8;
    let c = c as u8;
    for i in 0..n {
        if *s.add(i) == c {
            return s.add(i) as *mut c_void;
        }
    }
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn rawmemchr(s: *const c_void, c: c_int) -> *mut c_void {
    let mut p = s as *const u8;
    while *p != c as u8 {
        p = p.add(1);
    }
    p as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn memrchr(s: *const c_void, c: c_int, n: size_t) -> *mut c_void {
    let s = s as *const u8;
    let mut i = n;
    while i > 0 {
        i -= 1;
        if *s.add(i) == c as u8 {
            return s.add(i) as *mut c_void;
        }
    }
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn memmem(h: *const c_void, hl: size_t, n: *const c_void, nl: size_t) -> *mut c_void {
    if nl == 0 {
        return h as *mut c_void;
    }
    if nl > hl {
        return ptr::null_mut();
    }
    let h = core::slice::from_raw_parts(h as *const u8, hl);
    let n = core::slice::from_raw_parts(n as *const u8, nl);
    match h.windows(nl).position(|w| w == n) {
        Some(i) => h.as_ptr().add(i) as *mut c_void,
        None => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn strstr(h: *const c_char, n: *const c_char) -> *mut c_char {
    memmem(h as *const c_void, slen(h), n as *const c_void, slen(n)) as *mut c_char
}

#[no_mangle]
pub unsafe extern "C" fn strcasestr(h: *const c_char, n: *const c_char) -> *mut c_char {
    let nl = slen(n);
    let mut p = h;
    loop {
        if strncasecmp(p, n, nl) == 0 {
            return p as *mut c_char;
        }
        if *p == 0 {
            return ptr::null_mut();
        }
        p = p.add(1);
    }
}

unsafe fn in_set(set: *const c_char, c: u8) -> bool {
    let mut p = set;
    while *p != 0 {
        if *p == c {
            return true;
        }
        p = p.add(1);
    }
    false
}

#[no_mangle]
pub unsafe extern "C" fn strspn(s: *const c_char, set: *const c_char) -> size_t {
    let mut i = 0;
    while *s.add(i) != 0 && in_set(set, *s.add(i)) {
        i += 1;
    }
    i
}

#[no_mangle]
pub unsafe extern "C" fn strcspn(s: *const c_char, set: *const c_char) -> size_t {
    let mut i = 0;
    while *s.add(i) != 0 && !in_set(set, *s.add(i)) {
        i += 1;
    }
    i
}

#[no_mangle]
pub unsafe extern "C" fn strpbrk(s: *const c_char, set: *const c_char) -> *mut c_char {
    let p = s.add(strcspn(s, set));
    if *p == 0 {
        ptr::null_mut()
    } else {
        p as *mut c_char
    }
}

#[no_mangle]
pub unsafe extern "C" fn strtok_r(s: *mut c_char, delim: *const c_char, save: *mut *mut c_char) -> *mut c_char {
    let mut p = if s.is_null() { *save } else { s };
    if p.is_null() {
        return ptr::null_mut();
    }
    p = p.add(strspn(p, delim));
    if *p == 0 {
        *save = p;
        return ptr::null_mut();
    }
    let end = p.add(strcspn(p, delim));
    if *end != 0 {
        *end = 0;
        *save = end.add(1);
    } else {
        *save = end;
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn __strtok_r(s: *mut c_char, delim: *const c_char, save: *mut *mut c_char) -> *mut c_char {
    strtok_r(s, delim, save)
}

#[no_mangle]
pub unsafe extern "C" fn strtok(s: *mut c_char, delim: *const c_char) -> *mut c_char {
    static mut SAVE: *mut c_char = ptr::null_mut();
    strtok_r(s, delim, ptr::addr_of_mut!(SAVE))
}

#[no_mangle]
pub unsafe extern "C" fn strsep(sp: *mut *mut c_char, delim: *const c_char) -> *mut c_char {
    let s = *sp;
    if s.is_null() {
        return ptr::null_mut();
    }
    let e = s.add(strcspn(s, delim));
    if *e != 0 {
        *e = 0;
        *sp = e.add(1);
    } else {
        *sp = ptr::null_mut();
    }
    s
}

// --- comparing -------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn strcmp(a: *const c_char, b: *const c_char) -> c_int {
    let mut i = 0;
    loop {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y || x == 0 {
            return x as c_int - y as c_int;
        }
        i += 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn strncmp(a: *const c_char, b: *const c_char, n: size_t) -> c_int {
    for i in 0..n {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y || x == 0 {
            return x as c_int - y as c_int;
        }
    }
    0
}

#[inline]
fn lower(c: u8) -> u8 {
    c.to_ascii_lowercase()
}

#[no_mangle]
pub unsafe extern "C" fn strcasecmp(a: *const c_char, b: *const c_char) -> c_int {
    let mut i = 0;
    loop {
        let (x, y) = (lower(*a.add(i)), lower(*b.add(i)));
        if x != y || x == 0 {
            return x as c_int - y as c_int;
        }
        i += 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn strncasecmp(a: *const c_char, b: *const c_char, n: size_t) -> c_int {
    for i in 0..n {
        let (x, y) = (lower(*a.add(i)), lower(*b.add(i)));
        if x != y || x == 0 {
            return x as c_int - y as c_int;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn strcasecmp_l(a: *const c_char, b: *const c_char, _l: *mut c_void) -> c_int {
    strcasecmp(a, b)
}

#[no_mangle]
pub unsafe extern "C" fn strncasecmp_l(a: *const c_char, b: *const c_char, n: size_t, _l: *mut c_void) -> c_int {
    strncasecmp(a, b, n)
}

#[no_mangle]
pub unsafe extern "C" fn strcoll(a: *const c_char, b: *const c_char) -> c_int {
    strcmp(a, b)
}

#[no_mangle]
pub unsafe extern "C" fn strcoll_l(a: *const c_char, b: *const c_char, _l: *mut c_void) -> c_int {
    strcmp(a, b)
}

#[no_mangle]
pub unsafe extern "C" fn __strcoll_l(a: *const c_char, b: *const c_char, _l: *mut c_void) -> c_int {
    strcmp(a, b)
}

#[no_mangle]
pub unsafe extern "C" fn strxfrm(d: *mut c_char, s: *const c_char, n: size_t) -> size_t {
    let l = slen(s);
    if n > l {
        ptr::copy_nonoverlapping(s, d, l + 1);
    }
    l
}

#[no_mangle]
pub unsafe extern "C" fn strxfrm_l(d: *mut c_char, s: *const c_char, n: size_t, _l: *mut c_void) -> size_t {
    strxfrm(d, s, n)
}

#[no_mangle]
pub unsafe extern "C" fn __strxfrm_l(d: *mut c_char, s: *const c_char, n: size_t, _l: *mut c_void) -> size_t {
    strxfrm(d, s, n)
}

/// GNU version order: digit runs compare as numbers ("a2" < "a10").
#[no_mangle]
pub unsafe extern "C" fn strverscmp(a: *const c_char, b: *const c_char) -> c_int {
    let a = fl::mem::cstr(a);
    let b = fl::mem::cstr(b);
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (x, y) = (&a[si..i], &b[sj..j]);
            // leading zeros: fractional parts compare as strings, the longer zero run first
            let zx = x.iter().take_while(|&&c| c == b'0').count();
            let zy = y.iter().take_while(|&&c| c == b'0').count();
            if zx > 0 || zy > 0 {
                if zx != zy {
                    return if zx > zy { -1 } else { 1 };
                }
                match x.cmp(y) {
                    core::cmp::Ordering::Less => return -1,
                    core::cmp::Ordering::Greater => return 1,
                    _ => continue,
                }
            }
            let (x, y) = (&x[zx..], &y[zy..]);
            if x.len() != y.len() {
                return if x.len() < y.len() { -1 } else { 1 };
            }
            match x.cmp(y) {
                core::cmp::Ordering::Less => return -1,
                core::cmp::Ordering::Greater => return 1,
                _ => {}
            }
        } else {
            if a[i] != b[j] {
                return a[i] as c_int - b[j] as c_int;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i) as c_int - (b.len() - j) as c_int
}

// --- duplicating (with the program's malloc) --------------------------------------------------

pub unsafe fn umalloc(n: usize) -> *mut u8 {
    fl::malloc::umalloc(n)
}

#[no_mangle]
pub unsafe extern "C" fn strdup(s: *const c_char) -> *mut c_char {
    let n = slen(s) + 1;
    let d = umalloc(n);
    if !d.is_null() {
        ptr::copy_nonoverlapping(s, d, n);
    }
    d
}

#[no_mangle]
pub unsafe extern "C" fn __strdup(s: *const c_char) -> *mut c_char {
    strdup(s)
}

#[no_mangle]
pub unsafe extern "C" fn strndup(s: *const c_char, n: size_t) -> *mut c_char {
    let l = strnlen(s, n);
    let d = umalloc(l + 1);
    if !d.is_null() {
        ptr::copy_nonoverlapping(s, d, l);
        *d.add(l) = 0;
    }
    d
}

#[no_mangle]
pub unsafe extern "C" fn __strndup(s: *const c_char, n: size_t) -> *mut c_char {
    strndup(s, n)
}

// --- bits ------------------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn ffs(i: c_int) -> c_int {
    if i == 0 { 0 } else { i.trailing_zeros() as c_int + 1 }
}
#[no_mangle]
pub extern "C" fn ffsl(i: c_long) -> c_int {
    if i == 0 { 0 } else { i.trailing_zeros() as c_int + 1 }
}
#[no_mangle]
pub extern "C" fn ffsll(i: i64) -> c_int {
    ffsl(i)
}
#[no_mangle]
pub extern "C" fn abs(i: c_int) -> c_int {
    i.wrapping_abs()
}
#[no_mangle]
pub extern "C" fn labs(i: c_long) -> c_long {
    i.wrapping_abs()
}
#[no_mangle]
pub extern "C" fn llabs(i: i64) -> i64 {
    i.wrapping_abs()
}
#[no_mangle]
pub extern "C" fn imaxabs(i: i64) -> i64 {
    i.wrapping_abs()
}

#[repr(C)]
pub struct DivT {
    quot: c_int,
    rem: c_int,
}
#[repr(C)]
pub struct LDivT {
    quot: c_long,
    rem: c_long,
}

#[no_mangle]
pub extern "C" fn div(a: c_int, b: c_int) -> DivT {
    DivT { quot: a / b, rem: a % b }
}
#[no_mangle]
pub extern "C" fn ldiv(a: c_long, b: c_long) -> LDivT {
    LDivT { quot: a / b, rem: a % b }
}
#[no_mangle]
pub extern "C" fn lldiv(a: i64, b: i64) -> LDivT {
    LDivT { quot: a / b, rem: a % b }
}
#[no_mangle]
pub extern "C" fn imaxdiv(a: i64, b: i64) -> LDivT {
    LDivT { quot: a / b, rem: a % b }
}

// --- integer parsing ---------------------------------------------------------------------

/// Parse an integer: returns (magnitude, negative, end, overflowed). `c23` accepts 0b.
pub unsafe fn parse_int(s: *const c_char, end: *mut *mut c_char, base: c_int, c23: bool) -> Option<(u64, bool, bool)> {
    let mut p = s;
    while matches!(*p, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        p = p.add(1);
    }
    let mut neg = false;
    if *p == b'-' || *p == b'+' {
        neg = *p == b'-';
        p = p.add(1);
    }
    let mut base = base as u32;
    if base == 1 || base > 36 {
        fl::errno::set(fl::errno::EINVAL);
        if !end.is_null() {
            *end = s as *mut c_char;
        }
        return None;
    }
    let digit = |c: u8| -> Option<u32> {
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 10,
            b'A'..=b'Z' => (c - b'A') as u32 + 10,
            _ => return None,
        };
        Some(d)
    };
    if (base == 0 || base == 16) && *p == b'0' && (*p.add(1) | 0x20) == b'x' && digit(*p.add(2)).map_or(false, |d| d < 16) {
        p = p.add(2);
        base = 16;
    } else if c23 && (base == 0 || base == 2) && *p == b'0' && (*p.add(1) | 0x20) == b'b' && matches!(*p.add(2), b'0' | b'1') {
        p = p.add(2);
        base = 2;
    } else if base == 0 {
        base = if *p == b'0' { 8 } else { 10 };
    }
    let start = p;
    let mut v: u64 = 0;
    let mut over = false;
    while let Some(d) = digit(*p) {
        if d >= base {
            break;
        }
        match v.checked_mul(base as u64).and_then(|x| x.checked_add(d as u64)) {
            Some(x) => v = x,
            None => over = true,
        }
        p = p.add(1);
    }
    if p == start {
        if !end.is_null() {
            *end = s as *mut c_char;
        }
        return Some((0, false, false));
    }
    if !end.is_null() {
        *end = p as *mut c_char;
    }
    Some((v, neg, over))
}

unsafe fn to_signed(r: Option<(u64, bool, bool)>, min: i64, max: i64) -> i64 {
    match r {
        None => 0,
        Some((v, neg, over)) => {
            if neg {
                if over || v > (min as i128).unsigned_abs() as u64 {
                    fl::errno::set(fl::errno::ERANGE);
                    min
                } else {
                    (v as i64).wrapping_neg()
                }
            } else if over || v > max as u64 {
                fl::errno::set(fl::errno::ERANGE);
                max
            } else {
                v as i64
            }
        }
    }
}

unsafe fn to_unsigned(r: Option<(u64, bool, bool)>, max: u64) -> u64 {
    match r {
        None => 0,
        Some((v, neg, over)) => {
            if over || v > max {
                fl::errno::set(fl::errno::ERANGE);
                max
            } else if neg {
                v.wrapping_neg() & max
            } else {
                v
            }
        }
    }
}

macro_rules! strto {
    ($name:ident, $c23:expr, $ty:ty, signed) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name(s: *const c_char, end: *mut *mut c_char, base: c_int) -> $ty {
            to_signed(parse_int(s, end, base, $c23), <$ty>::MIN as i64, <$ty>::MAX as i64) as $ty
        }
    };
    ($name:ident, $c23:expr, $ty:ty, unsigned) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name(s: *const c_char, end: *mut *mut c_char, base: c_int) -> $ty {
            to_unsigned(parse_int(s, end, base, $c23), <$ty>::MAX as u64) as $ty
        }
    };
}

strto!(strtol, false, i64, signed);
strto!(strtoll, false, i64, signed);
strto!(strtoq, false, i64, signed);
strto!(strtoimax, false, i64, signed);
strto!(strtoul, false, u64, unsigned);
strto!(strtoull, false, u64, unsigned);
strto!(strtouq, false, u64, unsigned);
strto!(strtoumax, false, u64, unsigned);
strto!(__isoc23_strtol, true, i64, signed);
strto!(__isoc23_strtoll, true, i64, signed);
strto!(__isoc23_strtoimax, true, i64, signed);
strto!(__isoc23_strtoul, true, u64, unsigned);
strto!(__isoc23_strtoull, true, u64, unsigned);
strto!(__isoc23_strtoumax, true, u64, unsigned);

#[no_mangle]
pub unsafe extern "C" fn strtol_l(s: *const c_char, e: *mut *mut c_char, b: c_int, _l: *mut c_void) -> i64 {
    strtol(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn strtoll_l(s: *const c_char, e: *mut *mut c_char, b: c_int, _l: *mut c_void) -> i64 {
    strtoll(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn strtoul_l(s: *const c_char, e: *mut *mut c_char, b: c_int, _l: *mut c_void) -> u64 {
    strtoul(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn strtoull_l(s: *const c_char, e: *mut *mut c_char, b: c_int, _l: *mut c_void) -> u64 {
    strtoull(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __isoc23_strtoll_l(s: *const c_char, e: *mut *mut c_char, b: c_int, _l: *mut c_void) -> i64 {
    __isoc23_strtoll(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __isoc23_strtoull_l(s: *const c_char, e: *mut *mut c_char, b: c_int, _l: *mut c_void) -> u64 {
    __isoc23_strtoull(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __isoc23_strtol_l(s: *const c_char, e: *mut *mut c_char, b: c_int, _l: *mut c_void) -> i64 {
    __isoc23_strtol(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __isoc23_strtoul_l(s: *const c_char, e: *mut *mut c_char, b: c_int, _l: *mut c_void) -> u64 {
    __isoc23_strtoul(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __strtol_internal(s: *const c_char, e: *mut *mut c_char, b: c_int, _g: c_int) -> i64 {
    strtol(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __strtoul_internal(s: *const c_char, e: *mut *mut c_char, b: c_int, _g: c_int) -> u64 {
    strtoul(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __strtoll_internal(s: *const c_char, e: *mut *mut c_char, b: c_int, _g: c_int) -> i64 {
    strtoll(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __strtoull_internal(s: *const c_char, e: *mut *mut c_char, b: c_int, _g: c_int) -> u64 {
    strtoull(s, e, b)
}

#[no_mangle]
pub unsafe extern "C" fn atoi(s: *const c_char) -> c_int {
    strtol(s, ptr::null_mut(), 10) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn atol(s: *const c_char) -> c_long {
    strtol(s, ptr::null_mut(), 10)
}
#[no_mangle]
pub unsafe extern "C" fn atoll(s: *const c_char) -> i64 {
    strtoll(s, ptr::null_mut(), 10)
}

// --- ctype -------------------------------------------------------------------------------------
//
// glibc's ctype macros read a table of class bits, indexed by the character (-128..255),
// through __ctype_b_loc(); toupper/tolower likewise through tables of int32. The bit
// numbers are glibc's (little-endian layout of _ISbit).

const fn isbit(b: u32) -> u16 {
    if b < 8 { ((1u32 << b) << 8) as u16 } else { ((1u32 << b) >> 8) as u16 }
}
pub const IS_UPPER: u16 = isbit(0);
pub const IS_LOWER: u16 = isbit(1);
pub const IS_ALPHA: u16 = isbit(2);
pub const IS_DIGIT: u16 = isbit(3);
pub const IS_XDIGIT: u16 = isbit(4);
pub const IS_SPACE: u16 = isbit(5);
pub const IS_PRINT: u16 = isbit(6);
pub const IS_GRAPH: u16 = isbit(7);
pub const IS_BLANK: u16 = isbit(8);
pub const IS_CNTRL: u16 = isbit(9);
pub const IS_PUNCT: u16 = isbit(10);
pub const IS_ALNUM: u16 = isbit(11);

const fn class_of(c: u32) -> u16 {
    if c >= 128 {
        return 0;
    }
    let c = c as u8;
    let mut m = 0u16;
    if c.is_ascii_uppercase() {
        m |= IS_UPPER | IS_ALPHA | IS_ALNUM;
    }
    if c.is_ascii_lowercase() {
        m |= IS_LOWER | IS_ALPHA | IS_ALNUM;
    }
    if c.is_ascii_digit() {
        m |= IS_DIGIT | IS_ALNUM;
    }
    if c.is_ascii_hexdigit() {
        m |= IS_XDIGIT;
    }
    if c == b' ' || (c >= 9 && c <= 13) {
        m |= IS_SPACE;
    }
    if c == b' ' || c == b'\t' {
        m |= IS_BLANK;
    }
    if c < 32 || c == 127 {
        m |= IS_CNTRL;
    }
    if c >= 32 && c < 127 {
        m |= IS_PRINT;
    }
    if c > 32 && c < 127 {
        m |= IS_GRAPH;
    }
    if c.is_ascii_punctuation() {
        m |= IS_PUNCT;
    }
    m
}

const fn build_b() -> [u16; 384] {
    let mut t = [0u16; 384];
    let mut i = 0;
    while i < 384 {
        // index i is character i - 128; characters -128..-1 alias 128..255 (none have classes)
        let c = if i < 128 { 0x100 } else { (i - 128) as u32 };
        t[i] = if c == 0x100 { 0 } else { class_of(c) };
        i += 1;
    }
    t
}

const fn build_case(upper: bool) -> [i32; 384] {
    let mut t = [0i32; 384];
    let mut i = 0;
    while i < 384 {
        let c = i as i32 - 128;
        let mut v = c;
        if upper && c >= b'a' as i32 && c <= b'z' as i32 {
            v = c - 32;
        }
        if !upper && c >= b'A' as i32 && c <= b'Z' as i32 {
            v = c + 32;
        }
        // glibc maps -128..-1 to the values for 128..255 (unchanged here)
        if c < 0 {
            v = c;
        }
        t[i] = v;
        i += 1;
    }
    t
}

static CTYPE_B: [u16; 384] = build_b();
static CTYPE_TOUPPER: [i32; 384] = build_case(true);
static CTYPE_TOLOWER: [i32; 384] = build_case(false);

// pointers to entry 128 (character 0), as glibc's tables are used
static mut CTYPE_B_PTR: *const u16 = ptr::null();
static mut CTYPE_UP_PTR: *const i32 = ptr::null();
static mut CTYPE_LO_PTR: *const i32 = ptr::null();

pub unsafe fn ctype_init() {
    CTYPE_B_PTR = CTYPE_B.as_ptr().add(128);
    CTYPE_UP_PTR = CTYPE_TOUPPER.as_ptr().add(128);
    CTYPE_LO_PTR = CTYPE_TOLOWER.as_ptr().add(128);
}

pub fn ctype_tables() -> (*const u16, *const i32, *const i32) {
    // SAFETY: set once by ctype_init before the program runs.
    unsafe { (CTYPE_B_PTR, CTYPE_LO_PTR, CTYPE_UP_PTR) }
}

#[no_mangle]
pub unsafe extern "C" fn __ctype_b_loc() -> *const *const u16 {
    ptr::addr_of!(CTYPE_B_PTR)
}
#[no_mangle]
pub unsafe extern "C" fn __ctype_toupper_loc() -> *const *const i32 {
    ptr::addr_of!(CTYPE_UP_PTR)
}
#[no_mangle]
pub unsafe extern "C" fn __ctype_tolower_loc() -> *const *const i32 {
    ptr::addr_of!(CTYPE_LO_PTR)
}
#[no_mangle]
pub unsafe extern "C" fn __ctype_get_mb_cur_max() -> size_t {
    super::locale::mb_cur_max()
}

#[inline]
fn cls(c: c_int) -> u16 {
    if (0..256).contains(&c) {
        CTYPE_B[(c + 128) as usize]
    } else {
        0
    }
}

macro_rules! isfn {
    ($name:ident, $name_l:ident, $mask:expr) => {
        #[no_mangle]
        pub extern "C" fn $name(c: c_int) -> c_int {
            (cls(c) & $mask != 0) as c_int
        }
        #[no_mangle]
        pub extern "C" fn $name_l(c: c_int, _l: *mut c_void) -> c_int {
            (cls(c) & $mask != 0) as c_int
        }
    };
}
isfn!(isalpha, isalpha_l, IS_ALPHA);
isfn!(isupper, isupper_l, IS_UPPER);
isfn!(islower, islower_l, IS_LOWER);
isfn!(isdigit, isdigit_l, IS_DIGIT);
isfn!(isxdigit, isxdigit_l, IS_XDIGIT);
isfn!(isspace, isspace_l, IS_SPACE);
isfn!(isprint, isprint_l, IS_PRINT);
isfn!(isgraph, isgraph_l, IS_GRAPH);
isfn!(isblank, isblank_l, IS_BLANK);
isfn!(iscntrl, iscntrl_l, IS_CNTRL);
isfn!(ispunct, ispunct_l, IS_PUNCT);
isfn!(isalnum, isalnum_l, IS_ALNUM);

#[no_mangle]
pub extern "C" fn isascii(c: c_int) -> c_int {
    ((c & !0x7f) == 0) as c_int
}
#[no_mangle]
pub extern "C" fn toascii(c: c_int) -> c_int {
    c & 0x7f
}
#[no_mangle]
pub extern "C" fn toupper(c: c_int) -> c_int {
    if (b'a' as c_int..=b'z' as c_int).contains(&c) { c - 32 } else { c }
}
#[no_mangle]
pub extern "C" fn tolower(c: c_int) -> c_int {
    if (b'A' as c_int..=b'Z' as c_int).contains(&c) { c + 32 } else { c }
}
#[no_mangle]
pub extern "C" fn toupper_l(c: c_int, _l: *mut c_void) -> c_int {
    toupper(c)
}
#[no_mangle]
pub extern "C" fn tolower_l(c: c_int, _l: *mut c_void) -> c_int {
    tolower(c)
}
#[no_mangle]
pub extern "C" fn __toupper_l(c: c_int, _l: *mut c_void) -> c_int {
    toupper(c)
}
#[no_mangle]
pub extern "C" fn __tolower_l(c: c_int, _l: *mut c_void) -> c_int {
    tolower(c)
}
#[no_mangle]
pub extern "C" fn _toupper(c: c_int) -> c_int {
    toupper(c)
}
#[no_mangle]
pub extern "C" fn _tolower(c: c_int) -> c_int {
    tolower(c)
}

// --- glibc's GNU strerror_r (the POSIX one is __xpg_strerror_r, in flint-libc) -------------

#[no_mangle]
pub unsafe extern "C" fn strerror_r(e: c_int, buf: *mut c_char, n: size_t) -> *mut c_char {
    let s = fl::strerror::strerror(e);
    if n == 0 {
        return s as *mut c_char;
    }
    let l = slen(s as *const u8).min(n - 1);
    ptr::copy_nonoverlapping(s as *const u8, buf, l);
    *buf.add(l) = 0;
    buf
}

#[no_mangle]
pub unsafe extern "C" fn strerror_l(e: c_int, _l: *mut c_void) -> *mut c_char {
    fl::strerror::strerror(e) as *mut c_char
}

#[no_mangle]
pub unsafe extern "C" fn strerrordesc_np(e: c_int) -> *const c_char {
    fl::strerror::strerror(e) as *const c_char
}

static SIGNAMES: [&[u8]; 32] = [
    b"Unknown signal 0\0", b"Hangup\0", b"Interrupt\0", b"Quit\0", b"Illegal instruction\0",
    b"Trace/breakpoint trap\0", b"Aborted\0", b"Bus error\0", b"Floating point exception\0",
    b"Killed\0", b"User defined signal 1\0", b"Segmentation fault\0", b"User defined signal 2\0",
    b"Broken pipe\0", b"Alarm clock\0", b"Terminated\0", b"Stack fault\0", b"Child exited\0",
    b"Continued\0", b"Stopped (signal)\0", b"Stopped\0", b"Stopped (tty input)\0",
    b"Stopped (tty output)\0", b"Urgent I/O condition\0", b"CPU time limit exceeded\0",
    b"File size limit exceeded\0", b"Virtual timer expired\0", b"Profiling timer expired\0",
    b"Window changed\0", b"I/O possible\0", b"Power failure\0", b"Bad system call\0",
];

#[no_mangle]
pub unsafe extern "C" fn strsignal(sig: c_int) -> *mut c_char {
    if (0..32).contains(&sig) {
        SIGNAMES[sig as usize].as_ptr() as *mut c_char
    } else if (34..=64).contains(&sig) {
        b"Real-time signal\0".as_ptr() as *mut c_char
    } else {
        b"Unknown signal\0".as_ptr() as *mut c_char
    }
}

#[no_mangle]
pub unsafe extern "C" fn sigdescr_np(sig: c_int) -> *const c_char {
    strsignal(sig)
}

static ABBREV: [&[u8]; 32] = [
    b"0\0", b"HUP\0", b"INT\0", b"QUIT\0", b"ILL\0", b"TRAP\0", b"ABRT\0", b"BUS\0", b"FPE\0", b"KILL\0",
    b"USR1\0", b"SEGV\0", b"USR2\0", b"PIPE\0", b"ALRM\0", b"TERM\0", b"STKFLT\0", b"CHLD\0", b"CONT\0",
    b"STOP\0", b"TSTP\0", b"TTIN\0", b"TTOU\0", b"URG\0", b"XCPU\0", b"XFSZ\0", b"VTALRM\0", b"PROF\0",
    b"WINCH\0", b"POLL\0", b"PWR\0", b"SYS\0",
];

#[no_mangle]
pub unsafe extern "C" fn sigabbrev_np(sig: c_int) -> *const c_char {
    if (1..32).contains(&sig) { ABBREV[sig as usize].as_ptr() } else { ptr::null() }
}
