//! Smaller pieces programs such as Java need: confstr, termios speeds, getloadavg, the
//! utmpx readers (Flint keeps no login records), hash tables (hsearch), times, vfork and the
//! CPU-set allocators.

use super::umem;
use alloc::vec::Vec;
use crate::fl;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;
type size_t = usize;

// --- confstr --------------------------------------------------------------------------------

const CS_PATH: c_int = 0;
const CS_GNU_LIBC_VERSION: c_int = 2;
const CS_GNU_LIBPTHREAD_VERSION: c_int = 3;

#[no_mangle]
pub unsafe extern "C" fn confstr(name: c_int, buf: *mut c_char, len: size_t) -> size_t {
    let v: &[u8] = match name {
        CS_PATH => b"/bin:/usr/bin",
        // programs (Java among them) parse these: speak glibc's
        CS_GNU_LIBC_VERSION => b"glibc 2.39",
        CS_GNU_LIBPTHREAD_VERSION => b"NPTL 2.39",
        // the *_CFLAGS / *_LDFLAGS / *_LIBS strings for the programming environments: empty
        1 | 4..=7 | 1000..=1150 => b"",
        _ => {
            fl::errno::set(22);
            return 0;
        }
    };
    if !buf.is_null() && len > 0 {
        let n = v.len().min(len - 1);
        ptr::copy_nonoverlapping(v.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    v.len() + 1
}

// --- termios speeds (glibc's struct termios: c_ispeed at 52, c_ospeed at 56) ---------------

const CBAUD: u32 = 0o010017;
const CBAUDEX: u32 = 0o010000;

fn valid_speed(s: u32) -> bool {
    s & !CBAUD == 0 && (s & CBAUDEX == 0 || (0o010001..=0o010017).contains(&s))
}

#[no_mangle]
pub unsafe extern "C" fn cfgetospeed(t: *const u8) -> u32 {
    *(t.add(8) as *const u32) & CBAUD
}
#[no_mangle]
pub unsafe extern "C" fn cfgetispeed(t: *const u8) -> u32 {
    cfgetospeed(t)
}
#[no_mangle]
pub unsafe extern "C" fn cfsetospeed(t: *mut u8, s: u32) -> c_int {
    if !valid_speed(s) {
        fl::errno::set(22);
        return -1;
    }
    let c = t.add(8) as *mut u32;
    *c = (*c & !CBAUD) | s;
    *(t.add(56) as *mut u32) = s;
    0
}
#[no_mangle]
pub unsafe extern "C" fn cfsetispeed(t: *mut u8, s: u32) -> c_int {
    if !valid_speed(s) {
        fl::errno::set(22);
        return -1;
    }
    // input speed 0 means "the same as the output speed"
    if s != 0 {
        let c = t.add(8) as *mut u32;
        *c = (*c & !CBAUD) | s;
    }
    *(t.add(52) as *mut u32) = s;
    0
}
#[no_mangle]
pub unsafe extern "C" fn cfsetspeed(t: *mut u8, s: u32) -> c_int {
    if cfsetospeed(t, s) != 0 {
        return -1;
    }
    cfsetispeed(t, s)
}

// --- getloadavg --------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn getloadavg(out: *mut f64, n: c_int) -> c_int {
    if n < 0 {
        return -1;
    }
    let mut buf = [0u8; 128];
    let len = fl::resolve::read_file(b"/proc/loadavg\0", &mut buf);
    if len == 0 {
        return -1;
    }
    let mut k = 0;
    for w in buf[..len].split(|&c| c == b' ').take((n as usize).min(3)) {
        let v = core::str::from_utf8(w).ok().and_then(|s| s.parse::<f64>().ok());
        match v {
            Some(v) => {
                *out.add(k) = v;
                k += 1;
            }
            None => break,
        }
    }
    k as c_int
}

// --- utmpx: Flint keeps no login records ------------------------------------------------------

#[no_mangle]
pub extern "C" fn setutxent() {}
#[no_mangle]
pub extern "C" fn endutxent() {}
#[no_mangle]
pub extern "C" fn getutxent() -> *mut c_void {
    ptr::null_mut()
}
#[no_mangle]
pub extern "C" fn getutxid(_u: *const c_void) -> *mut c_void {
    ptr::null_mut()
}
#[no_mangle]
pub extern "C" fn getutxline(_u: *const c_void) -> *mut c_void {
    ptr::null_mut()
}
#[no_mangle]
pub extern "C" fn pututxline(_u: *const c_void) -> *mut c_void {
    ptr::null_mut()
}
#[no_mangle]
pub extern "C" fn setutent() {}
#[no_mangle]
pub extern "C" fn endutent() {}
#[no_mangle]
pub extern "C" fn getutent() -> *mut c_void {
    ptr::null_mut()
}
#[no_mangle]
pub extern "C" fn utmpname(_f: *const c_char) -> c_int {
    0
}
#[no_mangle]
pub extern "C" fn utmpxname(_f: *const c_char) -> c_int {
    0
}

// --- hsearch ------------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Entry {
    key: *mut c_char,
    data: *mut c_void,
}

