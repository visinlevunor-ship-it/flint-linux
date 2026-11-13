//! Assorted C library utilities: qsort, the random() family (glibc's generator, so seeded
//! sequences match), GNU getopt, basename/dirname, err/warn/error, syslog, mntent, obstack.

use super::printf::{format, RawVa, Va, VecSink};
use super::stdio::{self, FILE};
use crate::fl;
use alloc::vec::Vec;
use core::ffi::{c_void, VaList};
use core::ptr;

type c_char = u8;
type c_int = i32;
type c_long = i64;
type size_t = usize;

// --- qsort -------------------------------------------------------------------------------------

/// Stable merge sort, as glibc's qsort is when it can allocate (programs come to rely on it);
/// heapsort in place when it cannot.
unsafe fn sort(base: *mut u8, n: usize, size: usize, cmp: &mut dyn FnMut(*const u8, *const u8) -> c_int) {
    if n < 2 || size == 0 {
        return;
    }
    let total = match n.checked_mul(size) {
        Some(t) => t,
        None => return,
    };
    let tmp = fl::malloc::imalloc(total);
    if tmp.is_null() {
        heapsort(base, n, size, cmp);
        return;
    }
    msort(base, n, size, tmp, cmp);
    fl::malloc::ifree(tmp);
}

unsafe fn msort(b: *mut u8, n: usize, size: usize, tmp: *mut u8, cmp: &mut dyn FnMut(*const u8, *const u8) -> c_int) {
    if n <= 1 {
        return;
    }
    if n <= 8 {
        // insertion sort
        for i in 1..n {
            let mut j = i;
            while j > 0 && cmp(b.add((j - 1) * size), b.add(j * size)) > 0 {
                swap(b.add((j - 1) * size), b.add(j * size), size);
                j -= 1;
            }
        }
        return;
    }
    let n1 = n / 2;
    let n2 = n - n1;
    let b2 = b.add(n1 * size);
    msort(b, n1, size, tmp, cmp);
    msort(b2, n2, size, tmp, cmp);
    let (mut i, mut j, mut k) = (0, 0, 0);
    while i < n1 && j < n2 {
        let a = b.add(i * size);
        let c = b2.add(j * size);
        if cmp(a, c) <= 0 {
            ptr::copy_nonoverlapping(a, tmp.add(k * size), size);
            i += 1;
        } else {
            ptr::copy_nonoverlapping(c, tmp.add(k * size), size);
            j += 1;
        }
        k += 1;
    }
    if i < n1 {
        ptr::copy_nonoverlapping(b.add(i * size), tmp.add(k * size), (n1 - i) * size);
        k += n1 - i;
    }
    // the rest of the second half is already in place
    ptr::copy_nonoverlapping(tmp, b, k * size);
}

unsafe fn swap(a: *mut u8, b: *mut u8, size: usize) {
    for i in 0..size {
        ptr::swap(a.add(i), b.add(i));
    }
}

unsafe fn heapsort(b: *mut u8, n: usize, size: usize, cmp: &mut dyn FnMut(*const u8, *const u8) -> c_int) {
    let at = |i: usize| b.add(i * size);
    let mut sift = |mut root: usize, end: usize, cmp: &mut dyn FnMut(*const u8, *const u8) -> c_int| loop {
        let mut child = 2 * root + 1;
        if child >= end {
            break;
        }
        if child + 1 < end && cmp(at(child), at(child + 1)) < 0 {
            child += 1;
        }
        if cmp(at(root), at(child)) >= 0 {
            break;
        }
        swap(at(root), at(child), size);
        root = child;
    };
    for i in (0..n / 2).rev() {
        sift(i, n, cmp);
    }
    for end in (1..n).rev() {
        swap(at(0), at(end), size);
        sift(0, end, cmp);
    }
}

#[no_mangle]
pub unsafe extern "C" fn qsort(base: *mut c_void, n: size_t, size: size_t, cmp: unsafe extern "C" fn(*const c_void, *const c_void) -> c_int) {
    sort(base as *mut u8, n, size, &mut |a, b| cmp(a as *const c_void, b as *const c_void));
}
#[no_mangle]
pub unsafe extern "C" fn qsort_r(
    base: *mut c_void,
    n: size_t,
    size: size_t,
    cmp: unsafe extern "C" fn(*const c_void, *const c_void, *mut c_void) -> c_int,
    arg: *mut c_void,
) {
    sort(base as *mut u8, n, size, &mut |a, b| cmp(a as *const c_void, b as *const c_void, arg));
}

// --- random --------------------------------------------------------------------------------------

/// glibc's struct random_data.
#[repr(C)]
pub struct RandomData {
    fptr: *mut i32,
    rptr: *mut i32,
    state: *mut i32,
    rand_type: c_int,
    rand_deg: c_int,
    rand_sep: c_int,
    end_ptr: *mut i32,
}

const TYPES: [(c_int, c_int); 5] = [(0, 0), (7, 3), (15, 1), (31, 3), (63, 1)]; // (degree, separation)
const MAX_TYPES: c_int = 5;

