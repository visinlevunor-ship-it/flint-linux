//! POSIX threads: create, join, detach, exit, self, attributes, names and keys.
//!
//! A thread is one mapping: a guard page at the bottom, the stack, then its TLS area and
//! thread block at the top. The kernel clears `Thread::tid` and wakes it when the thread
//! dies (CLONE_CHILD_CLEARTID); join waits for that, then unmaps. A detached thread unmaps
//! its own mapping on the way out, in assembly, after telling the kernel not to touch `tid`.

use crate::imp::errno::*;
use crate::imp::lock::Lock;
use crate::imp::sys::{self, sc1, sc2, sc3, sc4, sc6};
use crate::imp::tls::{self, Thread, KEYS_MAX};
use crate::imp::types::*;
use crate::imp::{mem, start};
use core::ptr;
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};

pub type pthread_t = usize;
pub type pthread_key_t = c_uint;

/// Our view of musl's 56-byte pthread_attr_t.
#[repr(C)]
pub struct Attr {
    stack_size: usize,
    guard_size: usize,
    stack_addr: usize,
    detached: c_int,
    _pad: [u8; 28],
}

const DEFAULT_STACK: usize = 2 * 1024 * 1024;
const STACK_MIN: usize = 16 * 1024;

core::arch::global_asm!(
    // pid_t __flint_clone(flags, stack, ptid, ctid, tls, func, arg)
    ".globl __flint_clone",
    ".type __flint_clone,@function",
    "__flint_clone:",
    "mov r11, qword ptr [rsp + 8]", // arg (7th argument, on the stack)
    "and rsi, -16",
    "sub rsi, 16",
    "mov qword ptr [rsi], r9",      // func
    "mov qword ptr [rsi + 8], r11", // arg
    "mov r10, rcx",
    "mov eax, 56", // clone
    "syscall",
    "test eax, eax",
    "jnz 1f",
    // the new thread, on its own stack
    "xor ebp, ebp",
    "pop r9",
    "pop rdi",
    "call r9",
    "mov edi, eax",
    "mov eax, 60", // exit (this thread only)
    "syscall",
    "hlt",
    "1:",
    "ret",
    // void __flint_unmap_exit(void *base, size_t size): never returns, touches no stack
    ".globl __flint_unmap_exit",
    ".type __flint_unmap_exit,@function",
    "__flint_unmap_exit:",
    "mov eax, 11", // munmap
    "syscall",
    "xor edi, edi",
    "mov eax, 60", // exit
    "syscall",
    "hlt",
);

extern "C" {
    fn __flint_clone(flags: usize, stack: usize, ptid: usize, ctid: usize, tls: usize, func: usize, arg: usize) -> isize;
    fn __flint_unmap_exit(base: usize, size: usize) -> !;
}

static THREADS: AtomicUsize = AtomicUsize::new(1);

