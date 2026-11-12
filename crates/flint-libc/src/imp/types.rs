//! C types with musl's sizes on x86-64.

pub use core::ffi::{c_char, c_int, c_long, c_short, c_uint, c_ulong, c_void};

pub type size_t = usize;
pub type ssize_t = isize;
pub type off_t = i64;
pub type pid_t = i32;
pub type uid_t = u32;
pub type gid_t = u32;
pub type mode_t = u32;
pub type dev_t = u64;
pub type time_t = i64;
pub type clockid_t = i32;
pub type socklen_t = u32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// musl's sigset_t: 1024 bits, of which the kernel uses the first 64.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct sigset_t {
    pub val: [u64; 16],
}

/// musl's struct sigaction.
#[repr(C)]
pub struct sigaction {
    pub handler: usize,
    pub mask: sigset_t,
    pub flags: c_int,
    pub restorer: usize,
}

#[repr(C)]
pub struct iovec {
    pub base: *mut c_void,
    pub len: usize,
}
