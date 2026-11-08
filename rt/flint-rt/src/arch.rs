//! Raw x86_64 Linux system calls.
//!
//! Convention: number in rax, arguments in rdi, rsi, rdx, r10, r8, r9; the
//! kernel clobbers rcx and r11; the result is in rax, where -4095..=-1 is -errno.

use core::arch::asm;

#[inline(always)]
pub unsafe fn syscall0(n: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}

#[inline(always)]
pub unsafe fn syscall1(n: usize, a1: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a1, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}

#[inline(always)]
pub unsafe fn syscall2(n: usize, a1: usize, a2: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a1, in("rsi") a2, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}

#[inline(always)]
pub unsafe fn syscall3(n: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a1, in("rsi") a2, in("rdx") a3, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}

#[allow(dead_code)] // used by upcoming wrappers (mount, ...)
#[inline(always)]
pub unsafe fn syscall4(n: usize, a1: usize, a2: usize, a3: usize, a4: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}

#[inline(always)]
pub unsafe fn syscall6(n: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize, a6: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, in("r8") a5, in("r9") a6, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}