#[no_mangle]
pub unsafe extern "C" fn pthread_attr_init(a: *mut Attr) -> c_int {
    ptr::write_bytes(a as *mut u8, 0, core::mem::size_of::<Attr>());
    (*a).stack_size = DEFAULT_STACK;
    (*a).guard_size = 4096;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_destroy(_a: *mut Attr) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setstacksize(a: *mut Attr, size: size_t) -> c_int {
    if size < STACK_MIN {
        return EINVAL;
    }
    (*a).stack_size = size;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getstacksize(a: *const Attr, size: *mut size_t) -> c_int {
    *size = (*a).stack_size;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getguardsize(a: *const Attr, size: *mut size_t) -> c_int {
    *size = (*a).guard_size;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setguardsize(a: *mut Attr, size: size_t) -> c_int {
    (*a).guard_size = size;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getstack(a: *const Attr, addr: *mut *mut c_void, size: *mut size_t) -> c_int {
    *addr = (*a).stack_addr as *mut c_void;
    *size = (*a).stack_size;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setdetachstate(a: *mut Attr, state: c_int) -> c_int {
    (*a).detached = state;
    0
}

/// The attributes of a running thread (std uses them to find its stack's guard page).
#[no_mangle]
pub unsafe extern "C" fn pthread_getattr_np(t: pthread_t, a: *mut Attr) -> c_int {
    pthread_attr_init(a);
    let th = t as *mut Thread;
    if th == tls::main_thread() {
        // the main stack grows down from just above the auxiliary vector, up to RLIMIT_STACK
        let page = start::PAGE_SIZE.load(Ordering::Relaxed);
        let top = (start::AUXV.load(Ordering::Relaxed) + page) & !(page - 1);
        let mut lim = [0u64; 2];
        let size = if sc4(sys::PRLIMIT64, 0, 3 /* RLIMIT_STACK */, 0, lim.as_mut_ptr() as usize) == 0
            && lim[0] != u64::MAX
        {
            lim[0] as usize
        } else {
            8 << 20
        };
        (*a).stack_addr = top - size;
        (*a).stack_size = size;
        (*a).guard_size = page;
    } else {
        (*a).stack_addr = (*th).stack_lo;
        (*a).stack_size = (*th).stack_size;
        (*a).guard_size = (*th).guard_size;
    }
    0
}

unsafe extern "C" fn thread_start(t: *mut Thread) -> c_int {
    let f = (*t).start.unwrap_unchecked();
    let r = f((*t).arg);
    pthread_exit(r)
}

#[no_mangle]
pub unsafe extern "C" fn pthread_create(
    out: *mut pthread_t,
    attr: *const Attr,
    func: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
) -> c_int {
    let page = start::PAGE_SIZE.load(Ordering::Relaxed);
    let (mut stack, mut guard, mut detached) = (DEFAULT_STACK, page, false);
    if let Some(a) = attr.as_ref() {
        stack = a.stack_size.max(STACK_MIN);
        guard = a.guard_size;
        detached = a.detached != 0;
    }
    stack = (stack + page - 1) & !(page - 1);
    guard = (guard + page - 1) & !(page - 1);
    let (block, _) = tls::block_size();
    let size = guard + stack + ((block + page - 1) & !(page - 1));
    let base = sc6(sys::MMAP, 0, size, 3, 0x22 | 0x20000 /* MAP_STACK */, usize::MAX, 0);
    if base < 0 {
        return EAGAIN;
    }
    let base = base as usize;
    if guard > 0 {
        sc3(sys::MPROTECT, base, guard, 0);
    }
    let t = tls::setup_block(base + guard + stack);
    (*t).map_base = base;
    (*t).map_size = size;
    (*t).stack_lo = base + guard;
    (*t).stack_size = stack;
    (*t).guard_size = guard;
    (*t).start = Some(func);
    (*t).arg = arg;
    (*tls::current()).multiple_threads = 1;
    THREADS.fetch_add(1, Ordering::Relaxed);
    const FLAGS: usize = 0x100 /* VM */ | 0x200 /* FS */ | 0x400 /* FILES */ | 0x800 /* SIGHAND */
        | 0x10000 /* THREAD */ | 0x40000 /* SYSVSEM */ | 0x80000 /* SETTLS */
        | 0x100000 /* PARENT_SETTID */ | 0x200000 /* CHILD_CLEARTID */;
    let tid_addr = &(*t).tid as *const AtomicI32 as usize;
    let r = __flint_clone(FLAGS, base + guard + stack, tid_addr, tid_addr, t as usize, thread_start as *const () as usize, t as usize);
    if r < 0 {
        THREADS.fetch_sub(1, Ordering::Relaxed);
        sc2(sys::MUNMAP, base, size);
        return EAGAIN;
    }
    *out = t as pthread_t;
    if detached {
        pthread_detach(t as pthread_t);
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_self() -> pthread_t {
    tls::current() as pthread_t
}

#[no_mangle]
pub unsafe extern "C" fn pthread_equal(a: pthread_t, b: pthread_t) -> c_int {
    (a == b) as c_int
}

/// Wait for a thread's kernel task to be gone (the kernel zeroes tid and wakes it).
unsafe fn wait_dead(t: *mut Thread) {
    loop {
        let tid = (*t).tid.load(Ordering::Acquire);
        if tid == 0 {
            return;
        }
        // FUTEX_WAIT, shared: the kernel's exit-time wake is not a private futex operation
        sc4(sys::FUTEX, &(*t).tid as *const AtomicI32 as usize, 0, tid as u32 as usize, 0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_join(th: pthread_t, ret: *mut *mut c_void) -> c_int {
    let t = th as *mut Thread;
    if t == tls::current() {
        return 35; // EDEADLK
    }
    wait_dead(t);
    if !ret.is_null() {
        *ret = (*t).result;
    }
    sc2(sys::MUNMAP, (*t).map_base, (*t).map_size);
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_detach(th: pthread_t) -> c_int {
    let t = th as *mut Thread;
    // 0 -> 1: it will clean up after itself. If it already finished (2), clean up now.
    if (*t).state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return pthread_join(th, ptr::null_mut());
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_exit(result: *mut c_void) -> ! {
    let t = tls::current();
    run_key_destructors(t);
    (*t).result = result;
    THREADS.fetch_sub(1, Ordering::Relaxed);
    if t == tls::main_thread() {
        // the main thread ending the process this way is not supported: just end it
        sc1(sys::EXIT, 0);
    }
    if (*t).state.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        // joinable: the joiner unmaps once the kernel says the task is gone
        loop {
            sc1(sys::EXIT, 0);
        }
    }
    // detached: block signals (a handler must not run on a stack that is going away), make
    // the kernel forget tid (it lives in the mapping), then unmap and exit without a stack
    let all: u64 = !0;
    sc4(sys::RT_SIGPROCMASK, 0 /* SIG_BLOCK */, &all as *const u64 as usize, 0, 8);
    sc1(sys::SET_TID_ADDRESS, 0);
    __flint_unmap_exit((*t).map_base, (*t).map_size)
}

#[no_mangle]
pub unsafe extern "C" fn pthread_setname_np(th: pthread_t, name: *const c_char) -> c_int {
    let n = mem::cstrlen(name as *const u8);
    if n > 15 {
        return ERANGE;
    }
    if th as *mut Thread == tls::current() {
        sc2(sys::PRCTL, 15 /* PR_SET_NAME */, name as usize);
        return 0;
    }
    // another thread: /proc/self/task/<tid>/comm
    let mut path = *b"/proc/self/task/0000000000/comm\0";
    let mut tid = (*(th as *mut Thread)).tid.load(Ordering::Relaxed) as u32;
    for i in (16..26).rev() {
        path[i] = b'0' + (tid % 10) as u8;
        tid /= 10;
    }
    let fd = sc3(sys::OPEN, path.as_ptr() as usize, 1 /* O_WRONLY */ | 0o2000000, 0);
    if fd < 0 {
        return (-fd) as c_int;
    }
    sc3(sys::WRITE, fd as usize, name as usize, n);
    sc1(sys::CLOSE, fd as usize);
    0
}

// --- thread-specific data ---

struct Key {
    used: bool,
    dtor: Option<unsafe extern "C" fn(*mut c_void)>,
}

static KEY_LOCK: Lock = Lock::new();
const NO_KEY: Key = Key { used: false, dtor: None };
static mut KEYS: [Key; KEYS_MAX] = [NO_KEY; KEYS_MAX];
/// Bumped each time a key is created, so values left from a deleted key are not seen.
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU32 = AtomicU32::new(0);
static KEY_GEN: [AtomicU32; KEYS_MAX] = [ZERO; KEYS_MAX];

#[no_mangle]
pub unsafe extern "C" fn pthread_key_create(key: *mut pthread_key_t, dtor: Option<unsafe extern "C" fn(*mut c_void)>) -> c_int {
    let _g = KEY_LOCK.guard();
    let keys = &mut *ptr::addr_of_mut!(KEYS);
    for (i, k) in keys.iter_mut().enumerate() {
        if !k.used {
            k.used = true;
            k.dtor = dtor;
            KEY_GEN[i].fetch_add(1, Ordering::Relaxed);
            *key = i as pthread_key_t;
            return 0;
        }
    }
    EAGAIN
}

#[no_mangle]
pub unsafe extern "C" fn pthread_key_delete(key: pthread_key_t) -> c_int {
    let _g = KEY_LOCK.guard();
    let k = &mut (*ptr::addr_of_mut!(KEYS))[key as usize % KEYS_MAX];
    k.used = false;
    k.dtor = None;
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_setspecific(key: pthread_key_t, v: *const c_void) -> c_int {
    let k = key as usize;
    if k >= KEYS_MAX {
        return EINVAL;
    }
    let t = tls::current();
    (*t).keys[k] = v as *mut c_void;
    (*t).key_gen[k] = KEY_GEN[k].load(Ordering::Relaxed);
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_getspecific(key: pthread_key_t) -> *mut c_void {
    let k = key as usize;
    if k >= KEYS_MAX {
        return ptr::null_mut();
    }
    let t = tls::current();
    if (*t).key_gen[k] != KEY_GEN[k].load(Ordering::Relaxed) {
        return ptr::null_mut();
    }
    (*t).keys[k]
}

/// Up to four rounds, as POSIX allows: a destructor may set new values.
unsafe fn run_key_destructors(t: *mut Thread) {
    for _ in 0..4 {
        let mut any = false;
        for k in 0..KEYS_MAX {
            let v = (*t).keys[k];
            if v.is_null() || (*t).key_gen[k] != KEY_GEN[k].load(Ordering::Relaxed) {
                continue;
            }
            let dtor = {
                let _g = KEY_LOCK.guard();
                let key = &(*ptr::addr_of!(KEYS))[k];
                if key.used { key.dtor } else { None }
            };
            (*t).keys[k] = ptr::null_mut();
            if let Some(d) = dtor {
                d(v);
                any = true;
            }
        }
        if !any {
            break;
        }
    }
}

/// In the child after fork: one thread left.
pub fn after_fork() {
    THREADS.store(1, Ordering::Relaxed);
    KEY_LOCK.reset();
    // SAFETY: we are the only thread
    unsafe {
        let t = tls::current();
        (*t).tid.store(sys::sc0(sys::GETTID) as i32, Ordering::Relaxed);
    }
}