#[no_mangle]
pub unsafe extern "C" fn srandom_r(seed: u32, buf: *mut RandomData) -> c_int {
    let b = &mut *buf;
    if b.rand_type < 0 || b.rand_type >= MAX_TYPES {
        fl::errno::set(22);
        return -1;
    }
    let state = b.state;
    let seed = if seed == 0 { 1 } else { seed };
    *state = seed as i32;
    if b.rand_type == 0 {
        return 0;
    }
    let deg = b.rand_deg as usize;
    let mut word = seed as i32;
    for i in 1..deg {
        // 16807 * word % 2147483647 without overflow (Schrage)
        let hi = word / 127773;
        let lo = word % 127773;
        word = 16807 * lo - 2836 * hi;
        if word < 0 {
            word += 2147483647;
        }
        *state.add(i) = word;
    }
    b.fptr = state.add(b.rand_sep as usize);
    b.rptr = state;
    let mut discard = 0;
    for _ in 0..deg * 10 {
        random_r(buf, &mut discard);
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn random_r(buf: *mut RandomData, result: *mut i32) -> c_int {
    let b = &mut *buf;
    let state = b.state;
    if b.rand_type == 0 {
        let v = ((*state as u32).wrapping_mul(1103515245).wrapping_add(12345)) & 0x7fff_ffff;
        *state = v as i32;
        *result = v as i32;
        return 0;
    }
    let mut f = b.fptr;
    let mut r = b.rptr;
    let end = b.end_ptr;
    let v = (*f as u32).wrapping_add(*r as u32);
    *f = v as i32;
    *result = (v >> 1) as i32;
    f = f.add(1);
    if f >= end {
        f = state;
        r = r.add(1);
    } else {
        r = r.add(1);
        if r >= end {
            r = state;
        }
    }
    b.fptr = f;
    b.rptr = r;
    0
}

#[no_mangle]
pub unsafe extern "C" fn initstate_r(seed: u32, statebuf: *mut c_char, n: size_t, buf: *mut RandomData) -> c_int {
    let ty = if n >= 256 {
        4
    } else if n >= 128 {
        3
    } else if n >= 64 {
        2
    } else if n >= 32 {
        1
    } else if n >= 8 {
        0
    } else {
        fl::errno::set(22);
        return -1;
    };
    let b = &mut *buf;
    let (deg, sep) = TYPES[ty as usize];
    b.rand_type = ty;
    b.rand_deg = deg;
    b.rand_sep = sep;
    let state = (statebuf as *mut i32).add(1);
    b.state = state;
    b.end_ptr = state.add(deg as usize);
    srandom_r(seed, buf);
    // the type (and rear pointer) is recorded in the word before the state
    *state.sub(1) = if ty == 0 { 0 } else { (b.rptr.offset_from(state) as c_int) * MAX_TYPES + ty };
    0
}

#[no_mangle]
pub unsafe extern "C" fn setstate_r(statebuf: *mut c_char, buf: *mut RandomData) -> c_int {
    let b = &mut *buf;
    let new = (statebuf as *mut i32).add(1);
    let old = b.state;
    if b.rand_type == 0 {
        *old.sub(1) = 0;
    } else {
        *old.sub(1) = (b.rptr.offset_from(old) as c_int) * MAX_TYPES + b.rand_type;
    }
    let info = *new.sub(1);
    let ty = info % MAX_TYPES;
    if !(0..MAX_TYPES).contains(&ty) {
        fl::errno::set(22);
        return -1;
    }
    let (deg, sep) = TYPES[ty as usize];
    b.rand_type = ty;
    b.rand_deg = deg;
    b.rand_sep = sep;
    b.state = new;
    if ty != 0 {
        let rear = (info / MAX_TYPES) as usize;
        b.rptr = new.add(rear);
        b.fptr = new.add((rear + sep as usize) % deg as usize);
    }
    b.end_ptr = new.add(deg as usize);
    0
}

// the global generator: TYPE_3, as glibc starts
static mut RANDTBL: [i32; 32] = [
    3, -1726662223, 379960547, 1735697613, 1040273694, 1313901226, 1627687941, -179304937, -2073333483, 1780058412,
    -1989503057, -615974602, 344556628, 939512070, -1249116260, 1507946756, -812545463, 154635395, 1388815473,
    -1926676823, 525320961, -1009028674, 968117788, -123449607, 1284210865, 435012392, -2017506339, -911064859,
    -370259173, 1132637927, 1398500161, -205601318,
];
static mut UNSAFE: RandomData = RandomData {
    fptr: ptr::null_mut(),
    rptr: ptr::null_mut(),
    state: ptr::null_mut(),
    rand_type: 3,
    rand_deg: 31,
    rand_sep: 3,
    end_ptr: ptr::null_mut(),
};
static RLOCK: fl::lock::Lock = fl::lock::Lock::new();

unsafe fn global() -> *mut RandomData {
    let u = ptr::addr_of_mut!(UNSAFE);
    if (*u).state.is_null() {
        let t = ptr::addr_of_mut!(RANDTBL) as *mut i32;
        (*u).state = t.add(1);
        (*u).fptr = t.add(1 + 3);
        (*u).rptr = t.add(1);
        (*u).end_ptr = t.add(1 + 31);
    }
    u
}

#[no_mangle]
pub unsafe extern "C" fn random() -> c_long {
    let _g = RLOCK.guard();
    let mut r = 0;
    random_r(global(), &mut r);
    r as c_long
}
#[no_mangle]
pub unsafe extern "C" fn srandom(seed: u32) {
    let _g = RLOCK.guard();
    srandom_r(seed, global());
}
#[no_mangle]
pub unsafe extern "C" fn rand() -> c_int {
    random() as c_int
}
#[no_mangle]
pub unsafe extern "C" fn srand(seed: u32) {
    srandom(seed)
}
#[no_mangle]
pub unsafe extern "C" fn initstate(seed: u32, st: *mut c_char, n: size_t) -> *mut c_char {
    let _g = RLOCK.guard();
    let g = global();
    let old = (*g).state.sub(1) as *mut c_char;
    // record the old generator's position before switching
    if (*g).rand_type != 0 {
        *(*g).state.sub(1) = ((*g).rptr.offset_from((*g).state) as c_int) * MAX_TYPES + (*g).rand_type;
    }
    if initstate_r(seed, st, n, g) < 0 {
        return ptr::null_mut();
    }
    old
}
#[no_mangle]
pub unsafe extern "C" fn setstate(st: *mut c_char) -> *mut c_char {
    let _g = RLOCK.guard();
    let g = global();
    let old = (*g).state.sub(1) as *mut c_char;
    if setstate_r(st, g) < 0 {
        return ptr::null_mut();
    }
    old
}
#[no_mangle]
pub unsafe extern "C" fn rand_r(seed: *mut u32) -> c_int {
    let mut next = *seed;
    next = next.wrapping_mul(1103515245).wrapping_add(12345);
    let mut result = (next / 65536) % 2048;
    next = next.wrapping_mul(1103515245).wrapping_add(12345);
    result <<= 10;
    result ^= (next / 65536) % 1024;
    next = next.wrapping_mul(1103515245).wrapping_add(12345);
    result <<= 10;
    result ^= (next / 65536) % 1024;
    *seed = next;
    result as c_int
}

// --- getopt -------------------------------------------------------------------------------------

#[no_mangle]
pub static mut optarg: *mut c_char = ptr::null_mut();
#[no_mangle]
pub static mut optind: c_int = 1;
#[no_mangle]
pub static mut opterr: c_int = 1;
#[no_mangle]
pub static mut optopt: c_int = 0;

static mut OPTARG: *mut *mut c_char = ptr::addr_of_mut!(optarg);
static mut OPTIND: *mut c_int = ptr::addr_of_mut!(optind);
static mut OPTERR: *mut c_int = ptr::addr_of_mut!(opterr);
static mut OPTOPT: *mut c_int = ptr::addr_of_mut!(optopt);

pub unsafe fn bind(scope: &[*mut crate::loader::Dso]) {
    use super::start::program_copy;
    if let Some(a) = program_copy(scope, &[b"optarg"]) {
        OPTARG = a as *mut *mut c_char;
    }
    if let Some(a) = program_copy(scope, &[b"optind"]) {
        OPTIND = a as *mut c_int;
    }
    if let Some(a) = program_copy(scope, &[b"opterr"]) {
        OPTERR = a as *mut c_int;
    }
    if let Some(a) = program_copy(scope, &[b"optopt"]) {
        OPTOPT = a as *mut c_int;
    }
}

/// struct option
#[repr(C)]
pub struct LongOpt {
    name: *const c_char,
    has_arg: c_int,
    flag: *mut c_int,
    val: c_int,
}

#[derive(PartialEq, Clone, Copy)]
enum Ordering {
    Permute,
    RequireOrder,
    ReturnInOrder,
}

struct GetoptState {
    next: *const c_char,
    first_nonopt: c_int,
    last_nonopt: c_int,
    initialized: bool,
}
static mut GS: GetoptState = GetoptState { next: ptr::null(), first_nonopt: 1, last_nonopt: 1, initialized: false };

unsafe fn prog_name(argv: *const *mut c_char) -> &'static [u8] {
    if !argv.is_null() && !(*argv).is_null() {
        fl::mem::cstr(*argv)
    } else {
        b""
    }
}

static mut QUIET: bool = false;

unsafe fn complain(argv: *const *mut c_char, parts: &[&[u8]]) {
    // a leading ':' in the option string silences the messages
    if *OPTERR == 0 || QUIET {
        return;
    }
    let mut v: Vec<u8> = prog_name(argv).to_vec();
    v.extend_from_slice(b": ");
    for p in parts {
        v.extend_from_slice(p);
    }
    v.push(b'\n');
    let f = stdio::stderr_file();
    stdio::fwrite(v.as_ptr() as *const c_void, 1, v.len(), f);
}

/// Move the block of non-options [first_nonopt, last_nonopt) after the options that follow
/// it, up to optind.
unsafe fn exchange(argv: *mut *mut c_char) {
    let gs = &mut *ptr::addr_of_mut!(GS);
    let bottom = gs.first_nonopt as usize;
    let middle = gs.last_nonopt as usize;
    let top = *OPTIND as usize;
    // rotate argv[bottom..top] left by (middle - bottom)
    let s = core::slice::from_raw_parts_mut(argv.add(bottom), top - bottom);
    s.rotate_left(middle - bottom);
    gs.first_nonopt += (top - middle) as c_int;
    gs.last_nonopt = *OPTIND;
}

unsafe fn getopt_internal(
    argc: c_int,
    argv: *mut *mut c_char,
    optstring: *const c_char,
    longopts: *const LongOpt,
    longind: *mut c_int,
    long_only: bool,
) -> c_int {
    let gs = &mut *ptr::addr_of_mut!(GS);
    let mut os = fl::mem::cstr(optstring);
    if argc < 1 {
        return -1;
    }
    *OPTARG = ptr::null_mut();
    if *OPTIND == 0 || !gs.initialized {
        if *OPTIND == 0 {
            *OPTIND = 1;
        }
        gs.first_nonopt = *OPTIND;
        gs.last_nonopt = *OPTIND;
        gs.next = ptr::null();
        gs.initialized = true;
    }
    let mut ordering = if !fl::env::getenv(b"POSIXLY_CORRECT\0".as_ptr()).is_null() { Ordering::RequireOrder } else { Ordering::Permute };
    if os.first() == Some(&b'-') {
        ordering = Ordering::ReturnInOrder;
        os = &os[1..];
    } else if os.first() == Some(&b'+') {
        ordering = Ordering::RequireOrder;
        os = &os[1..];
    }
    let colon = os.first() == Some(&b':');
    QUIET = colon;
    let arg = |i: c_int| -> &'static [u8] { fl::mem::cstr(*argv.add(i as usize)) };
    let is_nonopt = |i: c_int| -> bool {
        let a = arg(i);
        a.first() != Some(&b'-') || a.len() == 1
    };

    if gs.next.is_null() || *gs.next == 0 {
        // advance to the next element
        if gs.last_nonopt > *OPTIND {
            gs.last_nonopt = *OPTIND;
        }
        if gs.first_nonopt > *OPTIND {
            gs.first_nonopt = *OPTIND;
        }
        if ordering == Ordering::Permute {
            if gs.first_nonopt != gs.last_nonopt && gs.last_nonopt != *OPTIND {
                exchange(argv);
            } else if gs.last_nonopt != *OPTIND {
                gs.first_nonopt = *OPTIND;
            }
            while *OPTIND < argc && is_nonopt(*OPTIND) {
                *OPTIND += 1;
            }
            gs.last_nonopt = *OPTIND;
        }
        if *OPTIND != argc && arg(*OPTIND) == b"--" {
            *OPTIND += 1;
            if gs.first_nonopt != gs.last_nonopt && gs.last_nonopt != *OPTIND {
                exchange(argv);
            } else if gs.first_nonopt == gs.last_nonopt {
                gs.first_nonopt = *OPTIND;
            }
            gs.last_nonopt = argc;
            *OPTIND = argc;
        }
        if *OPTIND == argc {
            if gs.first_nonopt != gs.last_nonopt {
                *OPTIND = gs.first_nonopt;
            }
            return -1;
        }
        if is_nonopt(*OPTIND) {
            if ordering == Ordering::RequireOrder {
                return -1;
            }
            *OPTARG = *argv.add(*OPTIND as usize);
            *OPTIND += 1;
            return 1;
        }
        // skip the dashes
        let a = *argv.add(*OPTIND as usize);
        let is_long = !longopts.is_null() && *a.add(1) == b'-';
        gs.next = a.add(if is_long { 2 } else { 1 });
        // getopt_long_only: "-f" alone, with f a short option, is that short option
        let single_short = !is_long && *a.add(2) == 0 && os.contains(&*a.add(1));
        if !longopts.is_null() && (is_long || (long_only && !single_short)) {
            // a long option; with getopt_long_only, "-abc" is one too unless no long option
            // matches and "-a" is a short one
            let r = long_option(argc, argv, os, longopts, longind, long_only && !is_long, colon);
            if r != -2 {
                return r;
            }
        }
    }

    // short options
    let c = *gs.next;
    gs.next = gs.next.add(1);
    let pos = os.iter().position(|&x| x == c);
    if *gs.next == 0 {
        *OPTIND += 1;
    }
    let spec = match pos {
        Some(p) if c != b':' && c != b';' => &os[p..],
        _ => {
            complain(argv, &[b"invalid option -- '", &[c], b"'"]);
            *OPTOPT = c as c_int;
            return b'?' as c_int;
        }
    };
    // -W foo is --foo when "W;" is in the optstring
    if spec.get(1) == Some(&b';') && c == b'W' && !longopts.is_null() {
        if *gs.next != 0 {
            *OPTARG = gs.next as *mut c_char;
        } else if *OPTIND == argc {
            complain(argv, &[b"option requires an argument -- '", &[c], b"'"]);
            *OPTOPT = c as c_int;
            return if colon { b':' } else { b'?' } as c_int;
        } else {
            *OPTARG = *argv.add(*OPTIND as usize);
        }
        gs.next = *OPTARG;
        if *gs.next == 0 {
            *OPTIND += 1;
        }
        let r = long_option(argc, argv, os, longopts, longind, false, colon);
        if r != -2 {
            return r;
        }
        gs.next = ptr::null();
        return b'W' as c_int;
    }
    if spec.get(1) == Some(&b':') {
        if spec.get(2) == Some(&b':') {
            // optional argument: only if attached
            if *gs.next != 0 {
                *OPTARG = gs.next as *mut c_char;
                *OPTIND += 1;
            } else {
                *OPTARG = ptr::null_mut();
            }
            gs.next = ptr::null();
        } else {
            if *gs.next != 0 {
                *OPTARG = gs.next as *mut c_char;
                *OPTIND += 1;
            } else if *OPTIND == argc {
                complain(argv, &[b"option requires an argument -- '", &[c], b"'"]);
                *OPTOPT = c as c_int;
                gs.next = ptr::null();
                return if colon { b':' } else { b'?' } as c_int;
            } else {
                *OPTARG = *argv.add(*OPTIND as usize);
                *OPTIND += 1;
            }
            gs.next = ptr::null();
        }
    }
    c as c_int
}

/// Parse the long option at GS.next. Returns -2 when, for getopt_long_only, the text should be
/// taken as short options instead.
unsafe fn long_option(argc: c_int, argv: *mut *mut c_char, os: &[u8], longopts: *const LongOpt, longind: *mut c_int, maybe_short: bool, colon: bool) -> c_int {
    let gs = &mut *ptr::addr_of_mut!(GS);
    let text = fl::mem::cstr(gs.next);
    let (name, value) = match text.iter().position(|&c| c == b'=') {
        Some(i) => (&text[..i], Some(i + 1)),
        None => (text, None),
    };
    let mut exact: Option<usize> = None;
    let mut found: Option<usize> = None;
    let mut ambiguous = false;
    let mut i = 0;
    loop {
        let o = &*longopts.add(i);
        if o.name.is_null() {
            break;
        }
        let on = fl::mem::cstr(o.name);
        if on.starts_with(name) {
            if on.len() == name.len() {
                exact = Some(i);
                break;
            }
            match found {
                None => found = Some(i),
                Some(f) => {
                    let p = &*longopts.add(f);
                    if p.has_arg != o.has_arg || p.flag != o.flag || p.val != o.val {
                        ambiguous = true;
                    }
                }
            }
        }
        i += 1;
    }
    let hit = exact.or(if ambiguous { None } else { found });
    let full = *argv.add(*OPTIND as usize);
    if ambiguous && exact.is_none() {
        complain(argv, &[b"option '", fl::mem::cstr(full), b"' is ambiguous"]);
        gs.next = ptr::null();
        *OPTIND += 1;
        *OPTOPT = 0;
        return b'?' as c_int;
    }
    let Some(idx) = hit else {
        // getopt_long_only: fall back to a short option if the first letter is one
        if maybe_short && os.contains(&gs.next.read()) {
            return -2;
        }
        complain(argv, &[b"unrecognized option '", fl::mem::cstr(full), b"'"]);
        gs.next = ptr::null();
        *OPTIND += 1;
        *OPTOPT = 0;
        return b'?' as c_int;
    };
    let o = &*longopts.add(idx);
    *OPTIND += 1;
    gs.next = ptr::null();
    match (o.has_arg, value) {
        (0, Some(_)) => {
            complain(argv, &[b"option '--", fl::mem::cstr(o.name), b"' doesn't allow an argument"]);
            *OPTOPT = o.val;
            return b'?' as c_int;
        }
        (_, Some(v)) => *OPTARG = text.as_ptr().add(v) as *mut c_char,
        (1, None) => {
            if *OPTIND < argc {
                *OPTARG = *argv.add(*OPTIND as usize);
                *OPTIND += 1;
            } else {
                complain(argv, &[b"option '--", fl::mem::cstr(o.name), b"' requires an argument"]);
                *OPTOPT = o.val;
                return if colon { b':' } else { b'?' } as c_int;
            }
        }
        _ => {}
    }
    if !longind.is_null() {
        *longind = idx as c_int;
    }
    if !o.flag.is_null() {
        *o.flag = o.val;
        return 0;
    }
    o.val
}

#[no_mangle]
pub unsafe extern "C" fn getopt(argc: c_int, argv: *mut *mut c_char, os: *const c_char) -> c_int {
    getopt_internal(argc, argv, os, ptr::null(), ptr::null_mut(), false)
}
#[no_mangle]
pub unsafe extern "C" fn __posix_getopt(argc: c_int, argv: *mut *mut c_char, os: *const c_char) -> c_int {
    // POSIX: stop at the first non-option
    let mut v: Vec<u8> = alloc::vec![b'+'];
    let s = fl::mem::cstr(os);
    v.extend_from_slice(if s.first() == Some(&b'+') || s.first() == Some(&b'-') { &s[1..] } else { s });
    v.push(0);
    getopt_internal(argc, argv, v.as_ptr(), ptr::null(), ptr::null_mut(), false)
}
#[no_mangle]
pub unsafe extern "C" fn getopt_long(argc: c_int, argv: *mut *mut c_char, os: *const c_char, lo: *const LongOpt, li: *mut c_int) -> c_int {
    getopt_internal(argc, argv, os, lo, li, false)
}
#[no_mangle]
pub unsafe extern "C" fn getopt_long_only(argc: c_int, argv: *mut *mut c_char, os: *const c_char, lo: *const LongOpt, li: *mut c_int) -> c_int {
    getopt_internal(argc, argv, os, lo, li, true)
}

// --- basename / dirname ---------------------------------------------------------------------------

/// GNU basename: the part after the last slash ("" for a trailing slash); never modifies.
#[no_mangle]
pub unsafe extern "C" fn basename(path: *const c_char) -> *mut c_char {
    let s = fl::mem::cstr(path);
    match s.iter().rposition(|&c| c == b'/') {
        Some(i) => path.add(i + 1) as *mut c_char,
        None => path as *mut c_char,
    }
}

/// POSIX basename: strips trailing slashes (in place).
#[no_mangle]
pub unsafe extern "C" fn __xpg_basename(path: *mut c_char) -> *mut c_char {
    if path.is_null() || *path == 0 {
        return b".\0".as_ptr() as *mut c_char;
    }
    let mut n = fl::mem::cstrlen(path);
    while n > 1 && *path.add(n - 1) == b'/' {
        n -= 1;
    }
    if n == 1 && *path == b'/' {
        *path.add(1) = 0;
        return path;
    }
    *path.add(n) = 0;
    let s = core::slice::from_raw_parts(path, n);
    match s.iter().rposition(|&c| c == b'/') {
        Some(i) => path.add(i + 1),
        None => path,
    }
}

#[no_mangle]
pub unsafe extern "C" fn dirname(path: *mut c_char) -> *mut c_char {
    if path.is_null() || *path == 0 {
        return b".\0".as_ptr() as *mut c_char;
    }
    let mut n = fl::mem::cstrlen(path);
    // only slashes: "//" stays (POSIX leaves it to the implementation), others become "/"
    if fl::mem::cstr(path).iter().all(|&c| c == b'/') {
        *path.add(if n == 2 { 2 } else { 1 }) = 0;
        return path;
    }
    while n > 1 && *path.add(n - 1) == b'/' {
        n -= 1;
    }
    let s = core::slice::from_raw_parts(path, n);
    match s.iter().rposition(|&c| c == b'/') {
        None => b".\0".as_ptr() as *mut c_char,
        Some(mut i) => {
            while i > 0 && *path.add(i - 1) == b'/' {
                i -= 1;
            }
            if i == 0 {
                // the root ("//" stays as POSIX allows; glibc keeps "//")
                let keep = if n >= 2 && *path.add(1) == b'/' && (n == 2 || *path.add(2) != b'/') { 2 } else { 1 };
                *path.add(keep) = 0;
            } else {
                *path.add(i) = 0;
            }
            path
        }
    }
}

// --- err / warn / error --------------------------------------------------------------------------

unsafe fn report(fmt: *const c_char, va: Option<&mut Va>, err: Option<c_int>, prefix_name: bool) {
    let mut out = VecSink(Vec::new());
    if prefix_name {
        let p = *super::start::PROGNAME;
        if !p.is_null() {
            out.0.extend_from_slice(fl::mem::cstr(p));
        }
        out.0.extend_from_slice(b": ");
    }
    let has_fmt = !fmt.is_null();
    if has_fmt {
        if let Some(va) = va {
            format(&mut out, fmt, va);
        }
    }
    if let Some(e) = err {
        if has_fmt {
            out.0.extend_from_slice(b": ");
        }
        out.0.extend_from_slice(fl::mem::cstr(fl::strerror::strerror(e) as *const u8));
    }
    out.0.push(b'\n');
    let f = stdio::stderr_file();
    stdio::fwrite(out.0.as_ptr() as *const c_void, 1, out.0.len(), f);
}

macro_rules! va {
    ($args:ident) => {
        Va(&mut $args as *mut _ as *mut RawVa)
    };
}

#[no_mangle]
pub unsafe extern "C" fn vwarn(fmt: *const c_char, mut ap: VaList) {
    let e = fl::errno::get();
    report(fmt, Some(&mut Va(&mut ap as *mut VaList as *mut RawVa)), Some(e), true);
}
#[no_mangle]
pub unsafe extern "C" fn vwarnx(fmt: *const c_char, mut ap: VaList) {
    report(fmt, Some(&mut Va(&mut ap as *mut VaList as *mut RawVa)), None, true);
}
#[no_mangle]
pub unsafe extern "C" fn warn(fmt: *const c_char, mut args: ...) {
    let e = fl::errno::get();
    report(fmt, Some(&mut va!(args)), Some(e), true);
}
#[no_mangle]
pub unsafe extern "C" fn warnx(fmt: *const c_char, mut args: ...) {
    report(fmt, Some(&mut va!(args)), None, true);
}
#[no_mangle]
pub unsafe extern "C" fn verr(status: c_int, fmt: *const c_char, mut ap: VaList) -> ! {
    vwarn(fmt, ap);
    super::start::exit(status)
}
#[no_mangle]
pub unsafe extern "C" fn verrx(status: c_int, fmt: *const c_char, mut ap: VaList) -> ! {
    vwarnx(fmt, ap);
    super::start::exit(status)
}
#[no_mangle]
pub unsafe extern "C" fn err(status: c_int, fmt: *const c_char, mut args: ...) -> ! {
    let e = fl::errno::get();
    report(fmt, Some(&mut va!(args)), Some(e), true);
    super::start::exit(status)
}
#[no_mangle]
pub unsafe extern "C" fn errx(status: c_int, fmt: *const c_char, mut args: ...) -> ! {
    report(fmt, Some(&mut va!(args)), None, true);
    super::start::exit(status)
}

#[no_mangle]
pub static mut error_message_count: u32 = 0;
#[no_mangle]
pub static mut error_one_per_line: c_int = 0;
#[no_mangle]
pub static mut error_print_progname: Option<unsafe extern "C" fn()> = None;

#[no_mangle]
pub unsafe extern "C" fn error(status: c_int, errnum: c_int, fmt: *const c_char, mut args: ...) {
    stdio::fflush(stdio::stdout_file());
    match error_print_progname {
        Some(f) => f(),
        None => {
            let p = *super::start::PROGNAME_FULL;
            let mut v: Vec<u8> = if p.is_null() { Vec::new() } else { fl::mem::cstr(p).to_vec() };
            v.extend_from_slice(b": ");
            stdio::fwrite(v.as_ptr() as *const c_void, 1, v.len(), stdio::stderr_file());
        }
    }
    report(fmt, Some(&mut va!(args)), if errnum != 0 { Some(errnum) } else { None }, false);
    error_message_count += 1;
    if status != 0 {
        super::start::exit(status);
    }
}
#[no_mangle]
pub unsafe extern "C" fn error_at_line(status: c_int, errnum: c_int, file: *const c_char, line: u32, fmt: *const c_char, mut args: ...) {
    stdio::fflush(stdio::stdout_file());
    let p = *super::start::PROGNAME_FULL;
    let mut v: Vec<u8> = if p.is_null() { Vec::new() } else { fl::mem::cstr(p).to_vec() };
    v.push(b':');
    if !file.is_null() {
        v.extend_from_slice(fl::mem::cstr(file));
        v.extend_from_slice(alloc::format!(":{}: ", line).as_bytes());
    }
    stdio::fwrite(v.as_ptr() as *const c_void, 1, v.len(), stdio::stderr_file());
    report(fmt, Some(&mut va!(args)), if errnum != 0 { Some(errnum) } else { None }, false);
    error_message_count += 1;
    if status != 0 {
        super::start::exit(status);
    }
}

// --- syslog ----------------------------------------------------------------------------------------

static mut LOG_IDENT: [u8; 64] = [0; 64];
static mut LOG_OPT: c_int = 0;
static mut LOG_FAC: c_int = 1 << 3; // LOG_USER
static mut LOG_MASK: c_int = 0xff;
static mut LOG_FD: c_int = -1;
static LOG_LOCK: fl::lock::Lock = fl::lock::Lock::new();

const LOG_PID: c_int = 1;
const LOG_CONS: c_int = 2;
const LOG_NDELAY: c_int = 8;
const LOG_PERROR: c_int = 0x20;

unsafe fn log_connect() {
    if LOG_FD >= 0 {
        return;
    }
    let fd = fl::sys::sc3(fl::sys::SOCKET, 1 /* AF_UNIX */, 2 /* SOCK_DGRAM */ | 0o2000000, 0);
    if fd < 0 {
        return;
    }
    let mut sa = [0u8; 110];
    sa[0] = 1;
    sa[2..2 + 8].copy_from_slice(b"/dev/log");
    if fl::sys::sc3(fl::sys::CONNECT, fd as usize, sa.as_ptr() as usize, 2 + 9) < 0 {
        fl::sys::sc1(fl::sys::CLOSE, fd as usize);
        return;
    }
    LOG_FD = fd as c_int;
}

#[no_mangle]
pub unsafe extern "C" fn openlog(ident: *const c_char, opt: c_int, fac: c_int) {
    let _g = LOG_LOCK.guard();
    let id = &mut *ptr::addr_of_mut!(LOG_IDENT);
    id.fill(0);
    if !ident.is_null() {
        let s = fl::mem::cstr(ident);
        let n = s.len().min(63);
        id[..n].copy_from_slice(&s[..n]);
    }
    LOG_OPT = opt;
    if fac != 0 {
        LOG_FAC = fac;
    }
    if opt & LOG_NDELAY != 0 {
        log_connect();
    }
}
#[no_mangle]
pub unsafe extern "C" fn closelog() {
    let _g = LOG_LOCK.guard();
    if LOG_FD >= 0 {
        fl::sys::sc1(fl::sys::CLOSE, LOG_FD as usize);
        LOG_FD = -1;
    }
}
#[no_mangle]
pub unsafe extern "C" fn setlogmask(mask: c_int) -> c_int {
    let old = LOG_MASK;
    if mask != 0 {
        LOG_MASK = mask;
    }
    old
}

unsafe fn vsyslog_va(pri: c_int, fmt: *const c_char, va: &mut Va) {
    if LOG_MASK & (1 << (pri & 7)) == 0 {
        return;
    }
    let saved = fl::errno::get();
    let _g = LOG_LOCK.guard();
    let pri = if pri & !0x3ff != 0 { pri & 0x3ff } else { pri };
    let pri = if pri & 0x3f8 == 0 { pri | LOG_FAC } else { pri };
    // "<pri>Mmm dd hh:mm:ss ident[pid]: message"
    let mut out = VecSink(Vec::new());
    out.0.extend_from_slice(alloc::format!("<{}>", pri).as_bytes());
    let now = fl::time::time(ptr::null_mut());
    let mut tm: super::time::Tm = core::mem::zeroed();
    super::time::localtime_r(&now, &mut tm);
    let mut ts = [0u8; 32];
    let n = super::time::strftime(ts.as_mut_ptr(), 32, b"%h %e %T \0".as_ptr(), &tm);
    out.0.extend_from_slice(&ts[..n]);
    let head = out.0.len();
    let ident = fl::mem::cstr(ptr::addr_of!(LOG_IDENT) as *const u8);
    if !ident.is_empty() {
        out.0.extend_from_slice(ident);
    } else {
        let p = *super::start::PROGNAME;
        if !p.is_null() {
            out.0.extend_from_slice(fl::mem::cstr(p));
        }
    }
    if LOG_OPT & LOG_PID != 0 {
        out.0.extend_from_slice(alloc::format!("[{}]", fl::sys::sc0(39 /* getpid */)).as_bytes());
    }
    out.0.extend_from_slice(b": ");
    fl::errno::set(saved);
    format(&mut out, fmt, va);
    log_connect();
    let mut sent = false;
    if LOG_FD >= 0 {
        sent = fl::sys::sc3(fl::sys::WRITE, LOG_FD as usize, out.0.as_ptr() as usize, out.0.len()) >= 0;
        if !sent {
            fl::sys::sc1(fl::sys::CLOSE, LOG_FD as usize);
            LOG_FD = -1;
        }
    }
    if LOG_OPT & LOG_PERROR != 0 || (!sent && LOG_OPT & LOG_CONS != 0) {
        let mut line = out.0[head..].to_vec();
        if line.last() != Some(&b'\n') {
            line.push(b'\n');
        }
        fl::file::write(2, line.as_ptr() as *const c_void, line.len());
    }
}

#[no_mangle]
pub unsafe extern "C" fn vsyslog(pri: c_int, fmt: *const c_char, mut ap: VaList) {
    vsyslog_va(pri, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa));
}
#[no_mangle]
pub unsafe extern "C" fn syslog(pri: c_int, fmt: *const c_char, mut args: ...) {
    vsyslog_va(pri, fmt, &mut va!(args));
}
#[no_mangle]
pub unsafe extern "C" fn __syslog_chk(pri: c_int, _flag: c_int, fmt: *const c_char, mut args: ...) {
    vsyslog_va(pri, fmt, &mut va!(args));
}
#[no_mangle]
pub unsafe extern "C" fn __vsyslog_chk(pri: c_int, _flag: c_int, fmt: *const c_char, mut ap: VaList) {
    vsyslog_va(pri, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa));
}

