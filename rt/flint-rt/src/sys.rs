//! System call wrappers. Each is a thin, typed layer over `arch::syscallN`.

use crate::arch::{syscall0, syscall1, syscall2, syscall3, syscall6};

pub const SYS_READ: usize = 0;
pub const SYS_WRITE: usize = 1;
pub const SYS_CLOSE: usize = 3;
pub const SYS_MMAP: usize = 9;
pub const SYS_MUNMAP: usize = 11;
pub const SYS_GETPID: usize = 39;
pub const SYS_EXIT: usize = 60;
pub const SYS_KILL: usize = 62;
pub const SYS_EXIT_GROUP: usize = 231;
pub const SYS_OPENAT: usize = 257;

pub const AT_FDCWD: isize = -100;
pub const O_RDONLY: usize = 0;
pub const O_CLOEXEC: usize = 0o2000000;

const PROT_READ: usize = 1;
const PROT_WRITE: usize = 2;
const MAP_PRIVATE: usize = 0x02;
const MAP_ANONYMOUS: usize = 0x20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Errno(pub i32);

impl Errno {
    pub const ENOENT: Errno = Errno(2);
    pub const EINTR: Errno = Errno(4);
    pub const EAGAIN: Errno = Errno(11);
    pub const ENOMEM: Errno = Errno(12);
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "errno {}", self.0)
    }
}

pub type Result<T> = core::result::Result<T, Errno>;

/// Raw results in -4095..=-1 are errors; everything else is a value.
#[inline]
fn check(r: isize) -> Result<usize> {
    if (-4095..0).contains(&r) {
        Err(Errno((-r) as i32))
    } else {
        Ok(r as usize)
    }
}

pub fn write(fd: i32, buf: &[u8]) -> Result<usize> {
    // SAFETY: pointer and length come from a valid slice.
    check(unsafe { syscall3(SYS_WRITE, fd as usize, buf.as_ptr() as usize, buf.len()) })
}

pub fn read(fd: i32, buf: &mut [u8]) -> Result<usize> {
    // SAFETY: pointer and length come from a valid mutable slice.
    check(unsafe { syscall3(SYS_READ, fd as usize, buf.as_mut_ptr() as usize, buf.len()) })
}

/// `path` must be NUL terminated.
pub fn open(path: &[u8], flags: usize) -> Result<i32> {
    if path.last() != Some(&0) {
        return Err(Errno(22)); // EINVAL
    }
    // SAFETY: `path` is a valid NUL-terminated byte string.
    check(unsafe { syscall3(SYS_OPENAT, AT_FDCWD as usize, path.as_ptr() as usize, flags) }).map(|fd| fd as i32)
}

pub fn close(fd: i32) -> Result<()> {
    // SAFETY: closing a descriptor has no memory safety preconditions.
    check(unsafe { syscall1(SYS_CLOSE, fd as usize) }).map(|_| ())
}

pub fn getpid() -> i32 {
    // SAFETY: getpid cannot fail.
    unsafe { syscall0(SYS_GETPID) as i32 }
}

pub fn kill(pid: i32, sig: i32) -> Result<()> {
    // SAFETY: kill has no memory safety preconditions.
    check(unsafe { syscall2(SYS_KILL, pid as usize, sig as usize) }).map(|_| ())
}

/// Anonymous private read/write mapping, zero filled, page aligned.
pub fn mmap_anon(len: usize) -> Result<*mut u8> {
    // SAFETY: a fresh anonymous mapping aliases nothing.
    let r = unsafe { syscall6(SYS_MMAP, 0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, usize::MAX, 0) };
    check(r).map(|a| a as *mut u8)
}

/// # Safety
/// `ptr`/`len` must describe a mapping from [`mmap_anon`] that is no longer used.
pub unsafe fn munmap(ptr: *mut u8, len: usize) -> Result<()> {
    check(syscall2(SYS_MUNMAP, ptr as usize, len)).map(|_| ())
}

pub fn exit(code: i32) -> ! {
    loop {
        // SAFETY: exit_group terminates the process; the loop only guards against a return.
        unsafe {
            syscall1(SYS_EXIT_GROUP, code as usize);
            syscall1(SYS_EXIT, code as usize);
        }
    }
}
