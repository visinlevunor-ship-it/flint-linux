//! Terminals: tcgetattr, tcsetattr, tcgetpgrp, tcsetpgrp, ttyname.
//!
//! musl's struct termios starts with the kernel's (flags, line, 32 control characters) and
//! adds two speed fields after it, so the ioctls work on it directly.

use crate::imp::sys::{self, ret, ret_i, sc3};
use crate::imp::types::*;
use crate::imp::errno;
use core::ptr;

const TCGETS: usize = 0x5401;
const TCSETS: usize = 0x5402;
const TIOCGPGRP: usize = 0x540f;
const TIOCSPGRP: usize = 0x5410;

#[no_mangle]
pub unsafe extern "C" fn tcgetattr(fd: c_int, t: *mut c_void) -> c_int {
    ret_i(sc3(sys::IOCTL, fd as isize as usize, TCGETS, t as usize))
}

#[no_mangle]
pub unsafe extern "C" fn tcsetattr(fd: c_int, when: c_int, t: *const c_void) -> c_int {
    // TCSANOW, TCSADRAIN, TCSAFLUSH are 0, 1, 2: TCSETS, TCSETSW, TCSETSF
    if !(0..=2).contains(&when) {
        errno::set(errno::EINVAL);
        return -1;
    }
    ret_i(sc3(sys::IOCTL, fd as isize as usize, TCSETS + when as usize, t as usize))
}

#[no_mangle]
pub unsafe extern "C" fn tcgetpgrp(fd: c_int) -> pid_t {
    let mut pg: c_int = 0;
    if ret(sc3(sys::IOCTL, fd as isize as usize, TIOCGPGRP, &mut pg as *mut c_int as usize)) < 0 {
        return -1;
    }
    pg
}

#[no_mangle]
pub unsafe extern "C" fn tcsetpgrp(fd: c_int, pg: pid_t) -> c_int {
    ret_i(sc3(sys::IOCTL, fd as isize as usize, TIOCSPGRP, &pg as *const c_int as usize))
}

static mut TTYNAME: [u8; 64] = [0; 64];

/// The terminal's path, from the /proc/self/fd link.
#[no_mangle]
pub unsafe extern "C" fn ttyname(fd: c_int) -> *mut c_char {
    let buf = &mut *ptr::addr_of_mut!(TTYNAME);
    if ttyname_r(fd, buf.as_mut_ptr() as *mut c_char, buf.len()) != 0 {
        return ptr::null_mut();
    }
    buf.as_mut_ptr() as *mut c_char
}

#[no_mangle]
pub unsafe extern "C" fn ttyname_r(fd: c_int, buf: *mut c_char, n: size_t) -> c_int {
    if crate::imp::file::isatty(fd) == 0 {
        return errno::ENOTTY;
    }
    let mut path = *b"/proc/self/fd/0000000000\0";
    let mut v = fd as u32;
    let mut digits = [0u8; 10];
    let mut k = 0;
    loop {
        digits[k] = b'0' + (v % 10) as u8;
        k += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in 0..k {
        path[14 + i] = digits[k - 1 - i];
    }
    path[14 + k] = 0;
    let r = sc3(sys::READLINK, path.as_ptr() as usize, buf as usize, n.saturating_sub(1));
    if r < 0 {
        return (-r) as c_int;
    }
    if r as usize >= n.saturating_sub(1) {
        return errno::ERANGE;
    }
    *buf.add(r as usize) = 0;
    0
}
