//! _FORTIFY_SOURCE's checked functions: the same as the plain ones, after checking that the
//! destination buffer (whose size the compiler knew) is big enough.

use super::start::__chk_fail;
use super::string::*;
use crate::fl;
use core::ffi::c_void;

type c_char = u8;
type c_int = i32;
type size_t = usize;

#[inline]
fn check(need: usize, have: usize) {
    if need > have {
        // SAFETY: does not return.
        unsafe { __chk_fail() }
    }
}

#[no_mangle]
pub unsafe extern "C" fn __memcpy_chk(d: *mut c_void, s: *const c_void, n: size_t, dl: size_t) -> *mut c_void {
    check(n, dl);
    core::ptr::copy_nonoverlapping(s as *const u8, d as *mut u8, n);
    d
}
#[no_mangle]
pub unsafe extern "C" fn __memmove_chk(d: *mut c_void, s: *const c_void, n: size_t, dl: size_t) -> *mut c_void {
    check(n, dl);
    core::ptr::copy(s as *const u8, d as *mut u8, n);
    d
}
#[no_mangle]
pub unsafe extern "C" fn __mempcpy_chk(d: *mut c_void, s: *const c_void, n: size_t, dl: size_t) -> *mut c_void {
    check(n, dl);
    mempcpy(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn __memset_chk(d: *mut c_void, c: c_int, n: size_t, dl: size_t) -> *mut c_void {
    check(n, dl);
    core::ptr::write_bytes(d as *mut u8, c as u8, n);
    d
}
#[no_mangle]
pub unsafe extern "C" fn __explicit_bzero_chk(d: *mut c_void, n: size_t, dl: size_t) {
    check(n, dl);
    explicit_bzero(d, n)
}
#[no_mangle]
pub unsafe extern "C" fn __strcpy_chk(d: *mut c_char, s: *const c_char, dl: size_t) -> *mut c_char {
    check(fl::mem::cstrlen(s) + 1, dl);
    strcpy(d, s)
}
#[no_mangle]
pub unsafe extern "C" fn __stpcpy_chk(d: *mut c_char, s: *const c_char, dl: size_t) -> *mut c_char {
    check(fl::mem::cstrlen(s) + 1, dl);
    stpcpy(d, s)
}
#[no_mangle]
pub unsafe extern "C" fn __strncpy_chk(d: *mut c_char, s: *const c_char, n: size_t, dl: size_t) -> *mut c_char {
    check(n, dl);
    strncpy(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn __stpncpy_chk(d: *mut c_char, s: *const c_char, n: size_t, dl: size_t) -> *mut c_char {
    check(n, dl);
    stpncpy(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn __strcat_chk(d: *mut c_char, s: *const c_char, dl: size_t) -> *mut c_char {
    check(fl::mem::cstrlen(d) + fl::mem::cstrlen(s) + 1, dl);
    strcat(d, s)
}
#[no_mangle]
pub unsafe extern "C" fn __strncat_chk(d: *mut c_char, s: *const c_char, n: size_t, dl: size_t) -> *mut c_char {
    check(fl::mem::cstrlen(d) + strnlen(s, n) + 1, dl);
    strncat(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn __strlcpy_chk(d: *mut c_char, s: *const c_char, n: size_t, dl: size_t) -> size_t {
    check(n, dl);
    strlcpy(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn __strlcat_chk(d: *mut c_char, s: *const c_char, n: size_t, dl: size_t) -> size_t {
    check(n, dl);
    strlcat(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn __wmemcpy_chk(d: *mut i32, s: *const i32, n: size_t, dl: size_t) -> *mut i32 {
    check(n, dl);
    core::ptr::copy_nonoverlapping(s, d, n);
    d
}
#[no_mangle]
pub unsafe extern "C" fn __wmemmove_chk(d: *mut i32, s: *const i32, n: size_t, dl: size_t) -> *mut i32 {
    check(n, dl);
    core::ptr::copy(s, d, n);
    d
}
#[no_mangle]
pub unsafe extern "C" fn __wmemset_chk(d: *mut i32, c: i32, n: size_t, dl: size_t) -> *mut i32 {
    check(n, dl);
    super::locale::wmemset(d, c, n)
}
#[no_mangle]
pub unsafe extern "C" fn __mbstowcs_chk(d: *mut i32, s: *const c_char, n: size_t, dl: size_t) -> size_t {
    check(n, dl);
    super::locale::mbstowcs(d, s, n)
}
#[no_mangle]
pub unsafe extern "C" fn __mbsrtowcs_chk(d: *mut i32, s: *mut *const c_char, n: size_t, ps: *mut super::locale::MbState, dl: size_t) -> size_t {
    check(n, dl);
    super::locale::mbsrtowcs(d, s, n, ps)
}
#[no_mangle]
pub unsafe extern "C" fn __mbsnrtowcs_chk(d: *mut i32, s: *mut *const c_char, nms: size_t, n: size_t, ps: *mut super::locale::MbState, dl: size_t) -> size_t {
    check(n, dl);
    super::locale::mbsnrtowcs(d, s, nms, n, ps)
}
#[no_mangle]
pub unsafe extern "C" fn __wcrtomb_chk(s: *mut c_char, wc: i32, ps: *mut super::locale::MbState, dl: size_t) -> size_t {
    check(4.min(dl + 1), dl);
    super::locale::wcrtomb(s, wc, ps)
}
#[no_mangle]
pub unsafe extern "C" fn __fgets_chk(s: *mut c_char, dl: size_t, n: c_int, f: *mut super::stdio::FILE) -> *mut c_char {
    check(n.max(0) as usize, dl);
    super::stdio::fgets(s, n, f)
}
#[no_mangle]
pub unsafe extern "C" fn __fgets_unlocked_chk(s: *mut c_char, dl: size_t, n: c_int, f: *mut super::stdio::FILE) -> *mut c_char {
    check(n.max(0) as usize, dl);
    super::stdio::fgets_unlocked(s, n, f)
}
#[no_mangle]
pub unsafe extern "C" fn __fread_chk(p: *mut c_void, dl: size_t, size: size_t, n: size_t, f: *mut super::stdio::FILE) -> size_t {
    check(size.saturating_mul(n), dl);
    super::stdio::fread(p, size, n, f)
}
#[no_mangle]
pub unsafe extern "C" fn __fread_unlocked_chk(p: *mut c_void, dl: size_t, size: size_t, n: size_t, f: *mut super::stdio::FILE) -> size_t {
    check(size.saturating_mul(n), dl);
    super::stdio::fread_unlocked(p, size, n, f)
}
#[no_mangle]
pub unsafe extern "C" fn __read_chk(fd: c_int, buf: *mut c_void, n: size_t, dl: size_t) -> isize {
    check(n, dl);
    fl::file::read(fd, buf, n)
}
#[no_mangle]
pub unsafe extern "C" fn __pread_chk(fd: c_int, buf: *mut c_void, n: size_t, off: i64, dl: size_t) -> isize {
    check(n, dl);
    fl::file::pread(fd, buf, n, off)
}
#[no_mangle]
pub unsafe extern "C" fn __pread64_chk(fd: c_int, buf: *mut c_void, n: size_t, off: i64, dl: size_t) -> isize {
    __pread_chk(fd, buf, n, off, dl)
}
#[no_mangle]
pub unsafe extern "C" fn __recv_chk(fd: c_int, buf: *mut c_void, n: size_t, dl: size_t, flags: c_int) -> isize {
    check(n, dl);
    fl::net::recv(fd, buf, n, flags)
}
#[no_mangle]
pub unsafe extern "C" fn __recvfrom_chk(fd: c_int, buf: *mut c_void, n: size_t, dl: size_t, flags: c_int, a: *mut c_void, al: *mut u32) -> isize {
    check(n, dl);
    fl::net::recvfrom(fd, buf, n, flags, a as *mut _, al as *mut _)
}
#[no_mangle]
pub unsafe extern "C" fn __readlink_chk(p: *const c_char, buf: *mut c_char, n: size_t, dl: size_t) -> isize {
    check(n, dl);
    fl::file::readlink(p as *const _, buf as *mut _, n)
}
#[no_mangle]
pub unsafe extern "C" fn __readlinkat_chk(fd: c_int, p: *const c_char, buf: *mut c_char, n: size_t, dl: size_t) -> isize {
    check(n, dl);
    fl::file::readlinkat(fd, p as *const _, buf as *mut _, n)
}
#[no_mangle]
pub unsafe extern "C" fn __getcwd_chk(buf: *mut c_char, n: size_t, dl: size_t) -> *mut c_char {
    check(n, dl);
    fl::file::getcwd(buf as *mut _, n) as *mut c_char
}
#[no_mangle]
pub unsafe extern "C" fn __realpath_chk(p: *const c_char, out: *mut c_char, dl: size_t) -> *mut c_char {
    check(4096, dl);
    fl::file::realpath(p as *const _, out as *mut _) as *mut c_char
}
#[no_mangle]
pub unsafe extern "C" fn __getgroups_chk(n: c_int, list: *mut u32, dl: size_t) -> c_int {
    check(n.max(0) as usize * 4, dl);
    fl::process::getgroups(n, list as *mut _)
}
#[no_mangle]
pub unsafe extern "C" fn __poll_chk(fds: *mut c_void, n: u64, t: c_int, dl: size_t) -> c_int {
    check(n as usize * 8, dl);
    fl::file::poll(fds as *mut _, n, t)
}
#[no_mangle]
pub unsafe extern "C" fn __ppoll_chk(fds: *mut c_void, n: u64, t: *const c_void, s: *const c_void, dl: size_t) -> c_int {
    check(n as usize * 8, dl);
    super::sysfns::ppoll(fds, n, t, s)
}
#[no_mangle]
pub unsafe extern "C" fn __fdelt_chk(d: i64) -> i64 {
    if !(0..1024).contains(&d) {
        __chk_fail();
    }
    d / 64
}
#[no_mangle]
pub unsafe extern "C" fn __fdelt_warn(d: i64) -> i64 {
    __fdelt_chk(d)
}
#[no_mangle]
pub unsafe extern "C" fn __open_2(p: *const c_char, flags: c_int) -> c_int {
    fl::file::open(p as *const _, flags, 0)
}
#[no_mangle]
pub unsafe extern "C" fn __open64_2(p: *const c_char, flags: c_int) -> c_int {
    fl::file::open(p as *const _, flags, 0)
}
#[no_mangle]
pub unsafe extern "C" fn __openat_2(fd: c_int, p: *const c_char, flags: c_int) -> c_int {
    fl::file::openat(fd, p as *const _, flags, 0)
}
#[no_mangle]
pub unsafe extern "C" fn __openat64_2(fd: c_int, p: *const c_char, flags: c_int) -> c_int {
    fl::file::openat(fd, p as *const _, flags, 0)
}
