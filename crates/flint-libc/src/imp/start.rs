//! Program start and exit: `_start`, the auxiliary vector, `exit`, `_exit`, `abort`.

use crate::imp::{env, lock::Lock, sys, tls};
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

// The kernel enters at _start with the stack holding argc, argv[], NULL, envp[], NULL, auxv.
core::arch::global_asm!(
    ".globl _start",
    ".type _start,@function",
    "_start:",
    "xor ebp, ebp",
    "mov rdi, rsp",
    "mov rbx, rsp", // kept across the call (callee-saved)
    "and rsp, -16",
    "call {start}",
    // main(argc, argv, envp), then exit with its result
    "mov rdi, qword ptr [rbx]",
    "lea rsi, [rbx + 8]",
    "lea rdx, [rsi + rdi*8 + 8]",
    "call main",
    "mov edi, eax",
    "call {exit}",
    "hlt",
    start = sym start_c,
    exit = sym exit,
);

pub static AUXV: AtomicUsize = AtomicUsize::new(0);
pub static PAGE_SIZE: AtomicUsize = AtomicUsize::new(4096);

extern "C" {
    static __init_array_start: [unsafe extern "C" fn(i32, *mut *mut u8, *mut *mut u8); 0];
    static __init_array_end: [unsafe extern "C" fn(i32, *mut *mut u8, *mut *mut u8); 0];
    static __preinit_array_start: [unsafe extern "C" fn(i32, *mut *mut u8, *mut *mut u8); 0];
    static __preinit_array_end: [unsafe extern "C" fn(i32, *mut *mut u8, *mut *mut u8); 0];
}

pub const AT_PHDR: usize = 3;
pub const AT_PHNUM: usize = 5;
pub const AT_PAGESZ: usize = 6;
pub const AT_RANDOM: usize = 25;

/// Everything before main: auxiliary vector, TLS, environment, constructors.
unsafe extern "C" fn start_c(sp: *const usize) {
    let argc = *sp as i32;
    let argv = sp.add(1) as *mut *mut u8;
    let envp = argv.add(argc as usize + 1);
    let mut p = envp;
    while !(*p).is_null() {
        p = p.add(1);
    }
    let auxv = p.add(1) as *const usize;
    AUXV.store(auxv as usize, Ordering::Relaxed);
    let (mut phdr, mut phnum, mut random) = (0, 0, 0);
    let mut a = auxv;
    while *a != 0 {
        match *a {
            AT_PHDR => phdr = *a.add(1),
            AT_PHNUM => phnum = *a.add(1),
            AT_PAGESZ => PAGE_SIZE.store(*a.add(1), Ordering::Relaxed),
            AT_RANDOM => random = *a.add(1),
            _ => {}
        }
        a = a.add(2);
    }
    tls::init_main(phdr, phnum, random);
    env::init(envp);
    run_array(ptr::addr_of!(__preinit_array_start) as usize, ptr::addr_of!(__preinit_array_end) as usize, argc, argv, envp);
    run_array(ptr::addr_of!(__init_array_start) as usize, ptr::addr_of!(__init_array_end) as usize, argc, argv, envp);
}

unsafe fn run_array(start: usize, end: usize, argc: i32, argv: *mut *mut u8, envp: *mut *mut u8) {
    let mut f = start;
    while f < end {
        let func: unsafe extern "C" fn(i32, *mut *mut u8, *mut *mut u8) = core::mem::transmute(*(f as *const usize));
        func(argc, argv, envp);
        f += 8;
    }
}

const ATEXIT_MAX: usize = 32;
static ATEXIT_LOCK: Lock = Lock::new();
static mut ATEXIT: [Option<unsafe extern "C" fn()>; ATEXIT_MAX] = [None; ATEXIT_MAX];
static mut ATEXIT_N: usize = 0;

#[no_mangle]
pub unsafe extern "C" fn atexit(f: unsafe extern "C" fn()) -> i32 {
    let _g = ATEXIT_LOCK.guard();
    let n = &mut *ptr::addr_of_mut!(ATEXIT_N);
    if *n == ATEXIT_MAX {
        return -1;
    }
    (*ptr::addr_of_mut!(ATEXIT))[*n] = Some(f);
    *n += 1;
    0
}

#[no_mangle]
pub unsafe extern "C" fn exit(code: i32) -> ! {
    // handlers run in reverse order of registration
    loop {
        let f = {
            let _g = ATEXIT_LOCK.guard();
            let n = &mut *ptr::addr_of_mut!(ATEXIT_N);
            if *n == 0 {
                break;
            }
            *n -= 1;
            (*ptr::addr_of!(ATEXIT))[*n]
        };
        if let Some(f) = f {
            f();
        }
    }
    _exit(code)
}

#[no_mangle]
pub unsafe extern "C" fn _exit(code: i32) -> ! {
    loop {
        sys::sc1(sys::EXIT_GROUP, code as usize);
    }
}

#[no_mangle]
pub unsafe extern "C" fn abort() -> ! {
    const SIGABRT: usize = 6;
    // unblock SIGABRT and raise it; if a handler returns, reset to default and raise again
    let set: u64 = 1 << (SIGABRT - 1);
    sys::sc4(sys::RT_SIGPROCMASK, 1 /* SIG_UNBLOCK */, &set as *const u64 as usize, 0, 8);
    crate::imp::signal::raise(SIGABRT as i32);
    let dfl = [0usize; 4];
    sys::sc4(sys::RT_SIGACTION, SIGABRT, dfl.as_ptr() as usize, 0, 8);
    crate::imp::signal::raise(SIGABRT as i32);
    loop {
        sys::sc1(sys::EXIT_GROUP, 127);
    }
}
