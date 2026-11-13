//! Locales (Flint has two: "C" and UTF-8 ones, which behave the same apart from their
//! names and CODESET), multibyte and wide characters (UTF-8 and UTF-32), wide strings,
//! iconv between the common Unicode and Latin encodings, and gettext without catalogs.

use super::string::{ctype_tables, umalloc};
use crate::fl;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;
type size_t = usize;
type wchar_t = i32;
type wint_t = u32;

const WEOF: wint_t = 0xffff_ffff;
const EILSEQ: c_int = 84;

// --- locale objects ------------------------------------------------------------------------

/// glibc's `struct __locale_struct`: macros read __ctype_b and the case tables directly.
#[repr(C)]
pub struct Locale {
    locales: [*mut c_void; 13],
    ctype_b: *const u16,
    ctype_tolower: *const i32,
    ctype_toupper: *const i32,
    names: [*const c_char; 13],
    // flint
    utf8: bool,
    name: [u8; 64],
}

static mut C_LOCALE: Locale = Locale {
    locales: [ptr::null_mut(); 13],
    ctype_b: ptr::null(),
    ctype_tolower: ptr::null(),
    ctype_toupper: ptr::null(),
    names: [ptr::null(); 13],
    utf8: false,
    name: [0; 64],
};

/// The global locale's name, per category (LC_CTYPE..LC_IDENTIFICATION, 13 of them; LC_ALL
/// is 6 and has no slot of its own).
static mut NAMES: [[u8; 64]; 13] = [[0; 64]; 13];
static mut ALL_NAME: [u8; 64] = [0; 64];
static mut GLOBAL_UTF8: bool = false;

const LC_ALL: c_int = 6;

unsafe fn set_name(dst: &mut [u8; 64], s: &[u8]) {
    let n = s.len().min(63);
    dst[..n].copy_from_slice(&s[..n]);
    dst[n] = 0;
}

pub unsafe fn locale_init() {
    let (b, lo, up) = ctype_tables();
    let c = &mut *ptr::addr_of_mut!(C_LOCALE);
    c.ctype_b = b;
    c.ctype_tolower = lo;
    c.ctype_toupper = up;
    set_name(&mut c.name, b"C");
    for i in 0..13 {
        c.names[i] = c.name.as_ptr();
        set_name(&mut (*ptr::addr_of_mut!(NAMES))[i], b"C");
    }
    set_name(&mut *ptr::addr_of_mut!(ALL_NAME), b"C");
}

fn is_utf8_name(n: &[u8]) -> bool {
    let l: Vec<u8> = n.iter().map(|c| c.to_ascii_lowercase()).collect();
    l.windows(5).any(|w| w == b"utf-8") || l.windows(4).any(|w| w == b"utf8")
}

