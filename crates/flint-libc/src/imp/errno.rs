//! errno: one per thread, kept in the thread control block.

// The full table, used or not: a reference for what comes next.
#![allow(dead_code)]

use crate::imp::tls;

pub const EPERM: i32 = 1;
pub const ENOENT: i32 = 2;
pub const ESRCH: i32 = 3;
pub const EINTR: i32 = 4;
pub const EIO: i32 = 5;
pub const EBADF: i32 = 9;
pub const ECHILD: i32 = 10;
pub const EAGAIN: i32 = 11;
pub const ENOMEM: i32 = 12;
pub const EACCES: i32 = 13;
pub const EFAULT: i32 = 14;
pub const EEXIST: i32 = 17;
pub const ENOTDIR: i32 = 20;
pub const EISDIR: i32 = 21;
pub const EINVAL: i32 = 22;
pub const ENOTTY: i32 = 25;
pub const ERANGE: i32 = 34;
pub const ENAMETOOLONG: i32 = 36;
pub const ENOSYS: i32 = 38;
pub const ELOOP: i32 = 40;
pub const ENOEXEC: i32 = 8;
pub const ETIMEDOUT: i32 = 110;

#[inline]
pub fn set(e: i32) {
    // SAFETY: the thread block exists from _start on, for every thread.
    unsafe { (*tls::current()).errno = e }
}

#[inline]
pub fn get() -> i32 {
    // SAFETY: as above.
    unsafe { (*tls::current()).errno }
}

#[no_mangle]
pub unsafe extern "C" fn __errno_location() -> *mut i32 {
    &mut (*tls::current()).errno
}