// --- mntent ---------------------------------------------------------------------------------------

#[repr(C)]
pub struct Mntent {
    fsname: *mut c_char,
    dir: *mut c_char,
    ty: *mut c_char,
    opts: *mut c_char,
    freq: c_int,
    passno: c_int,
}

#[no_mangle]
pub unsafe extern "C" fn setmntent(path: *const c_char, mode: *const c_char) -> *mut FILE {
    stdio::fopen(path, mode)
}
#[no_mangle]
pub unsafe extern "C" fn endmntent(f: *mut FILE) -> c_int {
    if !f.is_null() {
        stdio::fclose(f);
    }
    1
}

/// Undo the octal escapes fstab uses for spaces and the like (\040), in place.
fn unescape(s: &mut [u8]) -> usize {
    let (mut r, mut w) = (0, 0);
    while r < s.len() {
        if s[r] == b'\\' && r + 3 < s.len() + 0 && s[r + 1..r + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            s[w] = (s[r + 1] - b'0') * 64 + (s[r + 2] - b'0') * 8 + (s[r + 3] - b'0');
            r += 4;
        } else {
            s[w] = s[r];
            r += 1;
        }
        w += 1;
    }
    w
}

#[no_mangle]
pub unsafe extern "C" fn getmntent_r(f: *mut FILE, m: *mut Mntent, buf: *mut c_char, len: c_int) -> *mut Mntent {
    if f.is_null() || len <= 0 {
        return ptr::null_mut();
    }
    loop {
        if stdio::fgets(buf, len, f).is_null() {
            return ptr::null_mut();
        }
        let n = fl::mem::cstrlen(buf);
        let line = core::slice::from_raw_parts_mut(buf, n);
        // fields, split on blanks, terminated in place
        let mut fields: [(usize, usize); 6] = [(0, 0); 6];
        let mut nf = 0;
        let mut i = 0;
        while i < n && nf < 6 {
            while i < n && (line[i] == b' ' || line[i] == b'\t' || line[i] == b'\n') {
                i += 1;
            }
            if i >= n || (nf == 0 && line[i] == b'#') {
                break;
            }
            let st = i;
            while i < n && line[i] != b' ' && line[i] != b'\t' && line[i] != b'\n' {
                i += 1;
            }
            fields[nf] = (st, i);
            nf += 1;
        }
        if nf < 4 {
            continue;
        }
        let mut ptrs = [ptr::null_mut(); 4];
        for k in 0..4 {
            let (s, e) = fields[k];
            let w = unescape(&mut line[s..e]);
            line[s + w] = 0;
            ptrs[k] = buf.add(s);
        }
        let num = |k: usize| -> c_int {
            if k >= nf {
                return 0;
            }
            let (s, e) = fields[k];
            core::str::from_utf8(&line[s..e]).ok().and_then(|x| x.parse().ok()).unwrap_or(0)
        };
        (*m).freq = num(4);
        (*m).passno = num(5);
        (*m).fsname = ptrs[0];
        (*m).dir = ptrs[1];
        (*m).ty = ptrs[2];
        (*m).opts = ptrs[3];
        return m;
    }
}

