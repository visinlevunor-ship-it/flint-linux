//! Time: clock_gettime (through the kernel's vDSO, so no system call), clock_settime,
//! clock_nanosleep, nanosleep, time.

use crate::imp::sys::{self, ret_i, sc2, sc4};
use crate::imp::types::*;
use crate::imp::misc;
use core::sync::atomic::{AtomicUsize, Ordering};

/// The vDSO's __vdso_clock_gettime: 0 not looked up yet, 1 not there.
static VDSO_GETTIME: AtomicUsize = AtomicUsize::new(0);

unsafe fn vdso_gettime() -> usize {
    let f = VDSO_GETTIME.load(Ordering::Relaxed);
    if f != 0 {
        return f;
    }
    let f = misc::vdso_symbol(b"__vdso_clock_gettime").unwrap_or(1);
    VDSO_GETTIME.store(f, Ordering::Relaxed);
    f
}

#[no_mangle]
pub unsafe extern "C" fn clock_gettime(clk: clockid_t, ts: *mut timespec) -> c_int {
    let f = vdso_gettime();
    if f > 1 {
        let g: unsafe extern "C" fn(clockid_t, *mut timespec) -> c_int = core::mem::transmute(f);
        let r = g(clk, ts);
        if r == 0 {
            return 0;
        }
        // a clock the vDSO does not handle: ask the kernel
    }
    ret_i(sc2(sys::CLOCK_GETTIME, clk as usize, ts as usize))
}

#[no_mangle]
pub unsafe extern "C" fn clock_settime(clk: clockid_t, ts: *const timespec) -> c_int {
    ret_i(sc2(sys::CLOCK_SETTIME, clk as usize, ts as usize))
}

/// Returns the error number (not -1/errno), as POSIX specifies for clock_nanosleep.
#[no_mangle]
pub unsafe extern "C" fn clock_nanosleep(clk: clockid_t, flags: c_int, req: *const timespec, rem: *mut timespec) -> c_int {
    let r = sc4(sys::CLOCK_NANOSLEEP, clk as usize, flags as usize, req as usize, rem as usize);
    if r < 0 {
        (-r) as c_int
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn nanosleep(req: *const timespec, rem: *mut timespec) -> c_int {
    let e = clock_nanosleep(0, 0, req, rem);
    if e != 0 {
        crate::imp::errno::set(e);
        return -1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn time(t: *mut time_t) -> time_t {
    let mut ts = timespec { tv_sec: 0, tv_nsec: 0 };
    clock_gettime(0, &mut ts);
    if !t.is_null() {
        *t = ts.tv_sec;
    }
    ts.tv_sec
}