/// struct hsearch_data: table, size, filled.
#[repr(C)]
pub struct HData {
    table: *mut Slot,
    size: u32,
    filled: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Slot {
    used: u32,
    entry: Entry,
}

fn is_prime(n: u32) -> bool {
    if n < 2 {
        return false;
    }
    let mut d = 2u32;
    while d.saturating_mul(d) <= n {
        if n % d == 0 {
            return false;
        }
        d += 1;
    }
    true
}

#[no_mangle]
pub unsafe extern "C" fn hcreate_r(nel: size_t, h: *mut HData) -> c_int {
    if h.is_null() {
        fl::errno::set(22);
        return 0;
    }
    if !(*h).table.is_null() {
        return 0;
    }
    // a prime size, as glibc's double hashing wants
    let mut n = (nel.min(u32::MAX as usize - 2) as u32).max(3) | 1;
    while !is_prime(n) {
        n += 2;
    }
    let t = fl::malloc::imalloc((n as usize + 1) * core::mem::size_of::<Slot>()) as *mut Slot;
    if t.is_null() {
        fl::errno::set(12);
        return 0;
    }
    ptr::write_bytes(t, 0, n as usize + 1);
    (*h).table = t;
    (*h).size = n;
    (*h).filled = 0;
    1
}

#[no_mangle]
pub unsafe extern "C" fn hdestroy_r(h: *mut HData) {
    if h.is_null() {
        fl::errno::set(22);
        return;
    }
    fl::malloc::ifree((*h).table as *mut u8);
    (*h).table = ptr::null_mut();
}

#[no_mangle]
pub unsafe extern "C" fn hsearch_r(item: Entry, action: c_int, ret: *mut *mut Entry, h: *mut HData) -> c_int {
    let size = (*h).size;
    let key = fl::mem::cstr(item.key);
    // glibc's hash: shifted sum over the key
    let mut hval: u32 = key.len() as u32;
    for &c in key {
        hval = (hval << 4).wrapping_add(c as u32);
    }
    if hval == 0 {
        hval += 1;
    }
    let mut idx = hval % size;
    if idx == 0 {
        idx = 1;
    }
    let t = (*h).table;
    let step = 1 + hval % (size - 2);
    let first = idx;
    loop {
        let s = &mut *t.add(idx as usize);
        if s.used == 0 {
            break;
        }
        if s.used == hval && fl::mem::cstr(s.entry.key) == key {
            *ret = &mut s.entry;
            return 1;
        }
        idx = if idx <= step { idx + size - step } else { idx - step };
        if idx == first {
            break;
        }
    }
    if action == 1 {
        // ENTER
        if (*h).filled == size {
            fl::errno::set(12);
            *ret = ptr::null_mut();
            return 0;
        }
        let s = &mut *t.add(idx as usize);
        if s.used != 0 {
            fl::errno::set(12);
            *ret = ptr::null_mut();
            return 0;
        }
        s.used = hval;
        s.entry = item;
        (*h).filled += 1;
        *ret = &mut s.entry;
        return 1;
    }
    fl::errno::set(3); // ESRCH
    *ret = ptr::null_mut();
    0
}

static mut GLOBAL_H: HData = HData { table: ptr::null_mut(), size: 0, filled: 0 };

#[no_mangle]
pub unsafe extern "C" fn hcreate(nel: size_t) -> c_int {
    hcreate_r(nel, ptr::addr_of_mut!(GLOBAL_H))
}
#[no_mangle]
pub unsafe extern "C" fn hdestroy() {
    hdestroy_r(ptr::addr_of_mut!(GLOBAL_H))
}
#[no_mangle]
pub unsafe extern "C" fn hsearch(item: Entry, action: c_int) -> *mut Entry {
    let mut r = ptr::null_mut();
    hsearch_r(item, action, &mut r, ptr::addr_of_mut!(GLOBAL_H));
    r
}

// --- the rest ---------------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn times(t: *mut c_void) -> i64 {
    let r = fl::sys::sc1(100, t as usize);
    if r < 0 && r > -4096 {
        fl::errno::set(-r as c_int);
        return -1;
    }
    r as i64
}

/// vfork as fork: the child may only exec or _exit, which a fork child can do too.
#[no_mangle]
pub unsafe extern "C" fn vfork() -> c_int {
    fl::process::fork()
}

#[no_mangle]
pub unsafe extern "C" fn __sched_cpualloc(count: size_t) -> *mut c_void {
    let bytes = (count + 63) / 64 * 8;
    umem::calloc(1, bytes)
}
#[no_mangle]
pub unsafe extern "C" fn __sched_cpufree(p: *mut c_void) {
    umem::free(p)
}

