//! Everything else: temporary files, and smaller pieces.

use super::printf::{self, BufSink, Va};
use crate::fl;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;
type size_t = usize;

static mut RAND_STATE: u64 = 0x853c_49e6_748f_ea9b;

unsafe fn rand64() -> u64 {
    let mut b = [0u8; 8];
    if fl::sys::sc3(318 /* getrandom */, b.as_mut_ptr() as usize, 8, 1 /* GRND_NONBLOCK */) == 8 {
        return u64::from_ne_bytes(b);
    }
    RAND_STATE = RAND_STATE.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    RAND_STATE ^ fl::sys::sc0(39) as u64
}

/// Replace the trailing `suffix_len` + 6 "XXXXXX" and create the file.
unsafe fn make_temp(tmpl: *mut c_char, suffix_len: usize, flags: c_int, dir: bool) -> c_int {
    let n = fl::mem::cstrlen(tmpl);
    if n < 6 + suffix_len {
        fl::errno::set(fl::errno::EINVAL);
        return -1;
    }
    let x = tmpl.add(n - 6 - suffix_len);
    if core::slice::from_raw_parts(x, 6) != b"XXXXXX" {
        fl::errno::set(fl::errno::EINVAL);
        return -1;
    }
    const CH: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for _ in 0..1000 {
        let mut r = rand64();
        for i in 0..6 {
            *x.add(i) = CH[(r % 62) as usize];
            r /= 62;
        }
        let res = if dir {
            fl::sys::sc2(fl::sys::MKDIR, tmpl as usize, 0o700)
        } else {
            fl::sys::sc3(fl::sys::OPEN, tmpl as usize, (2 | 0o100 | 0o200 | flags) as usize /* RDWR|CREAT|EXCL */, 0o600)
        };
        if res >= 0 {
            return res as c_int;
        }
        if res != -17 {
            fl::errno::set(-res as i32);
            return -1;
        }
    }
    fl::errno::set(17);
    -1
}

#[no_mangle]
pub unsafe extern "C" fn mkstemp(t: *mut c_char) -> c_int {
    make_temp(t, 0, 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn mkstemp64(t: *mut c_char) -> c_int {
    make_temp(t, 0, 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn mkostemp(t: *mut c_char, flags: c_int) -> c_int {
    make_temp(t, 0, flags, false)
}
#[no_mangle]
pub unsafe extern "C" fn mkostemp64(t: *mut c_char, flags: c_int) -> c_int {
    make_temp(t, 0, flags, false)
}
#[no_mangle]
pub unsafe extern "C" fn mkstemps(t: *mut c_char, s: c_int) -> c_int {
    make_temp(t, s as usize, 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn mkostemps(t: *mut c_char, s: c_int, flags: c_int) -> c_int {
    make_temp(t, s as usize, flags, false)
}
#[no_mangle]
pub unsafe extern "C" fn mkdtemp(t: *mut c_char) -> *mut c_char {
    if make_temp(t, 0, 0, true) < 0 {
        ptr::null_mut()
    } else {
        t
    }
}
#[no_mangle]
pub unsafe extern "C" fn mktemp(t: *mut c_char) -> *mut c_char {
    let fd = make_temp(t, 0, 0, false);
    if fd >= 0 {
        fl::sys::sc1(fl::sys::CLOSE, fd as usize);
        fl::sys::sc1(fl::sys::UNLINK, t as usize);
    } else {
        *t = 0;
    }
    t
}

/// strfromd and friends: a printf conversion of one double.
pub unsafe fn strfrom_f64(buf: *mut c_char, n: size_t, fmt: *const c_char, v: f64) -> c_int {
    // build a va_list holding the double in its register save area
    let mut save = [0u8; 176 + 16];
    ptr::write_unaligned(save.as_mut_ptr().add(48) as *mut f64, v);
    let mut raw = [0u8; 24];
    ptr::write_unaligned(raw.as_mut_ptr() as *mut u32, 48); // gp_offset: no integer args
    ptr::write_unaligned(raw.as_mut_ptr().add(4) as *mut u32, 48); // fp_offset: first xmm slot
    ptr::write_unaligned(raw.as_mut_ptr().add(8) as *mut usize, save.as_mut_ptr().add(176) as usize);
    ptr::write_unaligned(raw.as_mut_ptr().add(16) as *mut usize, save.as_mut_ptr() as usize);
    let mut va = Va(raw.as_mut_ptr() as *mut printf::RawVa);
    let mut s = BufSink { buf, cap: n, len: 0 };
    let r = printf::format(&mut s, fmt, &mut va);
    if n > 0 {
        *buf.add(r.min(n - 1)) = 0;
    }
    r as c_int
}

#[allow(dead_code)]
fn _u(_: *mut c_void) {}
