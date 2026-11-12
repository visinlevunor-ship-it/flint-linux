//! Sockets: thin wrappers over the system calls.

use crate::imp::sys::{self, ret, ret_i, sc2, sc3, sc4, sc5, sc6};
use crate::imp::types::*;

#[no_mangle]
pub unsafe extern "C" fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int {
    ret_i(sc3(sys::SOCKET, domain as usize, ty as usize, proto as usize))
}
#[no_mangle]
pub unsafe extern "C" fn socketpair(domain: c_int, ty: c_int, proto: c_int, fds: *mut c_int) -> c_int {
    ret_i(sc4(sys::SOCKETPAIR, domain as usize, ty as usize, proto as usize, fds as usize))
}
#[no_mangle]
pub unsafe extern "C" fn bind(fd: c_int, addr: *const c_void, len: socklen_t) -> c_int {
    ret_i(sc3(sys::BIND, fd as isize as usize, addr as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn connect(fd: c_int, addr: *const c_void, len: socklen_t) -> c_int {
    ret_i(sc3(sys::CONNECT, fd as isize as usize, addr as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn listen(fd: c_int, backlog: c_int) -> c_int {
    ret_i(sc2(50, fd as isize as usize, backlog as usize))
}
#[no_mangle]
pub unsafe extern "C" fn accept4(fd: c_int, addr: *mut c_void, len: *mut socklen_t, flags: c_int) -> c_int {
    ret_i(sc4(288, fd as isize as usize, addr as usize, len as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn accept(fd: c_int, addr: *mut c_void, len: *mut socklen_t) -> c_int {
    accept4(fd, addr, len, 0)
}
#[no_mangle]
pub unsafe extern "C" fn shutdown(fd: c_int, how: c_int) -> c_int {
    ret_i(sc2(48, fd as isize as usize, how as usize))
}
#[no_mangle]
pub unsafe extern "C" fn getsockname(fd: c_int, addr: *mut c_void, len: *mut socklen_t) -> c_int {
    ret_i(sc3(51, fd as isize as usize, addr as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn getpeername(fd: c_int, addr: *mut c_void, len: *mut socklen_t) -> c_int {
    ret_i(sc3(52, fd as isize as usize, addr as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn setsockopt(fd: c_int, level: c_int, name: c_int, val: *const c_void, len: socklen_t) -> c_int {
    ret_i(sc5(sys::SETSOCKOPT, fd as isize as usize, level as usize, name as usize, val as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn getsockopt(fd: c_int, level: c_int, name: c_int, val: *mut c_void, len: *mut socklen_t) -> c_int {
    ret_i(sc5(sys::GETSOCKOPT, fd as isize as usize, level as usize, name as usize, val as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn sendto(fd: c_int, buf: *const c_void, n: size_t, flags: c_int, addr: *const c_void, len: socklen_t) -> ssize_t {
    ret(sc6(sys::SENDTO, fd as isize as usize, buf as usize, n, flags as usize, addr as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn send(fd: c_int, buf: *const c_void, n: size_t, flags: c_int) -> ssize_t {
    sendto(fd, buf, n, flags, core::ptr::null(), 0)
}
#[no_mangle]
pub unsafe extern "C" fn recvfrom(fd: c_int, buf: *mut c_void, n: size_t, flags: c_int, addr: *mut c_void, len: *mut socklen_t) -> ssize_t {
    ret(sc6(sys::RECVFROM, fd as isize as usize, buf as usize, n, flags as usize, addr as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn recv(fd: c_int, buf: *mut c_void, n: size_t, flags: c_int) -> ssize_t {
    recvfrom(fd, buf, n, flags, core::ptr::null_mut(), core::ptr::null_mut())
}
#[no_mangle]
pub unsafe extern "C" fn sendmsg(fd: c_int, msg: *const c_void, flags: c_int) -> ssize_t {
    ret(sc3(sys::SENDMSG, fd as isize as usize, msg as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn recvmsg(fd: c_int, msg: *mut c_void, flags: c_int) -> ssize_t {
    ret(sc3(sys::RECVMSG, fd as isize as usize, msg as usize, flags as usize))
}