static mut MNT: Mntent = Mntent { fsname: ptr::null_mut(), dir: ptr::null_mut(), ty: ptr::null_mut(), opts: ptr::null_mut(), freq: 0, passno: 0 };
static mut MNT_BUF: [u8; 4096] = [0; 4096];

#[no_mangle]
pub unsafe extern "C" fn getmntent(f: *mut FILE) -> *mut Mntent {
    getmntent_r(f, ptr::addr_of_mut!(MNT), ptr::addr_of_mut!(MNT_BUF) as *mut c_char, 4096)
}

#[no_mangle]
pub unsafe extern "C" fn addmntent(f: *mut FILE, m: *const Mntent) -> c_int {
    let esc = |s: *const c_char, out: &mut Vec<u8>| {
        for &c in fl::mem::cstr(s) {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\\' {
                out.extend_from_slice(alloc::format!("\\{:03o}", c).as_bytes());
            } else {
                out.push(c);
            }
        }
    };
    let mut v = Vec::new();
    esc((*m).fsname, &mut v);
    v.push(b' ');
    esc((*m).dir, &mut v);
    v.push(b' ');
    esc((*m).ty, &mut v);
    v.push(b' ');
    esc((*m).opts, &mut v);
    v.extend_from_slice(alloc::format!(" {} {}\n", (*m).freq, (*m).passno).as_bytes());
    stdio::fseek(f, 0, 2);
    if stdio::fwrite(v.as_ptr() as *const c_void, 1, v.len(), f) != v.len() {
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn hasmntopt(m: *const Mntent, opt: *const c_char) -> *mut c_char {
    let o = fl::mem::cstr(opt);
    let base = (*m).opts;
    let s = fl::mem::cstr(base);
    let mut st = 0;
    for (i, &c) in s.iter().chain(core::iter::once(&b',')).enumerate() {
        if c == b',' {
            let item = &s[st..i];
            if item.starts_with(o) && (item.len() == o.len() || item[o.len()] == b'=') {
                return base.add(st);
            }
            st = i + 1;
        }
    }
    ptr::null_mut()
}

// --- obstack ---------------------------------------------------------------------------------------

#[repr(C)]
pub struct Obstack {
    chunk_size: usize,
    chunk: *mut Chunk,
    object_base: *mut u8,
    next_free: *mut u8,
    chunk_limit: *mut u8,
    temp: usize,
    alignment_mask: usize,
    chunkfun: *mut c_void,
    freefun: *mut c_void,
    extra_arg: *mut c_void,
    flags: u32, // use_extra_arg:1, maybe_empty_object:1, alloc_failed:1
}

#[repr(C)]
pub struct Chunk {
    limit: *mut u8,
    prev: *mut Chunk,
    contents: [u8; 0],
}

const USE_EXTRA: u32 = 1;
const MAYBE_EMPTY: u32 = 2;

unsafe extern "C" fn default_failed() {
    let m = b"memory exhausted\n";
    fl::file::write(2, m.as_ptr() as *const c_void, m.len());
    super::start::exit(obstack_exit_failure);
}

#[no_mangle]
pub static mut obstack_alloc_failed_handler: unsafe extern "C" fn() = default_failed;
#[no_mangle]
pub static mut obstack_exit_failure: c_int = 1;

unsafe fn mask(h: *const Obstack) -> usize {
    // the field is an int in the old interface: read the low 32 bits
    ((*h).alignment_mask as u32) as usize
}
unsafe fn call_chunk(h: *mut Obstack, size: usize) -> *mut Chunk {
    if (*h).flags & USE_EXTRA != 0 {
        let f: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void = core::mem::transmute((*h).chunkfun);
        f((*h).extra_arg, size) as *mut Chunk
    } else {
        let f: unsafe extern "C" fn(usize) -> *mut c_void = core::mem::transmute((*h).chunkfun);
        f(size) as *mut Chunk
    }
}
unsafe fn call_free(h: *mut Obstack, c: *mut Chunk) {
    if (*h).flags & USE_EXTRA != 0 {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) = core::mem::transmute((*h).freefun);
        f((*h).extra_arg, c as *mut c_void)
    } else {
        let f: unsafe extern "C" fn(*mut c_void) = core::mem::transmute((*h).freefun);
        f(c as *mut c_void)
    }
}
fn align_up(p: *mut u8, m: usize) -> *mut u8 {
    ((p as usize + m) & !m) as *mut u8
}