// --- pieces Qt and libarchive import ---------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn bsearch(
    key: *const c_void,
    base: *const c_void,
    n: size_t,
    size: size_t,
    cmp: unsafe extern "C" fn(*const c_void, *const c_void) -> c_int,
) -> *mut c_void {
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = (base as *const u8).add(mid * size) as *const c_void;
        let r = cmp(key, p);
        if r == 0 {
            return p as *mut c_void;
        }
        if r < 0 {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn gnu_dev_major(dev: u64) -> u32 {
    (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfffu64) as u64) as u32
}
#[no_mangle]
pub extern "C" fn gnu_dev_minor(dev: u64) -> u32 {
    ((dev & 0xff) | ((dev >> 12) & !0xffu64)) as u32
}
#[no_mangle]
pub extern "C" fn gnu_dev_makedev(maj: u32, min: u32) -> u64 {
    let (maj, min) = (maj as u64, min as u64);
    ((maj & 0xfff) << 8) | (min & 0xff) | ((maj & !0xfff) << 32) | ((min & !0xff) << 12)
}

#[no_mangle]
pub unsafe extern "C" fn wait4(pid: c_int, status: *mut c_int, options: c_int, rusage: *mut c_void) -> c_int {
    fl::sys::ret_i(fl::sys::sc4(61 /* wait4 */, pid as usize, status as usize, options as usize, rusage as usize))
}

#[no_mangle]
pub unsafe extern "C" fn futimesat(dirfd: c_int, path: *const c_char, times: *const [i64; 2]) -> c_int {
    // as two timespecs for utimensat
    let mut ts = [[0i64; 2]; 2];
    let tsp = if times.is_null() {
        ptr::null()
    } else {
        for k in 0..2 {
            ts[k][0] = (*times.add(k))[0];
            ts[k][1] = (*times.add(k))[1] * 1000;
        }
        ts.as_ptr()
    };
    fl::sys::ret_i(fl::sys::sc4(280 /* utimensat */, dirfd as usize, path as usize, tsp as usize, 0))
}

/// Qt calls backtrace for its debug output; an empty trace is a valid answer.
#[no_mangle]
pub extern "C" fn backtrace(_buf: *mut *mut c_void, _n: c_int) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn backtrace_symbols(_buf: *const *mut c_void, _n: c_int) -> *mut *mut c_char {
    umem::calloc(1, 8) as *mut *mut c_char
}
#[no_mangle]
pub extern "C" fn backtrace_symbols_fd(_buf: *const *mut c_void, _n: c_int, _fd: c_int) {}

// --- named semaphores (sem_open): a 32-byte semaphore in a file under /dev/shm ------------------

unsafe fn sem_path(name: *const c_char) -> Option<Vec<u8>> {
    let mut n = fl::mem::cstr(name);
    while n.first() == Some(&b'/') {
        n = &n[1..];
    }
    if n.is_empty() || n.contains(&b'/') {
        fl::errno::set(22);
        return None;
    }
    let mut p: Vec<u8> = b"/dev/shm/sem.".to_vec();
    p.extend_from_slice(n);
    p.push(0);
    Some(p)
}

#[no_mangle]
pub unsafe extern "C" fn sem_open(name: *const c_char, oflag: c_int, mut args: ...) -> *mut c_void {
    const SEM_FAILED: *mut c_void = ptr::null_mut();
    let Some(p) = sem_path(name) else { return SEM_FAILED };
    let creating = oflag & 0o100 != 0;
    let (mode, value) = if creating {
        let mut va = super::printf::Va(&mut args as *mut _ as *mut super::printf::RawVa);
        (va.int() as u32, va.int() as u32)
    } else {
        (0, 0)
    };
    if value > i32::MAX as u32 {
        fl::errno::set(22);
        return SEM_FAILED;
    }
    // O_EXCL first when creating, so the value is set exactly once
    let fd = if creating {
        let fd = fl::file::open(p.as_ptr() as *const _, 0o2 | 0o100 | 0o200 /* RDWR|CREAT|EXCL */, mode);
        if fd >= 0 {
            let mut init = [0u8; 32];
            init[..4].copy_from_slice(&value.to_ne_bytes());
            init[8..12].copy_from_slice(&1u32.to_ne_bytes()); // shared
            if fl::file::write(fd, init.as_ptr() as *const c_void, 32) != 32 {
                fl::sys::sc1(fl::sys::CLOSE, fd as usize);
                return SEM_FAILED;
            }
            fd
        } else if fl::errno::get() == 17 /* EEXIST */ && oflag & 0o200 == 0 {
            fl::file::open(p.as_ptr() as *const _, 0o2, 0)
        } else {
            return SEM_FAILED;
        }
    } else {
        fl::file::open(p.as_ptr() as *const _, 0o2, 0)
    };
    if fd < 0 {
        return SEM_FAILED;
    }
    let m = fl::sys::sc6(9 /* mmap */, 0, 32, 3 /* RW */, 1 /* SHARED */, fd as usize, 0);
    fl::sys::sc1(fl::sys::CLOSE, fd as usize);
    if m < 0 {
        fl::errno::set((-m) as c_int);
        return SEM_FAILED;
    }
    m as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn sem_close(s: *mut c_void) -> c_int {
    fl::sys::sc2(11 /* munmap */, s as usize, 32);
    0
}

#[no_mangle]
pub unsafe extern "C" fn sem_unlink(name: *const c_char) -> c_int {
    let Some(p) = sem_path(name) else { return -1 };
    fl::sys::ret_i(fl::sys::sc1(87 /* unlink */, p.as_ptr() as usize))
}
