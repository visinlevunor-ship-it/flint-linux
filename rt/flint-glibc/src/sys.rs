//! System calls for the loader, usable before anything else is set up (no errno, no TLS).

use core::arch::asm;

pub const READ: usize = 0;
pub const WRITE: usize = 1;
pub const CLOSE: usize = 3;
pub const FSTAT: usize = 5;
pub const MMAP: usize = 9;
pub const MPROTECT: usize = 10;
pub const MUNMAP: usize = 11;
pub const PREAD: usize = 17;
pub const ACCESS: usize = 21;
pub const GETPID: usize = 39;
pub const EXIT_GROUP: usize = 231;
pub const OPENAT: usize = 257;

#[inline(always)]
pub unsafe fn sc1(n: usize, a: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}
#[inline(always)]
pub unsafe fn sc2(n: usize, a: usize, b: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a, in("rsi") b, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}
#[inline(always)]
pub unsafe fn sc3(n: usize, a: usize, b: usize, c: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a, in("rsi") b, in("rdx") c,
         lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}
#[inline(always)]
pub unsafe fn sc4(n: usize, a: usize, b: usize, c: usize, d: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d,
         lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}
#[inline(always)]
pub unsafe fn sc6(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d,
         in("r8") e, in("r9") f, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}

pub fn exit_group(code: i32) -> ! {
    loop {
        // SAFETY: exit_group takes no pointers.
        unsafe { sc1(EXIT_GROUP, code as usize) };
    }
}

pub fn write_all(fd: usize, mut b: &[u8]) {
    while !b.is_empty() {
        // SAFETY: b is a valid slice.
        let n = unsafe { sc3(WRITE, fd, b.as_ptr() as usize, b.len()) };
        if n <= 0 {
            if n == -4 {
                continue; // EINTR
            }
            return;
        }
        b = &b[n as usize..];
    }
}

/// `write!` to standard error, for the loader's messages.
pub struct Stderr;

impl core::fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_all(2, s.as_bytes());
        Ok(())
    }
}

pub unsafe fn mmap(addr: usize, len: usize, prot: usize, flags: usize, fd: usize, off: usize) -> Result<usize, i32> {
    let r = sc6(MMAP, addr, len, prot, flags, fd, off);
    if r < 0 && r > -4096 {
        Err(-r as i32)
    } else {
        Ok(r as usize)
    }
}

/// Open a file read-only (close-on-exec). `path` must end in a NUL byte.
pub unsafe fn open_ro(path: &[u8]) -> Result<usize, i32> {
    const AT_FDCWD: isize = -100;
    let r = sc4(OPENAT, AT_FDCWD as usize, path.as_ptr() as usize, 0o2000000 /* O_CLOEXEC */, 0);
    if r < 0 {
        Err(-r as i32)
    } else {
        Ok(r as usize)
    }
}

pub unsafe fn pread_exact(fd: usize, buf: &mut [u8], off: usize) -> bool {
    let mut done = 0;
    while done < buf.len() {
        let r = sc4(PREAD, fd, buf.as_mut_ptr() as usize + done, buf.len() - done, off + done);
        if r == -4 {
            continue;
        }
        if r <= 0 {
            return false;
        }
        done += r as usize;
    }
    true
}

pub unsafe fn close(fd: usize) {
    sc1(CLOSE, fd);
}