unsafe fn begin(h: *mut Obstack, size: usize, align: usize, chunkfun: *mut c_void, freefun: *mut c_void, arg: *mut c_void, extra: bool) -> c_int {
    let align = if align == 0 { 16 } else { align };
    let size = if size == 0 { 4064 } else { size };
    (*h).chunkfun = chunkfun;
    (*h).freefun = freefun;
    (*h).chunk_size = size;
    (*h).alignment_mask = align - 1;
    (*h).extra_arg = arg;
    (*h).flags = if extra { USE_EXTRA } else { 0 };
    let c = call_chunk(h, size);
    (*h).chunk = c;
    if c.is_null() {
        obstack_alloc_failed_handler();
        return 0;
    }
    (*h).object_base = align_up((*c).contents.as_mut_ptr(), align - 1);
    (*h).next_free = (*h).object_base;
    (*c).limit = (c as *mut u8).add(size);
    (*h).chunk_limit = (*c).limit;
    (*c).prev = ptr::null_mut();
    1
}

#[no_mangle]
pub unsafe extern "C" fn _obstack_begin(h: *mut Obstack, size: c_int, align: c_int, chunkfun: *mut c_void, freefun: *mut c_void) -> c_int {
    begin(h, size.max(0) as usize, align.max(0) as usize, chunkfun, freefun, ptr::null_mut(), false)
}
#[no_mangle]
pub unsafe extern "C" fn _obstack_begin_1(h: *mut Obstack, size: c_int, align: c_int, chunkfun: *mut c_void, freefun: *mut c_void, arg: *mut c_void) -> c_int {
    begin(h, size.max(0) as usize, align.max(0) as usize, chunkfun, freefun, arg, true)
}