/// Which name a locale request "" means: LC_ALL, then LC_<category>, then LANG.
unsafe fn env_locale(cat: c_int) -> Vec<u8> {
    const CATS: [&[u8]; 13] = [
        b"LC_CTYPE", b"LC_NUMERIC", b"LC_TIME", b"LC_COLLATE", b"LC_MONETARY", b"LC_MESSAGES", b"LC_ALL",
        b"LC_PAPER", b"LC_NAME", b"LC_ADDRESS", b"LC_TELEPHONE", b"LC_MEASUREMENT", b"LC_IDENTIFICATION",
    ];
    let get = |n: &[u8]| -> Option<Vec<u8>> {
        let mut k = n.to_vec();
        k.push(0);
        let v = fl::env::getenv(k.as_ptr());
        if v.is_null() || *v == 0 {
            None
        } else {
            Some(fl::mem::cstr(v).to_vec())
        }
    };
    if let Some(v) = get(b"LC_ALL") {
        return v;
    }
    if cat != LC_ALL && (0..13).contains(&cat) {
        if let Some(v) = get(CATS[cat as usize]) {
            return v;
        }
    }
    get(b"LANG").unwrap_or_else(|| b"C".to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn setlocale(cat: c_int, name: *const c_char) -> *mut c_char {
    if !(0..13).contains(&cat) {
        fl::errno::set(fl::errno::EINVAL);
        return ptr::null_mut();
    }
    let names = &mut *ptr::addr_of_mut!(NAMES);
    if name.is_null() {
        return if cat == LC_ALL { ptr::addr_of_mut!(ALL_NAME) as *mut c_char } else { names[cat as usize].as_mut_ptr() };
    }
    let req = fl::mem::cstr(name);
    let n = if req.is_empty() { env_locale(cat) } else { req.to_vec() };
    // anything other than C/POSIX must be a UTF-8 locale here
    let ok = n == b"C" || n == b"POSIX" || is_utf8_name(&n);
    if !ok {
        return ptr::null_mut();
    }
    if cat == LC_ALL {
        for i in 0..13 {
            set_name(&mut names[i], &n);
        }
        set_name(&mut *ptr::addr_of_mut!(ALL_NAME), &n);
    } else {
        set_name(&mut names[cat as usize], &n);
    }
    GLOBAL_UTF8 = is_utf8_name(fl::mem::cstr(names[0].as_ptr()));
    if cat == LC_ALL { ptr::addr_of_mut!(ALL_NAME) as *mut c_char } else { names[cat as usize].as_mut_ptr() }
}

#[no_mangle]
pub unsafe extern "C" fn newlocale(_mask: c_int, name: *const c_char, base: *mut Locale) -> *mut Locale {
    let n = if name.is_null() { b"C".to_vec() } else { fl::mem::cstr(name).to_vec() };
    let n = if n.is_empty() { env_locale(LC_ALL) } else { n };
    if !(n == b"C" || n == b"POSIX" || is_utf8_name(&n)) {
        fl::errno::set(2 /* ENOENT */);
        return ptr::null_mut();
    }
    let l = if base.is_null() || base == ptr::addr_of_mut!(C_LOCALE) {
        let p = fl::malloc::imalloc(core::mem::size_of::<Locale>()) as *mut Locale;
        if p.is_null() {
            return p;
        }
        ptr::copy_nonoverlapping(ptr::addr_of!(C_LOCALE), p, 1);
        p
    } else {
        base
    };
    set_name(&mut (*l).name, &n);
    (*l).utf8 = is_utf8_name(&n);
    for i in 0..13 {
        (*l).names[i] = (*l).name.as_ptr();
    }
    l
}

#[no_mangle]
pub unsafe extern "C" fn __newlocale(m: c_int, n: *const c_char, b: *mut Locale) -> *mut Locale {
    newlocale(m, n, b)
}

#[no_mangle]
pub unsafe extern "C" fn duplocale(l: *mut Locale) -> *mut Locale {
    let src = if l as isize == -1 || l.is_null() { ptr::addr_of_mut!(C_LOCALE) } else { l };
    let p = fl::malloc::imalloc(core::mem::size_of::<Locale>()) as *mut Locale;
    if !p.is_null() {
        ptr::copy_nonoverlapping(src, p, 1);
        for i in 0..13 {
            (*p).names[i] = (*p).name.as_ptr();
        }
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn __duplocale(l: *mut Locale) -> *mut Locale {
    duplocale(l)
}

#[no_mangle]
pub unsafe extern "C" fn freelocale(l: *mut Locale) {
    if !l.is_null() && l != ptr::addr_of_mut!(C_LOCALE) && l as isize != -1 {
        fl::malloc::ifree(l as *mut u8);
    }
}

#[no_mangle]
pub unsafe extern "C" fn __freelocale(l: *mut Locale) {
    freelocale(l)
}

/// The thread's locale (uselocale); -1 means the global one. Kept per thread in a slot of
/// the thread block glibc programs never look at.
#[no_mangle]
pub unsafe extern "C" fn uselocale(l: *mut Locale) -> *mut Locale {
    let t = fl::tls::current();
    let slot = &mut (*t)._tcbhead_rest[6];
    let old = if *slot == 0 { usize::MAX } else { *slot };
    if !l.is_null() {
        *slot = if l as isize == -1 { 0 } else { l as usize };
    }
    old as *mut Locale
}

#[no_mangle]
pub unsafe extern "C" fn __uselocale(l: *mut Locale) -> *mut Locale {
    uselocale(l)
}

/// Whether multibyte text is UTF-8 for the calling thread. Flint decodes UTF-8 in every
/// locale; only CODESET's answer differs.
pub fn mb_cur_max() -> usize {
    6
}

/// Is the calling thread's LC_CTYPE a UTF-8 one? Pattern matching (fnmatch, glob, regex)
/// works on bytes in the C locale, as glibc's does.
pub fn ctype_utf8() -> bool {
    // SAFETY: the thread block exists; a set slot holds a live locale object.
    unsafe {
        let slot = (*fl::tls::current())._tcbhead_rest[6];
        if slot != 0 {
            (*(slot as *const Locale)).utf8
        } else {
            GLOBAL_UTF8
        }
    }
}

#[repr(C)]
pub struct Lconv {
    decimal_point: *const c_char,
    thousands_sep: *const c_char,
    grouping: *const c_char,
    int_curr_symbol: *const c_char,
    currency_symbol: *const c_char,
    mon_decimal_point: *const c_char,
    mon_thousands_sep: *const c_char,
    mon_grouping: *const c_char,
    positive_sign: *const c_char,
    negative_sign: *const c_char,
    int_frac_digits: u8,
    frac_digits: u8,
    p_cs_precedes: u8,
    p_sep_by_space: u8,
    n_cs_precedes: u8,
    n_sep_by_space: u8,
    p_sign_posn: u8,
    n_sign_posn: u8,
    int_p_cs_precedes: u8,
    int_p_sep_by_space: u8,
    int_n_cs_precedes: u8,
    int_n_sep_by_space: u8,
    int_p_sign_posn: u8,
    int_n_sign_posn: u8,
}

static mut LCONV: Lconv = Lconv {
    decimal_point: ptr::null(),
    thousands_sep: ptr::null(),
    grouping: ptr::null(),
    int_curr_symbol: ptr::null(),
    currency_symbol: ptr::null(),
    mon_decimal_point: ptr::null(),
    mon_thousands_sep: ptr::null(),
    mon_grouping: ptr::null(),
    positive_sign: ptr::null(),
    negative_sign: ptr::null(),
    int_frac_digits: 127,
    frac_digits: 127,
    p_cs_precedes: 127,
    p_sep_by_space: 127,
    n_cs_precedes: 127,
    n_sep_by_space: 127,
    p_sign_posn: 127,
    n_sign_posn: 127,
    int_p_cs_precedes: 127,
    int_p_sep_by_space: 127,
    int_n_cs_precedes: 127,
    int_n_sep_by_space: 127,
    int_p_sign_posn: 127,
    int_n_sign_posn: 127,
};

#[no_mangle]
pub unsafe extern "C" fn localeconv() -> *mut Lconv {
    let l = &mut *ptr::addr_of_mut!(LCONV);
    l.decimal_point = b".\0".as_ptr();
    let e = b"\0".as_ptr();
    l.thousands_sep = e;
    l.grouping = e;
    l.int_curr_symbol = e;
    l.currency_symbol = e;
    l.mon_decimal_point = e;
    l.mon_thousands_sep = e;
    l.mon_grouping = e;
    l.positive_sign = e;
    l.negative_sign = e;
    l
}

static DAYS: [&[u8]; 7] = [b"Sunday\0", b"Monday\0", b"Tuesday\0", b"Wednesday\0", b"Thursday\0", b"Friday\0", b"Saturday\0"];
static ABDAYS: [&[u8]; 7] = [b"Sun\0", b"Mon\0", b"Tue\0", b"Wed\0", b"Thu\0", b"Fri\0", b"Sat\0"];
static MONTHS: [&[u8]; 12] = [
    b"January\0", b"February\0", b"March\0", b"April\0", b"May\0", b"June\0", b"July\0", b"August\0",
    b"September\0", b"October\0", b"November\0", b"December\0",
];
static ABMONTHS: [&[u8]; 12] = [
    b"Jan\0", b"Feb\0", b"Mar\0", b"Apr\0", b"May\0", b"Jun\0", b"Jul\0", b"Aug\0", b"Sep\0", b"Oct\0", b"Nov\0", b"Dec\0",
];

pub fn day_name(i: usize, abbr: bool) -> &'static [u8] {
    let s = if abbr { ABDAYS[i % 7] } else { DAYS[i % 7] };
    &s[..s.len() - 1]
}

pub fn month_name(i: usize, abbr: bool) -> &'static [u8] {
    let s = if abbr { ABMONTHS[i % 12] } else { MONTHS[i % 12] };
    &s[..s.len() - 1]
}

static FIRST_WEEKDAY: [u8; 2] = [2, 0];

#[no_mangle]
pub unsafe extern "C" fn nl_langinfo(item: c_int) -> *mut c_char {
    nl_langinfo_l(item, ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn nl_langinfo_l(item: c_int, l: *mut Locale) -> *mut c_char {
    let utf8 = if l.is_null() || l as isize == -1 { ctype_utf8() } else { (*l).utf8 };
    let cat = item >> 16;
    let idx = item & 0xffff;
    let s: &[u8] = match (cat, idx) {
        (0, 14) => if utf8 { b"UTF-8\0" } else { b"ANSI_X3.4-1968\0" }, // CODESET
        (1, 0) => b".\0",                                                // RADIXCHAR
        (1, 1) => b"\0",                                                 // THOUSEP
        (2, 0..=6) => ABDAYS[idx as usize],
        (2, 7..=13) => DAYS[(idx - 7) as usize],
        (2, 14..=25) => ABMONTHS[(idx - 14) as usize],
        (2, 26..=37) => MONTHS[(idx - 26) as usize],
        (2, 38) => b"AM\0",
        (2, 39) => b"PM\0",
        (2, 40) => b"%a %b %e %H:%M:%S %Y\0", // D_T_FMT
        (2, 41) => b"%m/%d/%y\0",              // D_FMT
        (2, 42) => b"%H:%M:%S\0",              // T_FMT
        (2, 43) => b"%I:%M:%S %p\0",           // T_FMT_AMPM
        (2, 102) => return 19971130usize as *mut c_char, // _NL_TIME_WEEK_1STDAY (a Sunday)
        (2, 101) => return 7usize as *mut c_char,        // _NL_TIME_WEEK_NDAYS
        (2, 104) => &FIRST_WEEKDAY,                       // _NL_TIME_FIRST_WEEKDAY: Monday
        (4, 15) => b"-\0",                                 // CRNCYSTR
        (5, 0) => b"^[yY]\0",                              // YESEXPR
        (5, 1) => b"^[nN]\0",                              // NOEXPR
        (5, 2) => b"yes\0",
        (5, 3) => b"no\0",
        (7, 0) => return 297usize as *mut c_char,          // _NL_PAPER_HEIGHT (A4, mm)
        (7, 1) => return 210usize as *mut c_char,          // _NL_PAPER_WIDTH
        (11, 0) => return 1usize as *mut c_char,           // _NL_MEASUREMENT_MEASUREMENT: metric
        _ => b"\0",
    };
    s.as_ptr() as *mut c_char
}

#[no_mangle]
pub unsafe extern "C" fn __nl_langinfo_l(item: c_int, l: *mut Locale) -> *mut c_char {
    nl_langinfo_l(item, l)
}

// --- multibyte (UTF-8) ---------------------------------------------------------------------

/// glibc's mbstate_t: { int count; union { wint_t wch; char c[4]; } value; }. Here count is
/// the number of bytes still expected and value the code point decoded so far.
#[repr(C)]
pub struct MbState {
    count: i32,
    value: u32,
}

static mut INTERNAL_STATE: [MbState; 6] = [
    MbState { count: 0, value: 0 },
    MbState { count: 0, value: 0 },
    MbState { count: 0, value: 0 },
    MbState { count: 0, value: 0 },
    MbState { count: 0, value: 0 },
    MbState { count: 0, value: 0 },
];

/// Decode from `s` (n bytes) continuing `st`. Returns bytes used for a complete character,
/// 0 for NUL, -2 for incomplete, -1 for invalid.
unsafe fn utf8_decode(st: &mut MbState, s: *const u8, n: usize, out: &mut u32) -> isize {
    let mut i = 0usize;
    let (mut need, mut v) = (st.count, st.value);
    if need == 0 {
        if n == 0 {
            return -2;
        }
        let c = *s;
        i = 1;
        if c < 0x80 {
            *out = c as u32;
            return if c == 0 { 0 } else { 1 };
        } else if c & 0xe0 == 0xc0 && c >= 0xc2 {
            need = 1;
            v = (c & 0x1f) as u32;
        } else if c & 0xf0 == 0xe0 {
            need = 2;
            v = (c & 0x0f) as u32;
        } else if c & 0xf8 == 0xf0 && c <= 0xf4 {
            need = 3;
            v = (c & 0x07) as u32;
        } else {
            return -1;
        }
    }
    while need > 0 {
        if i >= n {
            st.count = need;
            st.value = v;
            return -2;
        }
        let c = *s.add(i);
        if c & 0xc0 != 0x80 {
            st.count = 0;
            return -1;
        }
        v = (v << 6) | (c & 0x3f) as u32;
        need -= 1;
        i += 1;
    }
    st.count = 0;
    st.value = 0;
    if v > 0x10ffff || (0xd800..0xe000).contains(&v) {
        return -1;
    }
    *out = v;
    if v == 0 { 0 } else { i as isize }
}

pub fn utf8_encode(c: u32, out: &mut [u8; 4]) -> usize {
    if c < 0x80 {
        out[0] = c as u8;
        1
    } else if c < 0x800 {
        out[0] = 0xc0 | (c >> 6) as u8;
        out[1] = 0x80 | (c & 0x3f) as u8;
        2
    } else if c < 0x10000 {
        out[0] = 0xe0 | (c >> 12) as u8;
        out[1] = 0x80 | ((c >> 6) & 0x3f) as u8;
        out[2] = 0x80 | (c & 0x3f) as u8;
        3
    } else {
        out[0] = 0xf0 | (c >> 18) as u8;
        out[1] = 0x80 | ((c >> 12) & 0x3f) as u8;
        out[2] = 0x80 | ((c >> 6) & 0x3f) as u8;
        out[3] = 0x80 | (c & 0x3f) as u8;
        4
    }
}

#[no_mangle]
pub unsafe extern "C" fn mbrtowc(pwc: *mut wchar_t, s: *const c_char, n: size_t, ps: *mut MbState) -> size_t {
    let st = if ps.is_null() { &mut (*ptr::addr_of_mut!(INTERNAL_STATE))[0] } else { &mut *ps };
    if s.is_null() {
        st.count = 0;
        st.value = 0;
        return 0;
    }
    let mut c = 0;
    match utf8_decode(st, s, n, &mut c) {
        -1 => {
            fl::errno::set(EILSEQ);
            usize::MAX
        }
        -2 => usize::MAX - 1,
        r => {
            if !pwc.is_null() {
                *pwc = c as wchar_t;
            }
            r as size_t
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn mbrlen(s: *const c_char, n: size_t, ps: *mut MbState) -> size_t {
    let st = if ps.is_null() { ptr::addr_of_mut!(INTERNAL_STATE[1]) } else { ps };
    mbrtowc(ptr::null_mut(), s, n, st)
}

#[no_mangle]
pub unsafe extern "C" fn __mbrlen(s: *const c_char, n: size_t, ps: *mut MbState) -> size_t {
    mbrlen(s, n, ps)
}

#[no_mangle]
pub unsafe extern "C" fn mbtowc(pwc: *mut wchar_t, s: *const c_char, n: size_t) -> c_int {
    if s.is_null() {
        return 0;
    }
    let mut st = MbState { count: 0, value: 0 };
    let r = mbrtowc(pwc, s, n, &mut st);
    if r >= usize::MAX - 1 {
        fl::errno::set(EILSEQ);
        -1
    } else {
        r as c_int
    }
}

#[no_mangle]
pub unsafe extern "C" fn mblen(s: *const c_char, n: size_t) -> c_int {
    mbtowc(ptr::null_mut(), s, n)
}

#[no_mangle]
pub unsafe extern "C" fn mbsinit(ps: *const MbState) -> c_int {
    (ps.is_null() || (*ps).count == 0) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn wcrtomb(s: *mut c_char, wc: wchar_t, _ps: *mut MbState) -> size_t {
    if s.is_null() {
        return 1;
    }
    let c = wc as u32;
    if c > 0x10ffff || (0xd800..0xe000).contains(&c) {
        fl::errno::set(EILSEQ);
        return usize::MAX;
    }
    let mut b = [0u8; 4];
    let n = utf8_encode(c, &mut b);
    ptr::copy_nonoverlapping(b.as_ptr(), s, n);
    n
}

#[no_mangle]
pub unsafe extern "C" fn wctomb(s: *mut c_char, wc: wchar_t) -> c_int {
    if s.is_null() {
        return 0;
    }
    wcrtomb(s, wc, ptr::null_mut()) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn btowc(c: c_int) -> wint_t {
    if (0..128).contains(&c) { c as wint_t } else { WEOF }
}

#[no_mangle]
pub unsafe extern "C" fn wctob(c: wint_t) -> c_int {
    if c < 128 { c as c_int } else { -1 }
}

#[no_mangle]
pub unsafe extern "C" fn mbsnrtowcs(dst: *mut wchar_t, src: *mut *const c_char, nms: size_t, len: size_t, ps: *mut MbState) -> size_t {
    let st = if ps.is_null() { &mut (*ptr::addr_of_mut!(INTERNAL_STATE))[2] } else { &mut *ps };
    let mut s = *src;
    let end = s.wrapping_add(nms);
    let mut n = 0usize;
    while dst.is_null() || n < len {
        let avail = (end as usize).saturating_sub(s as usize);
        let mut c = 0;
        let r = utf8_decode(st, s, avail, &mut c);
        match r {
            -1 => {
                fl::errno::set(EILSEQ);
                if !dst.is_null() {
                    *src = s;
                }
                return usize::MAX;
            }
            -2 => {
                s = end;
                break;
            }
            0 => {
                if !dst.is_null() {
                    *dst.add(n) = 0;
                    *src = ptr::null();
                }
                return n;
            }
            r => {
                if !dst.is_null() {
                    *dst.add(n) = c as wchar_t;
                }
                s = s.add(r as usize);
                n += 1;
            }
        }
    }
    if !dst.is_null() {
        *src = s;
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn mbsrtowcs(dst: *mut wchar_t, src: *mut *const c_char, len: size_t, ps: *mut MbState) -> size_t {
    mbsnrtowcs(dst, src, usize::MAX / 2, len, ps)
}

#[no_mangle]
pub unsafe extern "C" fn mbstowcs(dst: *mut wchar_t, src: *const c_char, len: size_t) -> size_t {
    let mut s = src;
    let mut st = MbState { count: 0, value: 0 };
    mbsrtowcs(dst, &mut s, len, &mut st)
}

#[no_mangle]
pub unsafe extern "C" fn wcsnrtombs(dst: *mut c_char, src: *mut *const wchar_t, nwc: size_t, len: size_t, _ps: *mut MbState) -> size_t {
    let mut s = *src;
    let mut n = 0usize;
    let mut i = 0usize;
    while i < nwc {
        let c = *s as u32;
        if c == 0 {
            if !dst.is_null() {
                if n < len {
                    *dst.add(n) = 0;
                }
                *src = ptr::null();
            }
            return n;
        }
        let mut b = [0u8; 4];
        if c > 0x10ffff || (0xd800..0xe000).contains(&c) {
            fl::errno::set(EILSEQ);
            if !dst.is_null() {
                *src = s;
            }
            return usize::MAX;
        }
        let k = utf8_encode(c, &mut b);
        if !dst.is_null() {
            if n + k > len {
                break;
            }
            ptr::copy_nonoverlapping(b.as_ptr(), dst.add(n), k);
        }
        n += k;
        s = s.add(1);
        i += 1;
    }
    if !dst.is_null() {
        *src = s;
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn wcsrtombs(dst: *mut c_char, src: *mut *const wchar_t, len: size_t, ps: *mut MbState) -> size_t {
    wcsnrtombs(dst, src, usize::MAX / 8, len, ps)
}

#[no_mangle]
pub unsafe extern "C" fn wcstombs(dst: *mut c_char, src: *const wchar_t, len: size_t) -> size_t {
    let mut s = src;
    wcsrtombs(dst, &mut s, len, ptr::null_mut())
}

// --- wide character classes ------------------------------------------------------------------

fn wch(c: wint_t) -> Option<char> {
    char::from_u32(c)
}

macro_rules! iswfn {
    ($name:ident, $name_l:ident, $f:expr) => {
        #[no_mangle]
        pub extern "C" fn $name(c: wint_t) -> c_int {
            wch(c).map_or(false, $f) as c_int
        }
        #[no_mangle]
        pub extern "C" fn $name_l(c: wint_t, _l: *mut c_void) -> c_int {
            $name(c)
        }
    };
}
iswfn!(iswalpha, iswalpha_l, |c: char| c.is_alphabetic());
iswfn!(iswupper, iswupper_l, |c: char| c.is_uppercase());
iswfn!(iswlower, iswlower_l, |c: char| c.is_lowercase());
iswfn!(iswdigit, iswdigit_l, |c: char| c.is_ascii_digit());
iswfn!(iswxdigit, iswxdigit_l, |c: char| c.is_ascii_hexdigit());
iswfn!(iswspace, iswspace_l, |c: char| c.is_whitespace() && c != '\u{a0}' && c != '\u{2007}' && c != '\u{202f}');
iswfn!(iswblank, iswblank_l, |c: char| c == ' ' || c == '\t' || (c.is_whitespace() && !matches!(c, '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}')));
iswfn!(iswcntrl, iswcntrl_l, |c: char| c.is_control() || c == '\u{2028}' || c == '\u{2029}');
iswfn!(iswalnum, iswalnum_l, |c: char| c.is_alphanumeric());
iswfn!(iswpunct, iswpunct_l, |c: char| !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control() && wcwidth(c as wchar_t) > 0);
iswfn!(iswgraph, iswgraph_l, |c: char| !c.is_whitespace() && !c.is_control() && wcwidth(c as wchar_t) >= 0);
iswfn!(iswprint, iswprint_l, |c: char| !c.is_control() && wcwidth(c as wchar_t) >= 0);

#[no_mangle]
pub extern "C" fn towupper(c: wint_t) -> wint_t {
    match wch(c) {
        Some(ch) => {
            let mut u = ch.to_uppercase();
            match (u.next(), u.next()) {
                (Some(x), None) => x as wint_t,
                _ => c,
            }
        }
        None => c,
    }
}

#[no_mangle]
pub extern "C" fn towlower(c: wint_t) -> wint_t {
    match wch(c) {
        Some(ch) => {
            let mut u = ch.to_lowercase();
            match (u.next(), u.next()) {
                (Some(x), None) => x as wint_t,
                _ => c,
            }
        }
        None => c,
    }
}

#[no_mangle]
pub extern "C" fn towupper_l(c: wint_t, _l: *mut c_void) -> wint_t {
    towupper(c)
}
#[no_mangle]
pub extern "C" fn towlower_l(c: wint_t, _l: *mut c_void) -> wint_t {
    towlower(c)
}
#[no_mangle]
pub extern "C" fn __towupper_l(c: wint_t, _l: *mut c_void) -> wint_t {
    towupper(c)
}
#[no_mangle]
pub extern "C" fn __towlower_l(c: wint_t, _l: *mut c_void) -> wint_t {
    towlower(c)
}

const WCTYPE_NAMES: [&[u8]; 12] = [
    b"alnum", b"alpha", b"blank", b"cntrl", b"digit", b"graph", b"lower", b"print", b"punct", b"space", b"upper", b"xdigit",
];

#[no_mangle]
pub unsafe extern "C" fn wctype(name: *const c_char) -> usize {
    let n = fl::mem::cstr(name);
    WCTYPE_NAMES.iter().position(|&x| x == n).map_or(0, |i| i + 1)
}

#[no_mangle]
pub unsafe extern "C" fn wctype_l(name: *const c_char, _l: *mut c_void) -> usize {
    wctype(name)
}

#[no_mangle]
pub unsafe extern "C" fn __wctype_l(name: *const c_char, _l: *mut c_void) -> usize {
    wctype(name)
}

#[no_mangle]
pub extern "C" fn iswctype(c: wint_t, t: usize) -> c_int {
    match t {
        1 => iswalnum(c),
        2 => iswalpha(c),
        3 => iswblank(c),
        4 => iswcntrl(c),
        5 => iswdigit(c),
        6 => iswgraph(c),
        7 => iswlower(c),
        8 => iswprint(c),
        9 => iswpunct(c),
        10 => iswspace(c),
        11 => iswupper(c),
        12 => iswxdigit(c),
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn iswctype_l(c: wint_t, t: usize, _l: *mut c_void) -> c_int {
    iswctype(c, t)
}

#[no_mangle]
pub extern "C" fn __iswctype_l(c: wint_t, t: usize, _l: *mut c_void) -> c_int {
    iswctype(c, t)
}

#[no_mangle]
pub unsafe extern "C" fn wctrans(name: *const c_char) -> usize {
    match fl::mem::cstr(name) {
        b"toupper" => 1,
        b"tolower" => 2,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn towctrans(c: wint_t, t: usize) -> wint_t {
    match t {
        1 => towupper(c),
        2 => towlower(c),
        _ => c,
    }
}

/// Zero-width (combining) and double-width (East Asian wide) ranges, after Markus Kuhn's
/// wcwidth, updated for emoji.
static ZERO: &[(u32, u32)] = &[
    (0x0300, 0x036F), (0x0483, 0x0489), (0x0591, 0x05BD), (0x05BF, 0x05BF), (0x05C1, 0x05C2),
    (0x05C4, 0x05C5), (0x05C7, 0x05C7), (0x0610, 0x061A), (0x064B, 0x065F), (0x0670, 0x0670),
    (0x06D6, 0x06DC), (0x06DF, 0x06E4), (0x06E7, 0x06E8), (0x06EA, 0x06ED), (0x0711, 0x0711),
    (0x0730, 0x074A), (0x07A6, 0x07B0), (0x07EB, 0x07F3), (0x0816, 0x0819), (0x081B, 0x0823),
    (0x0825, 0x0827), (0x0829, 0x082D), (0x0859, 0x085B), (0x08D3, 0x08E1), (0x08E3, 0x0902),
    (0x093A, 0x093A), (0x093C, 0x093C), (0x0941, 0x0948), (0x094D, 0x094D), (0x0951, 0x0957),
    (0x0962, 0x0963), (0x0981, 0x0981), (0x09BC, 0x09BC), (0x09C1, 0x09C4), (0x09CD, 0x09CD),
    (0x09E2, 0x09E3), (0x0A01, 0x0A02), (0x0A3C, 0x0A3C), (0x0A41, 0x0A51), (0x0A70, 0x0A71),
    (0x0A75, 0x0A75), (0x0A81, 0x0A82), (0x0ABC, 0x0ABC), (0x0AC1, 0x0AC8), (0x0ACD, 0x0ACD),
    (0x0B01, 0x0B01), (0x0B3C, 0x0B3C), (0x0B3F, 0x0B3F), (0x0B41, 0x0B44), (0x0B4D, 0x0B4D),
    (0x0BC0, 0x0BC0), (0x0BCD, 0x0BCD), (0x0C3E, 0x0C40), (0x0C46, 0x0C56), (0x0CBC, 0x0CBC),
    (0x0CCC, 0x0CCD), (0x0D41, 0x0D44), (0x0D4D, 0x0D4D), (0x0DCA, 0x0DCA), (0x0DD2, 0x0DD6),
    (0x0E31, 0x0E31), (0x0E34, 0x0E3A), (0x0E47, 0x0E4E), (0x0EB1, 0x0EB1), (0x0EB4, 0x0EBC),
    (0x0EC8, 0x0ECD), (0x0F18, 0x0F19), (0x0F35, 0x0F35), (0x0F37, 0x0F37), (0x0F39, 0x0F39),
    (0x0F71, 0x0F7E), (0x0F80, 0x0F84), (0x0F86, 0x0F87), (0x0F8D, 0x0FBC), (0x0FC6, 0x0FC6),
    (0x102D, 0x1030), (0x1032, 0x1037), (0x1039, 0x103A), (0x1160, 0x11FF), (0x135D, 0x135F),
    (0x1712, 0x1714), (0x1732, 0x1734), (0x1752, 0x1753), (0x1772, 0x1773), (0x17B4, 0x17B5),
    (0x17B7, 0x17BD), (0x17C6, 0x17C6), (0x17C9, 0x17D3), (0x180B, 0x180E), (0x18A9, 0x18A9),
    (0x1920, 0x1922), (0x1927, 0x1928), (0x1932, 0x1932), (0x1939, 0x193B), (0x1A17, 0x1A18),
    (0x1AB0, 0x1AFF), (0x1B00, 0x1B03), (0x1B34, 0x1B34), (0x1B36, 0x1B3A), (0x1DC0, 0x1DFF),
    (0x200B, 0x200F), (0x202A, 0x202E), (0x2060, 0x2064), (0x20D0, 0x20F0), (0x2CEF, 0x2CF1),
    (0x2DE0, 0x2DFF), (0x302A, 0x302D), (0x3099, 0x309A), (0xA66F, 0xA672), (0xA674, 0xA67D),
    (0xA69E, 0xA69F), (0xA6F0, 0xA6F1), (0xA802, 0xA802), (0xA806, 0xA806), (0xA80B, 0xA80B),
    (0xA825, 0xA826), (0xFB1E, 0xFB1E), (0xFE00, 0xFE0F), (0xFE20, 0xFE2F), (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB), (0x1D167, 0x1D169), (0x1D173, 0x1D182), (0x1D185, 0x1D18B),
    (0x1D1AA, 0x1D1AD), (0xE0001, 0xE0001), (0xE0020, 0xE007F), (0xE0100, 0xE01EF),
];

static WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115F), (0x231A, 0x231B), (0x2329, 0x232A), (0x23E9, 0x23EC), (0x23F0, 0x23F0),
    (0x23F3, 0x23F3), (0x25FD, 0x25FE), (0x2614, 0x2615), (0x2648, 0x2653), (0x267F, 0x267F),
    (0x2693, 0x2693), (0x26A1, 0x26A1), (0x26AA, 0x26AB), (0x26BD, 0x26BE), (0x26C4, 0x26C5),
    (0x26CE, 0x26CE), (0x26D4, 0x26D4), (0x26EA, 0x26EA), (0x26F2, 0x26F3), (0x26F5, 0x26F5),
    (0x26FA, 0x26FA), (0x26FD, 0x26FD), (0x2705, 0x2705), (0x270A, 0x270B), (0x2728, 0x2728),
    (0x274C, 0x274C), (0x274E, 0x274E), (0x2753, 0x2755), (0x2757, 0x2757), (0x2795, 0x2797),
    (0x27B0, 0x27B0), (0x27BF, 0x27BF), (0x2B1B, 0x2B1C), (0x2B50, 0x2B50), (0x2B55, 0x2B55),
    (0x2E80, 0x303E), (0x3041, 0x33FF), (0x3400, 0x4DBF), (0x4E00, 0x9FFF), (0xA000, 0xA4CF),
    (0xA960, 0xA97F), (0xAC00, 0xD7A3), (0xF900, 0xFAFF), (0xFE10, 0xFE19), (0xFE30, 0xFE6F),
    (0xFF00, 0xFF60), (0xFFE0, 0xFFE6), (0x16FE0, 0x16FE4), (0x17000, 0x18CFF), (0x1B000, 0x1B2FF),
    (0x1F004, 0x1F004), (0x1F0CF, 0x1F0CF), (0x1F18E, 0x1F18E), (0x1F191, 0x1F19A),
    (0x1F200, 0x1F251), (0x1F260, 0x1F265), (0x1F300, 0x1F64F), (0x1F680, 0x1F6FF),
    (0x1F7E0, 0x1F7EB), (0x1F90C, 0x1F9FF), (0x1FA70, 0x1FAFF), (0x20000, 0x2FFFD),
    (0x30000, 0x3FFFD),
];

fn in_ranges(c: u32, t: &[(u32, u32)]) -> bool {
    t.binary_search_by(|&(lo, hi)| {
        if hi < c {
            core::cmp::Ordering::Less
        } else if lo > c {
            core::cmp::Ordering::Greater
        } else {
            core::cmp::Ordering::Equal
        }
    })
    .is_ok()
}

#[no_mangle]
pub extern "C" fn wcwidth(c: wchar_t) -> c_int {
    let c = c as u32;
    if c == 0 {
        return 0;
    }
    if c < 32 || (0x7f..0xa0).contains(&c) {
        return -1;
    }
    if c > 0x10ffff || (0xd800..0xe000).contains(&c) {
        return -1;
    }
    if in_ranges(c, ZERO) {
        return 0;
    }
    if in_ranges(c, WIDE) {
        return 2;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn wcswidth(s: *const wchar_t, n: size_t) -> c_int {
    let mut w = 0;
    for i in 0..n {
        let c = *s.add(i);
        if c == 0 {
            break;
        }
        let x = wcwidth(c);
        if x < 0 {
            return -1;
        }
        w += x;
    }
    w
}

// --- wide strings ----------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn wcslen(s: *const wchar_t) -> size_t {
    let mut i = 0;
    while *s.add(i) != 0 {
        i += 1;
    }
    i
}
#[no_mangle]
pub unsafe extern "C" fn wcsnlen(s: *const wchar_t, n: size_t) -> size_t {
    let mut i = 0;
    while i < n && *s.add(i) != 0 {
        i += 1;
    }
    i
}
#[no_mangle]
pub unsafe extern "C" fn wcscpy(d: *mut wchar_t, s: *const wchar_t) -> *mut wchar_t {
    ptr::copy_nonoverlapping(s, d, wcslen(s) + 1);
    d
}
#[no_mangle]
pub unsafe extern "C" fn wcpcpy(d: *mut wchar_t, s: *const wchar_t) -> *mut wchar_t {
    let n = wcslen(s);
    ptr::copy_nonoverlapping(s, d, n + 1);
    d.add(n)
}
#[no_mangle]
pub unsafe extern "C" fn wcsncpy(d: *mut wchar_t, s: *const wchar_t, n: size_t) -> *mut wchar_t {
    let l = wcsnlen(s, n);
    ptr::copy_nonoverlapping(s, d, l);
    for i in l..n {
        *d.add(i) = 0;
    }
    d
}
#[no_mangle]
pub unsafe extern "C" fn wcscat(d: *mut wchar_t, s: *const wchar_t) -> *mut wchar_t {
    wcscpy(d.add(wcslen(d)), s);
    d
}
#[no_mangle]
pub unsafe extern "C" fn wcsncat(d: *mut wchar_t, s: *const wchar_t, n: size_t) -> *mut wchar_t {
    let dl = wcslen(d);
    let l = wcsnlen(s, n);
    ptr::copy_nonoverlapping(s, d.add(dl), l);
    *d.add(dl + l) = 0;
    d
}
#[no_mangle]
pub unsafe extern "C" fn wcscmp(a: *const wchar_t, b: *const wchar_t) -> c_int {
    let mut i = 0;
    loop {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y {
            return if x < y { -1 } else { 1 };
        }
        if x == 0 {
            return 0;
        }
        i += 1;
    }
}
#[no_mangle]
pub unsafe extern "C" fn wcsncmp(a: *const wchar_t, b: *const wchar_t, n: size_t) -> c_int {
    for i in 0..n {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y {
            return if x < y { -1 } else { 1 };
        }
        if x == 0 {
            return 0;
        }
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn wcscasecmp(a: *const wchar_t, b: *const wchar_t) -> c_int {
    let mut i = 0;
    loop {
        let (x, y) = (towlower(*a.add(i) as u32), towlower(*b.add(i) as u32));
        if x != y {
            return if x < y { -1 } else { 1 };
        }
        if x == 0 {
            return 0;
        }
        i += 1;
    }
}
#[no_mangle]
pub unsafe extern "C" fn wcscoll(a: *const wchar_t, b: *const wchar_t) -> c_int {
    wcscmp(a, b)
}
#[no_mangle]
pub unsafe extern "C" fn wcscoll_l(a: *const wchar_t, b: *const wchar_t, _l: *mut c_void) -> c_int {
    wcscmp(a, b)
}
#[no_mangle]
pub unsafe extern "C" fn __wcscoll_l(a: *const wchar_t, b: *const wchar_t, _l: *mut c_void) -> c_int {
    wcscmp(a, b)
}
#[no_mangle]
pub unsafe extern "C" fn wcsxfrm(d: *mut wchar_t, s: *const wchar_t, n: size_t) -> size_t {
    let l = wcslen(s);
    if n > l {
        ptr::copy_nonoverlapping(s, d, l + 1);
    }
    l
}
#[no_mangle]
pub unsafe extern "C" fn wcsxfrm_l(d: *mut wchar_t, s: *const wchar_t, n: size_t, _l: *mut c_void) -> size_t {
    wcsxfrm(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn __wcsxfrm_l(d: *mut wchar_t, s: *const wchar_t, n: size_t, _l: *mut c_void) -> size_t {
    wcsxfrm(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn wcschr(s: *const wchar_t, c: wchar_t) -> *mut wchar_t {
    let mut p = s;
    loop {
        if *p == c {
            return p as *mut wchar_t;
        }
        if *p == 0 {
            return ptr::null_mut();
        }
        p = p.add(1);
    }
}
#[no_mangle]
pub unsafe extern "C" fn wcsrchr(s: *const wchar_t, c: wchar_t) -> *mut wchar_t {
    let mut last = ptr::null_mut();
    let mut p = s;
    loop {
        if *p == c {
            last = p as *mut wchar_t;
        }
        if *p == 0 {
            return last;
        }
        p = p.add(1);
    }
}
#[no_mangle]
pub unsafe extern "C" fn wcsstr(h: *const wchar_t, n: *const wchar_t) -> *mut wchar_t {
    let nl = wcslen(n);
    let mut p = h;
    loop {
        if wcsncmp(p, n, nl) == 0 {
            return p as *mut wchar_t;
        }
        if *p == 0 {
            return ptr::null_mut();
        }
        p = p.add(1);
    }
}
#[no_mangle]
pub unsafe extern "C" fn wcsspn(s: *const wchar_t, set: *const wchar_t) -> size_t {
    let mut i = 0;
    while *s.add(i) != 0 && !wcschr(set, *s.add(i)).is_null() {
        i += 1;
    }
    i
}
#[no_mangle]
pub unsafe extern "C" fn wcscspn(s: *const wchar_t, set: *const wchar_t) -> size_t {
    let mut i = 0;
    while *s.add(i) != 0 && wcschr(set, *s.add(i)).is_null() {
        i += 1;
    }
    i
}
#[no_mangle]
pub unsafe extern "C" fn wcspbrk(s: *const wchar_t, set: *const wchar_t) -> *mut wchar_t {
    let p = s.add(wcscspn(s, set));
    if *p == 0 { ptr::null_mut() } else { p as *mut wchar_t }
}
#[no_mangle]
pub unsafe extern "C" fn wcsdup(s: *const wchar_t) -> *mut wchar_t {
    let n = wcslen(s) + 1;
    let d = umalloc(n * 4) as *mut wchar_t;
    if !d.is_null() {
        ptr::copy_nonoverlapping(s, d, n);
    }
    d
}
#[no_mangle]
pub unsafe extern "C" fn wmemchr(s: *const wchar_t, c: wchar_t, n: size_t) -> *mut wchar_t {
    for i in 0..n {
        if *s.add(i) == c {
            return s.add(i) as *mut wchar_t;
        }
    }
    ptr::null_mut()
}
#[no_mangle]
pub unsafe extern "C" fn wmemcmp(a: *const wchar_t, b: *const wchar_t, n: size_t) -> c_int {
    for i in 0..n {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn wmemcpy(d: *mut wchar_t, s: *const wchar_t, n: size_t) -> *mut wchar_t {
    ptr::copy_nonoverlapping(s, d, n);
    d
}
#[no_mangle]
pub unsafe extern "C" fn wmempcpy(d: *mut wchar_t, s: *const wchar_t, n: size_t) -> *mut wchar_t {
    ptr::copy_nonoverlapping(s, d, n);
    d.add(n)
}
#[no_mangle]
pub unsafe extern "C" fn wmemmove(d: *mut wchar_t, s: *const wchar_t, n: size_t) -> *mut wchar_t {
    ptr::copy(s, d, n);
    d
}
#[no_mangle]
pub unsafe extern "C" fn wmemset(d: *mut wchar_t, c: wchar_t, n: size_t) -> *mut wchar_t {
    for i in 0..n {
        *d.add(i) = c;
    }
    d
}

// --- iconv -------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Enc {
    Utf8,
    Ascii,
    Latin1,
    Cp1252,
    Utf16Le,
    Utf16Be,
    Utf16, // with BOM; big-endian unless the input's BOM says otherwise
    Utf32Le,
    Utf32Be,
    Utf32,
}

struct Iconv {
    from: Enc,
    to: Enc,
    translit: bool,
    ignore: bool,
    wrote_bom: bool,
    from_le: Option<bool>, // detected byte order for Utf16/Utf32 input
}

fn parse_enc(name: &[u8]) -> Option<(Enc, bool, bool)> {
    let mut base: Vec<u8> = Vec::new();
    let (mut translit, mut ignore) = (false, false);
    let mut parts = name.split(|&c| c == b'/');
    if let Some(b) = parts.next() {
        base = b.iter().map(|c| c.to_ascii_uppercase()).filter(|&c| c != b'-' && c != b'_').collect();
    }
    for p in parts {
        let p: Vec<u8> = p.iter().map(|c| c.to_ascii_uppercase()).collect();
        if p.starts_with(b"TRANSLIT") {
            translit = true;
        }
        if p.starts_with(b"IGNORE") {
            ignore = true;
        }
    }
    let e = match &base[..] {
        b"UTF8" | b"" => Enc::Utf8,
        b"ASCII" | b"USASCII" | b"ANSIX3.41968" | b"646" | b"US" => Enc::Ascii,
        b"ISO88591" | b"LATIN1" | b"L1" | b"ISO885915" | b"LATIN9" => Enc::Latin1,
        b"CP1252" | b"WINDOWS1252" => Enc::Cp1252,
        b"UTF16LE" | b"UCS2LE" => Enc::Utf16Le,
        b"UTF16BE" | b"UCS2BE" => Enc::Utf16Be,
        b"UTF16" | b"UCS2" => Enc::Utf16,
        b"UTF32LE" | b"UCS4LE" | b"WCHART" => Enc::Utf32Le,
        b"UTF32BE" | b"UCS4BE" | b"UCS4" => Enc::Utf32Be,
        b"UTF32" => Enc::Utf32,
        _ => return None,
    };
    Some((e, translit, ignore))
}

static CP1252_HIGH: [u16; 32] = [
    0x20AC, 0xFFFD, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0xFFFD,
    0x017D, 0xFFFD, 0xFFFD, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A,
    0x0153, 0xFFFD, 0x017E, 0x0178,
];

#[no_mangle]
pub unsafe extern "C" fn iconv_open(to: *const c_char, from: *const c_char) -> *mut c_void {
    let (t, f) = (parse_enc(fl::mem::cstr(to)), parse_enc(fl::mem::cstr(from)));
    match (t, f) {
        (Some((to, translit, ignore)), Some((from, _, _))) => {
            let b = alloc::boxed::Box::new(Iconv { from, to, translit, ignore, wrote_bom: false, from_le: None });
            alloc::boxed::Box::into_raw(b) as *mut c_void
        }
        _ => {
            fl::errno::set(fl::errno::EINVAL);
            usize::MAX as *mut c_void
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn iconv_close(cd: *mut c_void) -> c_int {
    if !cd.is_null() && cd as usize != usize::MAX {
        drop(alloc::boxed::Box::from_raw(cd as *mut Iconv));
    }
    0
}

/// Decode one character: Ok(Some((char, bytes))), Ok(None) for incomplete input, Err for
/// invalid.
unsafe fn dec(cv: &mut Iconv, s: &[u8]) -> Result<Option<(u32, usize)>, ()> {
    match cv.from {
        Enc::Utf8 => {
            let mut st = MbState { count: 0, value: 0 };
            let mut c = 0;
            match utf8_decode(&mut st, s.as_ptr(), s.len(), &mut c) {
                -1 => Err(()),
                -2 => Ok(None),
                0 => Ok(Some((0, 1))),
                n => Ok(Some((c, n as usize))),
            }
        }
        Enc::Ascii => {
            if s[0] < 128 { Ok(Some((s[0] as u32, 1))) } else { Err(()) }
        }
        Enc::Latin1 => Ok(Some((s[0] as u32, 1))),
        Enc::Cp1252 => {
            let c = s[0];
            if (0x80..0xa0).contains(&c) {
                let u = CP1252_HIGH[(c - 0x80) as usize];
                if u == 0xFFFD { Err(()) } else { Ok(Some((u as u32, 1))) }
            } else {
                Ok(Some((c as u32, 1)))
            }
        }
        Enc::Utf16Le | Enc::Utf16Be | Enc::Utf16 => {
            if s.len() < 2 {
                return Ok(None);
            }
            let mut le = match cv.from {
                Enc::Utf16Le => true,
                Enc::Utf16Be => false,
                _ => cv.from_le.unwrap_or(false),
            };
            let mut skip = 0;
            if cv.from == Enc::Utf16 && cv.from_le.is_none() {
                if s[0] == 0xff && s[1] == 0xfe {
                    le = true;
                    skip = 2;
                } else if s[0] == 0xfe && s[1] == 0xff {
                    skip = 2;
                }
                cv.from_le = Some(le);
                if skip > 0 {
                    return Ok(Some((0xFEFF_FFFF, 2))); // a BOM: consumed, nothing written
                }
            }
            let w = |i: usize| if le { u16::from_le_bytes([s[i], s[i + 1]]) } else { u16::from_be_bytes([s[i], s[i + 1]]) };
            let a = w(0) as u32;
            if (0xd800..0xdc00).contains(&a) {
                if s.len() < 4 {
                    return Ok(None);
                }
                let b = w(2) as u32;
                if !(0xdc00..0xe000).contains(&b) {
                    return Err(());
                }
                Ok(Some((0x10000 + ((a - 0xd800) << 10) + (b - 0xdc00), 4)))
            } else if (0xdc00..0xe000).contains(&a) {
                Err(())
            } else {
                Ok(Some((a, 2)))
            }
        }
        Enc::Utf32Le | Enc::Utf32Be | Enc::Utf32 => {
            if s.len() < 4 {
                return Ok(None);
            }
            let mut le = match cv.from {
                Enc::Utf32Le => true,
                Enc::Utf32Be => false,
                _ => cv.from_le.unwrap_or(false),
            };
            if cv.from == Enc::Utf32 && cv.from_le.is_none() {
                if s[..4] == [0xff, 0xfe, 0, 0] {
                    le = true;
                    cv.from_le = Some(true);
                    return Ok(Some((0xFEFF_FFFF, 4)));
                }
                if s[..4] == [0, 0, 0xfe, 0xff] {
                    cv.from_le = Some(false);
                    return Ok(Some((0xFEFF_FFFF, 4)));
                }
                cv.from_le = Some(le);
            }
            let b = [s[0], s[1], s[2], s[3]];
            let c = if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) };
            if c > 0x10ffff || (0xd800..0xe000).contains(&c) {
                Err(())
            } else {
                Ok(Some((c, 4)))
            }
        }
    }
}

/// Encode one character into `out`; None when it cannot be represented.
fn enc(cv: &mut Iconv, c: u32, out: &mut [u8; 12]) -> Option<usize> {
    let mut n = 0;
    let bom = !cv.wrote_bom && matches!(cv.to, Enc::Utf16 | Enc::Utf32);
    match cv.to {
        Enc::Utf8 => {
            let mut b = [0u8; 4];
            let k = utf8_encode(c, &mut b);
            out[..k].copy_from_slice(&b[..k]);
            n = k;
        }
        Enc::Ascii => {
            if c >= 128 {
                return None;
            }
            out[0] = c as u8;
            n = 1;
        }
        Enc::Latin1 => {
            if c >= 256 {
                return None;
            }
            out[0] = c as u8;
            n = 1;
        }
        Enc::Cp1252 => {
            if c < 0x80 || (0xa0..0x100).contains(&c) {
                out[0] = c as u8;
                n = 1;
            } else if let Some(i) = CP1252_HIGH.iter().position(|&u| u as u32 == c && u != 0xFFFD) {
                out[0] = 0x80 + i as u8;
                n = 1;
            } else {
                return None;
            }
        }
        Enc::Utf16Le | Enc::Utf16Be | Enc::Utf16 => {
            let be = cv.to != Enc::Utf16Le;
            let mut put = |w: u16, n: &mut usize| {
                let b = if be { w.to_be_bytes() } else { w.to_le_bytes() };
                out[*n] = b[0];
                out[*n + 1] = b[1];
                *n += 2;
            };
            if bom {
                put(0xfeff, &mut n);
            }
            if c >= 0x10000 {
                let v = c - 0x10000;
                put(0xd800 + (v >> 10) as u16, &mut n);
                put(0xdc00 + (v & 0x3ff) as u16, &mut n);
            } else {
                put(c as u16, &mut n);
            }
        }
        Enc::Utf32Le | Enc::Utf32Be | Enc::Utf32 => {
            let be = cv.to != Enc::Utf32Le;
            let mut put = |w: u32, n: &mut usize| {
                let b = if be { w.to_be_bytes() } else { w.to_le_bytes() };
                out[*n..*n + 4].copy_from_slice(&b);
                *n += 4;
            };
            if bom {
                put(0xfeff, &mut n);
            }
            put(c, &mut n);
        }
    }
    if bom {
        cv.wrote_bom = true;
    }
    Some(n)
}

#[no_mangle]
pub unsafe extern "C" fn iconv(
    cd: *mut c_void,
    inbuf: *mut *mut c_char,
    inleft: *mut size_t,
    outbuf: *mut *mut c_char,
    outleft: *mut size_t,
) -> size_t {
    const E2BIG: c_int = 7;
    let cv = &mut *(cd as *mut Iconv);
    if inbuf.is_null() || (*inbuf).is_null() {
        // reset the shift state (there is none besides the BOM)
        return 0;
    }
    let mut irreversible = 0usize;
    while *inleft > 0 {
        let s = core::slice::from_raw_parts(*inbuf, *inleft);
        let (c, used) = match dec(cv, s) {
            Ok(Some(x)) => x,
            Ok(None) => {
                fl::errno::set(fl::errno::EINVAL);
                return usize::MAX;
            }
            Err(()) => {
                if cv.ignore {
                    *inbuf = (*inbuf).add(1);
                    *inleft -= 1;
                    irreversible += 1;
                    continue;
                }
                fl::errno::set(EILSEQ);
                return usize::MAX;
            }
        };
        if c == 0xFEFF_FFFF {
            *inbuf = (*inbuf).add(used);
            *inleft -= used;
            continue;
        }
        let mut buf = [0u8; 12];
        let n = match enc(cv, c, &mut buf) {
            Some(n) => n,
            None => {
                if cv.translit || cv.ignore {
                    irreversible += 1;
                    if cv.ignore && !cv.translit {
                        *inbuf = (*inbuf).add(used);
                        *inleft -= used;
                        continue;
                    }
                    buf[0] = b'?';
                    1
                } else {
                    fl::errno::set(EILSEQ);
                    return usize::MAX;
                }
            }
        };
        if n > *outleft {
            fl::errno::set(E2BIG);
            return usize::MAX;
        }
        ptr::copy_nonoverlapping(buf.as_ptr(), *outbuf, n);
        *outbuf = (*outbuf).add(n);
        *outleft -= n;
        *inbuf = (*inbuf).add(used);
        *inleft -= used;
    }
    irreversible
}

// --- gettext (no message catalogs: messages come out as written) ------------------------------

#[no_mangle]
pub unsafe extern "C" fn gettext(msg: *const c_char) -> *mut c_char {
    msg as *mut c_char
}
#[no_mangle]
pub unsafe extern "C" fn dgettext(_d: *const c_char, msg: *const c_char) -> *mut c_char {
    msg as *mut c_char
}
#[no_mangle]
pub unsafe extern "C" fn dcgettext(_d: *const c_char, msg: *const c_char, _c: c_int) -> *mut c_char {
    msg as *mut c_char
}
#[no_mangle]
pub unsafe extern "C" fn ngettext(a: *const c_char, b: *const c_char, n: u64) -> *mut c_char {
    (if n == 1 { a } else { b }) as *mut c_char
}
#[no_mangle]
pub unsafe extern "C" fn dngettext(_d: *const c_char, a: *const c_char, b: *const c_char, n: u64) -> *mut c_char {
    ngettext(a, b, n)
}
#[no_mangle]
pub unsafe extern "C" fn dcngettext(_d: *const c_char, a: *const c_char, b: *const c_char, n: u64, _c: c_int) -> *mut c_char {
    ngettext(a, b, n)
}
#[no_mangle]
pub unsafe extern "C" fn __dcgettext(d: *const c_char, msg: *const c_char, c: c_int) -> *mut c_char {
    dcgettext(d, msg, c)
}

static mut DOMAIN: [u8; 128] = *b"messages\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

#[no_mangle]
pub unsafe extern "C" fn textdomain(d: *const c_char) -> *mut c_char {
    let dom = &mut *ptr::addr_of_mut!(DOMAIN);
    if !d.is_null() {
        let s = fl::mem::cstr(d);
        let n = s.len().min(127);
        dom[..n].copy_from_slice(&s[..n]);
        dom[n] = 0;
    }
    dom.as_mut_ptr()
}
#[no_mangle]
pub unsafe extern "C" fn bindtextdomain(_d: *const c_char, dir: *const c_char) -> *mut c_char {
    if dir.is_null() { b"/usr/share/locale\0".as_ptr() as *mut c_char } else { dir as *mut c_char }
}
#[no_mangle]
pub unsafe extern "C" fn bind_textdomain_codeset(_d: *const c_char, cs: *const c_char) -> *mut c_char {
    if cs.is_null() { b"UTF-8\0".as_ptr() as *mut c_char } else { cs as *mut c_char }
}
