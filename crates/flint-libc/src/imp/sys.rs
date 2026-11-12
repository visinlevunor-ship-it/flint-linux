//! Raw Linux system calls on x86-64, and the numbers flint-libc uses.

// The full table, used or not: a reference for what comes next.
#![allow(dead_code)]

use core::arch::asm;

#[inline(always)]
pub unsafe fn sc0(n: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}
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
pub unsafe fn sc5(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d, in("r8") e,
         lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}
#[inline(always)]
pub unsafe fn sc6(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> isize {
    let r: isize;
    asm!("syscall", inlateout("rax") n as isize => r, in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d, in("r8") e, in("r9") f,
         lateout("rcx") _, lateout("r11") _, options(nostack));
    r
}

/// A raw result as C returns it: the value, or -1 with errno set.
#[inline]
pub fn ret(r: isize) -> isize {
    if (-4095..0).contains(&r) {
        crate::imp::errno::set((-r) as i32);
        -1
    } else {
        r
    }
}

#[inline]
pub fn ret_i(r: isize) -> i32 {
    ret(r) as i32
}

// x86-64 system call numbers (arch/x86/entry/syscalls/syscall_64.tbl)
pub const READ: usize = 0;
pub const WRITE: usize = 1;
pub const OPEN: usize = 2;
pub const CLOSE: usize = 3;
pub const STAT: usize = 4;
pub const FSTAT: usize = 5;
pub const LSTAT: usize = 6;
pub const POLL: usize = 7;
pub const LSEEK: usize = 8;
pub const MMAP: usize = 9;
pub const MPROTECT: usize = 10;
pub const MUNMAP: usize = 11;
pub const RT_SIGACTION: usize = 13;
pub const RT_SIGPROCMASK: usize = 14;
pub const RT_SIGRETURN: usize = 15;
pub const IOCTL: usize = 16;
pub const READV: usize = 19;
pub const WRITEV: usize = 20;
pub const ACCESS: usize = 21;
pub const PIPE: usize = 22;
pub const SCHED_YIELD: usize = 24;
pub const MREMAP: usize = 25;
pub const MADVISE: usize = 28;
pub const DUP: usize = 32;
pub const DUP2: usize = 33;
pub const PAUSE: usize = 34;
pub const ALARM: usize = 37;
pub const GETPID: usize = 39;
pub const SENDFILE: usize = 40;
pub const SOCKET: usize = 41;
pub const CONNECT: usize = 42;
pub const SENDTO: usize = 44;
pub const RECVFROM: usize = 45;
pub const SENDMSG: usize = 46;
pub const RECVMSG: usize = 47;
pub const BIND: usize = 49;
pub const SOCKETPAIR: usize = 53;
pub const SETSOCKOPT: usize = 54;
pub const GETSOCKOPT: usize = 55;
pub const CLONE: usize = 56;
pub const FORK: usize = 57;
pub const EXECVE: usize = 59;
pub const EXIT: usize = 60;
pub const WAIT4: usize = 61;
pub const KILL: usize = 62;
pub const UNAME: usize = 63;
pub const FCNTL: usize = 72;
pub const FSYNC: usize = 74;
pub const GETCWD: usize = 79;
pub const CHDIR: usize = 80;
pub const RENAME: usize = 82;
pub const MKDIR: usize = 83;
pub const RMDIR: usize = 84;
pub const LINK: usize = 86;
pub const UNLINK: usize = 87;
pub const SYMLINK: usize = 88;
pub const READLINK: usize = 89;
pub const CHMOD: usize = 90;
pub const FCHMOD: usize = 91;
pub const FCHOWN: usize = 93;
pub const LCHOWN: usize = 94;
pub const UMASK: usize = 95;
pub const GETRLIMIT: usize = 97;
pub const SYSLOG: usize = 103;
pub const GETUID: usize = 102;
pub const GETGID: usize = 104;
pub const SETUID: usize = 105;
pub const SETGID: usize = 106;
pub const GETEUID: usize = 107;
pub const GETEGID: usize = 108;
pub const SETPGID: usize = 109;
pub const GETPGRP: usize = 111;
pub const SETSID: usize = 112;
pub const GETGROUPS: usize = 115;
pub const SETGROUPS: usize = 116;
pub const RT_SIGTIMEDWAIT: usize = 128;
pub const SIGALTSTACK: usize = 131;
pub const MKNOD: usize = 133;
pub const STATFS: usize = 137;
pub const PRCTL: usize = 157;
pub const ARCH_PRCTL: usize = 158;
pub const CHROOT: usize = 161;
pub const SYNC: usize = 162;
pub const MOUNT: usize = 165;
pub const UMOUNT2: usize = 166;
pub const REBOOT: usize = 169;
pub const SETHOSTNAME: usize = 170;
pub const GETTID: usize = 186;
pub const LSETXATTR: usize = 189;
pub const LGETXATTR: usize = 192;
pub const LLISTXATTR: usize = 195;
pub const TKILL: usize = 200;
pub const FUTEX: usize = 202;
pub const SCHED_GETAFFINITY: usize = 204;
pub const GETDENTS64: usize = 217;
pub const SET_TID_ADDRESS: usize = 218;
pub const CLOCK_SETTIME: usize = 227;
pub const CLOCK_GETTIME: usize = 228;
pub const CLOCK_NANOSLEEP: usize = 230;
pub const EXIT_GROUP: usize = 231;
pub const TGKILL: usize = 234;
pub const WAITID: usize = 247;
pub const OPENAT: usize = 257;
pub const MKNODAT: usize = 259;
pub const NEWFSTATAT: usize = 262;
pub const UNLINKAT: usize = 263;
pub const LINKAT: usize = 265;
pub const READLINKAT: usize = 267;
pub const FCHMODAT: usize = 268;
pub const FACCESSAT: usize = 269;
pub const SPLICE: usize = 275;
pub const UTIMENSAT: usize = 280;
pub const DUP3: usize = 292;
pub const PIPE2: usize = 293;
pub const PRLIMIT64: usize = 302;
pub const GETRANDOM: usize = 318;
pub const CLOSE_RANGE: usize = 436;