#[no_mangle]
pub unsafe extern "C" fn _obstack_newchunk(h: *mut Obstack, length: c_int) {
    let old = (*h).chunk;
    let obj = (*h).next_free.offset_from((*h).object_base) as usize;
    let m = mask(h);
    let mut new_size = obj + length.max(0) as usize + (obj >> 3) + m + 100;
    if new_size < (*h).chunk_size {
        new_size = (*h).chunk_size;
    }
    let c = call_chunk(h, new_size);
    if c.is_null() {
        obstack_alloc_failed_handler();
        return;
    }
    (*h).chunk = c;
    (*c).prev = old;
    (*c).limit = (c as *mut u8).add(new_size);
    (*h).chunk_limit = (*c).limit;
    let base = align_up((*c).contents.as_mut_ptr(), m);
    ptr::copy_nonoverlapping((*h).object_base, base, obj);
    // free the old chunk if the object was all it held
    if (*h).flags & MAYBE_EMPTY == 0 && (*h).object_base == align_up((*old).contents.as_mut_ptr(), m) {
        (*c).prev = (*old).prev;
        call_free(h, old);
    }
    (*h).object_base = base;
    (*h).next_free = base.add(obj);
    (*h).flags &= !MAYBE_EMPTY;
}

#[no_mangle]
pub unsafe extern "C" fn _obstack_allocated_p(h: *mut Obstack, obj: *mut c_void) -> c_int {
    let mut lp = (*h).chunk;
    let o = obj as *mut u8;
    while !lp.is_null() && (o <= lp as *mut u8 || o > (*lp).limit) {
        lp = (*lp).prev;
    }
    (!lp.is_null()) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn _obstack_free(h: *mut Obstack, obj: *mut c_void) {
    let o = obj as *mut u8;
    let mut lp = (*h).chunk;
    while !lp.is_null() && (o <= lp as *mut u8 || o > (*lp).limit) {
        let plp = (*lp).prev;
        call_free(h, lp);
        lp = plp;
        (*h).flags |= MAYBE_EMPTY;
    }
    if !lp.is_null() {
        (*h).object_base = o;
        (*h).next_free = o;
        (*h).chunk_limit = (*lp).limit;
        (*h).chunk = lp;
    } else if !obj.is_null() {
        fl::start::abort();
    }
}
#[no_mangle]
pub unsafe extern "C" fn obstack_free(h: *mut Obstack, obj: *mut c_void) {
    _obstack_free(h, obj)
}
#[no_mangle]
pub unsafe extern "C" fn _obstack_memory_used(h: *mut Obstack) -> c_int {
    let mut n = 0usize;
    let mut lp = (*h).chunk;
    while !lp.is_null() {
        n += (*lp).limit.offset_from(lp as *mut u8) as usize;
        lp = (*lp).prev;
    }
    n as c_int
}

unsafe fn obstack_grow(h: *mut Obstack, data: &[u8]) {
    if ((*h).chunk_limit.offset_from((*h).next_free) as usize) < data.len() {
        _obstack_newchunk(h, data.len() as c_int);
    }
    ptr::copy_nonoverlapping(data.as_ptr(), (*h).next_free, data.len());
    (*h).next_free = (*h).next_free.add(data.len());
}

#[no_mangle]
pub unsafe extern "C" fn obstack_vprintf(h: *mut Obstack, fmt: *const c_char, mut ap: VaList) -> c_int {
    let mut out = VecSink(Vec::new());
    format(&mut out, fmt, &mut Va(&mut ap as *mut VaList as *mut RawVa));
    obstack_grow(h, &out.0);
    out.0.len() as c_int
}
#[no_mangle]
pub unsafe extern "C" fn obstack_printf(h: *mut Obstack, fmt: *const c_char, mut args: ...) -> c_int {
    let mut out = VecSink(Vec::new());
    format(&mut out, fmt, &mut va!(args));
    obstack_grow(h, &out.0);
    out.0.len() as c_int
}
#[no_mangle]
pub unsafe extern "C" fn __obstack_vprintf_chk(h: *mut Obstack, _flag: c_int, fmt: *const c_char, mut ap: VaList) -> c_int {
    obstack_vprintf(h, fmt, ap)
}
#[no_mangle]
pub unsafe extern "C" fn __obstack_printf_chk(h: *mut Obstack, _flag: c_int, fmt: *const c_char, mut args: ...) -> c_int {
    let mut out = VecSink(Vec::new());
    format(&mut out, fmt, &mut va!(args));
    obstack_grow(h, &out.0);
    out.0.len() as c_int
}
