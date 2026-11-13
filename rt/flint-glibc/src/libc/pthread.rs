//! pthread synchronisation with glibc's object sizes and static initialisers: mutexes,
//! condition variables, read-write locks, once, barriers, spin locks, semaphores; plus the
//! thread functions flint-libc does not have (scheduling, signals, cancellation, atfork).
//!
//! The parts of each object a program can set without calling a function (glibc's static
//! initialisers) are where glibc keeps them: a mutex's kind at offset 16, a read-write
//! lock's flags at offset 48. Everything else is laid out here.

use crate::fl;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

type c_int = i32;
type pthread_t = usize;

const EBUSY: c_int = 16;
const EINVAL: c_int = 22;
const EDEADLK: c_int = 35;
const EPERM: c_int = 1;
const ETIMEDOUT: c_int = 110;
const EAGAIN: c_int = 11;
const EINTR: c_int = 4;

#[repr(C)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

unsafe fn tid() -> i32 {
    (*fl::tls::current()).tid.load(Ordering::Relaxed)
}

const FUTEX_WAIT: usize = 0;
const FUTEX_WAKE: usize = 1;
const FUTEX_WAIT_BITSET: usize = 9;
const FUTEX_PRIVATE: usize = 128;
const FUTEX_CLOCK_REALTIME: usize = 256;

/// Wait while *a == val; `abs` is an absolute deadline on `clock` (0 realtime, 1 monotonic).
/// Returns 0, ETIMEDOUT or EINTR/EAGAIN.
unsafe fn futex_wait(a: *const AtomicU32, val: u32, abs: *const Timespec, clock: c_int, shared: bool) -> c_int {
    let op = FUTEX_WAIT_BITSET | if shared { 0 } else { FUTEX_PRIVATE } | if clock == 0 && !abs.is_null() { FUTEX_CLOCK_REALTIME } else { 0 };
    let r = if abs.is_null() {
        fl::sys::sc4(fl::sys::FUTEX, a as usize, FUTEX_WAIT | if shared { 0 } else { FUTEX_PRIVATE }, val as usize, 0)
    } else {
        fl::sys::sc6(fl::sys::FUTEX, a as usize, op, val as usize, abs as usize, 0, 0xffff_ffff)
    };
    match r {
        0 => 0,
        -110 => ETIMEDOUT,
        -4 => EINTR,
        _ => EAGAIN,
    }
}

unsafe fn futex_wake(a: *const AtomicU32, n: i32, shared: bool) {
    fl::sys::sc3(fl::sys::FUTEX, a as usize, FUTEX_WAKE | if shared { 0 } else { FUTEX_PRIVATE }, n as usize);
}

fn valid_ts(ts: *const Timespec) -> bool {
    // SAFETY: caller passes a pointer to a timespec (or null).
    ts.is_null() || unsafe { (*ts).tv_nsec >= 0 && (*ts).tv_nsec < 1_000_000_000 }
}

// --- mutexes (40 bytes) -----------------------------------------------------------------------

#[repr(C)]
pub struct Mutex {
    lock: AtomicU32, // 0 free, 1 locked, 2 locked with waiters
    count: u32,
    owner: AtomicI32,
    nusers: u32,
    kind: c_int, // glibc: offset 16
    spins: i16,
    elision: i16,
    _list: [usize; 2],
}

const MUTEX_NORMAL: c_int = 0;
const MUTEX_RECURSIVE: c_int = 1;
const MUTEX_ERRORCHECK: c_int = 2;
const KIND_MASK: c_int = 3;
const KIND_PSHARED: c_int = 128;

#[repr(C)]
pub struct MutexAttr {
    v: c_int, // kind | pshared(128) | robust(16) | protocol<<12
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_init(a: *mut MutexAttr) -> c_int {
    (*a).v = 0;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_destroy(_a: *mut MutexAttr) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_settype(a: *mut MutexAttr, t: c_int) -> c_int {
    if !(0..=3).contains(&t) {
        return EINVAL;
    }
    (*a).v = ((*a).v & !KIND_MASK) | t;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_gettype(a: *const MutexAttr, t: *mut c_int) -> c_int {
    *t = (*a).v & KIND_MASK;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_setpshared(a: *mut MutexAttr, p: c_int) -> c_int {
    (*a).v = if p != 0 { (*a).v | KIND_PSHARED } else { (*a).v & !KIND_PSHARED };
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_getpshared(a: *const MutexAttr, p: *mut c_int) -> c_int {
    *p = ((*a).v & KIND_PSHARED != 0) as c_int;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_setrobust(a: *mut MutexAttr, r: c_int) -> c_int {
    (*a).v = if r != 0 { (*a).v | 16 } else { (*a).v & !16 };
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_setprotocol(_a: *mut MutexAttr, _p: c_int) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_getprotocol(_a: *const MutexAttr, p: *mut c_int) -> c_int {
    *p = 0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_init(m: *mut Mutex, a: *const MutexAttr) -> c_int {
    ptr::write_bytes(m as *mut u8, 0, 40);
    if !a.is_null() {
        (*m).kind = (*a).v & (KIND_MASK | KIND_PSHARED);
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_destroy(_m: *mut Mutex) -> c_int {
    0
}

unsafe fn mutex_lock(m: *mut Mutex, abs: *const Timespec, clock: c_int) -> c_int {
    let kind = (*m).kind & KIND_MASK;
    let me = tid();
    if kind != MUTEX_NORMAL && (*m).owner.load(Ordering::Relaxed) == me {
        if kind == MUTEX_RECURSIVE {
            if (*m).count == u32::MAX {
                return EAGAIN;
            }
            (*m).count += 1;
            return 0;
        }
        return EDEADLK;
    }
    let shared = (*m).kind & KIND_PSHARED != 0;
    if (*m).lock.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        let mut spins = 0;
        loop {
            if spins < 100 {
                if (*m).lock.load(Ordering::Relaxed) == 0
                    && (*m).lock.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok()
                {
                    break;
                }
                spins += 1;
                core::hint::spin_loop();
                continue;
            }
            if (*m).lock.swap(2, Ordering::Acquire) == 0 {
                break;
            }
            if !valid_ts(abs) {
                return EINVAL;
            }
            if futex_wait(&(*m).lock, 2, abs, clock, shared) == ETIMEDOUT {
                return ETIMEDOUT;
            }
        }
    }
    (*m).owner.store(me, Ordering::Relaxed);
    (*m).count = 1;
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_lock(m: *mut Mutex) -> c_int {
    mutex_lock(m, ptr::null(), 0)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_timedlock(m: *mut Mutex, abs: *const Timespec) -> c_int {
    mutex_lock(m, abs, 0)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_clocklock(m: *mut Mutex, clock: c_int, abs: *const Timespec) -> c_int {
    mutex_lock(m, abs, clock)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_trylock(m: *mut Mutex) -> c_int {
    let kind = (*m).kind & KIND_MASK;
    let me = tid();
    if kind == MUTEX_RECURSIVE && (*m).owner.load(Ordering::Relaxed) == me {
        (*m).count += 1;
        return 0;
    }
    if (*m).lock.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return EBUSY;
    }
    (*m).owner.store(me, Ordering::Relaxed);
    (*m).count = 1;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_unlock(m: *mut Mutex) -> c_int {
    let kind = (*m).kind & KIND_MASK;
    if kind != MUTEX_NORMAL {
        if (*m).owner.load(Ordering::Relaxed) != tid() {
            return EPERM;
        }
        if kind == MUTEX_RECURSIVE && (*m).count > 1 {
            (*m).count -= 1;
            return 0;
        }
    }
    (*m).owner.store(0, Ordering::Relaxed);
    (*m).count = 0;
    if (*m).lock.swap(0, Ordering::Release) == 2 {
        futex_wake(&(*m).lock, 1, (*m).kind & KIND_PSHARED != 0);
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_consistent(_m: *mut Mutex) -> c_int {
    0
}

// --- condition variables (48 bytes) ---------------------------------------------------------

#[repr(C)]
pub struct Cond {
    seq: AtomicU32,
    _pad: u32,
    clock: c_int, // set by pthread_cond_init; 0 (realtime) for the static initialiser
    shared: c_int,
    _rest: [u8; 32],
}

#[repr(C)]
pub struct CondAttr {
    v: c_int, // bit 0: pshared, bits 1..: clock
}

#[no_mangle]
pub unsafe extern "C" fn pthread_condattr_init(a: *mut CondAttr) -> c_int {
    (*a).v = 0;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_condattr_destroy(_a: *mut CondAttr) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_condattr_setclock(a: *mut CondAttr, c: c_int) -> c_int {
    if c != 0 && c != 1 {
        return EINVAL;
    }
    (*a).v = ((*a).v & 1) | (c << 1);
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_condattr_getclock(a: *const CondAttr, c: *mut c_int) -> c_int {
    *c = (*a).v >> 1;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_condattr_setpshared(a: *mut CondAttr, p: c_int) -> c_int {
    (*a).v = ((*a).v & !1) | (p != 0) as c_int;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_condattr_getpshared(a: *const CondAttr, p: *mut c_int) -> c_int {
    *p = (*a).v & 1;
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_cond_init(c: *mut Cond, a: *const CondAttr) -> c_int {
    ptr::write_bytes(c as *mut u8, 0, 48);
    if !a.is_null() {
        (*c).clock = (*a).v >> 1;
        (*c).shared = (*a).v & 1;
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_destroy(_c: *mut Cond) -> c_int {
    0
}

unsafe fn cond_wait(c: *mut Cond, m: *mut Mutex, abs: *const Timespec, clock: c_int) -> c_int {
    if !valid_ts(abs) {
        return EINVAL;
    }
    let seq = (*c).seq.load(Ordering::Acquire);
    // a recursive mutex held several times is released fully and taken back as it was
    let saved = (*m).count;
    (*m).count = 1;
    pthread_mutex_unlock(m);
    let r = futex_wait(&(*c).seq, seq, abs, clock, (*c).shared != 0);
    // relock (marking it contended: others may be waiting after a broadcast)
    let mut rr = mutex_lock(m, ptr::null(), 0);
    if rr == 0 {
        (*m).count = saved.max(1);
    }
    if r == ETIMEDOUT {
        rr = ETIMEDOUT;
    }
    rr
}

#[no_mangle]
pub unsafe extern "C" fn pthread_cond_wait(c: *mut Cond, m: *mut Mutex) -> c_int {
    cond_wait(c, m, ptr::null(), 0)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_timedwait(c: *mut Cond, m: *mut Mutex, abs: *const Timespec) -> c_int {
    cond_wait(c, m, abs, (*c).clock)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_clockwait(c: *mut Cond, m: *mut Mutex, clock: c_int, abs: *const Timespec) -> c_int {
    cond_wait(c, m, abs, clock)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_signal(c: *mut Cond) -> c_int {
    (*c).seq.fetch_add(1, Ordering::Release);
    futex_wake(&(*c).seq, 1, (*c).shared != 0);
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_cond_broadcast(c: *mut Cond) -> c_int {
    (*c).seq.fetch_add(1, Ordering::Release);
    futex_wake(&(*c).seq, i32::MAX, (*c).shared != 0);
    0
}

// --- read-write locks (56 bytes) --------------------------------------------------------------

#[repr(C)]
pub struct RwLock {
    state: AtomicU32, // bit 31: a writer holds it; low bits: number of readers
    seq: AtomicU32,   // changes on every release; waiters sleep on it
    waiting_writers: AtomicU32,
    _pad: [u8; 36],
    flags: c_int, // glibc: offset 48 (writer preference)
    shared: c_int,
}

const WRITER: u32 = 1 << 31;

#[repr(C)]
pub struct RwLockAttr {
    v: [c_int; 2],
}

#[no_mangle]
pub unsafe extern "C" fn pthread_rwlockattr_init(a: *mut RwLockAttr) -> c_int {
    (*a).v = [0, 0];
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlockattr_destroy(_a: *mut RwLockAttr) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlockattr_setkind_np(a: *mut RwLockAttr, k: c_int) -> c_int {
    (*a).v[0] = k;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlockattr_setpshared(a: *mut RwLockAttr, p: c_int) -> c_int {
    (*a).v[1] = p;
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_init(l: *mut RwLock, a: *const RwLockAttr) -> c_int {
    ptr::write_bytes(l as *mut u8, 0, 56);
    if !a.is_null() {
        (*l).flags = (*a).v[0];
        (*l).shared = (*a).v[1];
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_destroy(_l: *mut RwLock) -> c_int {
    0
}

unsafe fn rd_lock(l: *mut RwLock, abs: *const Timespec, clock: c_int, try_: bool) -> c_int {
    loop {
        let s = (*l).state.load(Ordering::Relaxed);
        // writer preference: new readers wait while a writer is waiting (unless it would
        // deadlock a reader that already holds it: glibc's default prefers readers)
        let writer_waiting = (*l).flags == 2 && (*l).waiting_writers.load(Ordering::Relaxed) > 0;
        if s & WRITER == 0 && !writer_waiting {
            if (*l).state.compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return 0;
            }
            continue;
        }
        if try_ {
            return EBUSY;
        }
        let q = (*l).seq.load(Ordering::Acquire);
        if (*l).state.load(Ordering::Relaxed) != s {
            continue;
        }
        if futex_wait(&(*l).seq, q, abs, clock, (*l).shared != 0) == ETIMEDOUT {
            return ETIMEDOUT;
        }
    }
}

unsafe fn wr_lock(l: *mut RwLock, abs: *const Timespec, clock: c_int, try_: bool) -> c_int {
    let mut counted = false;
    let r = loop {
        let s = (*l).state.load(Ordering::Relaxed);
        if s == 0 {
            if (*l).state.compare_exchange_weak(0, WRITER, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                break 0;
            }
            continue;
        }
        if try_ {
            break EBUSY;
        }
        if !counted {
            (*l).waiting_writers.fetch_add(1, Ordering::Relaxed);
            counted = true;
        }
        let q = (*l).seq.load(Ordering::Acquire);
        if (*l).state.load(Ordering::Relaxed) != s {
            continue;
        }
        if futex_wait(&(*l).seq, q, abs, clock, (*l).shared != 0) == ETIMEDOUT {
            break ETIMEDOUT;
        }
    };
    if counted {
        (*l).waiting_writers.fetch_sub(1, Ordering::Relaxed);
    }
    r
}

#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_rdlock(l: *mut RwLock) -> c_int {
    rd_lock(l, ptr::null(), 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_tryrdlock(l: *mut RwLock) -> c_int {
    rd_lock(l, ptr::null(), 0, true)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_timedrdlock(l: *mut RwLock, abs: *const Timespec) -> c_int {
    rd_lock(l, abs, 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_clockrdlock(l: *mut RwLock, c: c_int, abs: *const Timespec) -> c_int {
    rd_lock(l, abs, c, false)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_wrlock(l: *mut RwLock) -> c_int {
    wr_lock(l, ptr::null(), 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_trywrlock(l: *mut RwLock) -> c_int {
    wr_lock(l, ptr::null(), 0, true)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_timedwrlock(l: *mut RwLock, abs: *const Timespec) -> c_int {
    wr_lock(l, abs, 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_clockwrlock(l: *mut RwLock, c: c_int, abs: *const Timespec) -> c_int {
    wr_lock(l, abs, c, false)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_unlock(l: *mut RwLock) -> c_int {
    let s = (*l).state.load(Ordering::Relaxed);
    if s & WRITER != 0 {
        (*l).state.store(0, Ordering::Release);
    } else {
        (*l).state.fetch_sub(1, Ordering::Release);
    }
    (*l).seq.fetch_add(1, Ordering::Release);
    futex_wake(&(*l).seq, i32::MAX, (*l).shared != 0);
    0
}

// --- once -----------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn pthread_once(o: *mut AtomicU32, f: unsafe extern "C" fn()) -> c_int {
    // 0 not run, 1 running, 2 running with waiters, 3 done
    loop {
        match (*o).load(Ordering::Acquire) {
            3 => return 0,
            0 => {
                if (*o).compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    f();
                    if (*o).swap(3, Ordering::Release) == 2 {
                        futex_wake(o, i32::MAX, false);
                    }
                    return 0;
                }
            }
            1 => {
                let _ = (*o).compare_exchange(1, 2, Ordering::Relaxed, Ordering::Relaxed);
            }
            _ => {
                futex_wait(o, 2, ptr::null(), 0, false);
            }
        }
    }
}

// --- barriers (32 bytes) ---------------------------------------------------------------------

#[repr(C)]
pub struct Barrier {
    count: u32,
    waiting: AtomicU32,
    seq: AtomicU32,
    lock: AtomicU32,
    _pad: [u8; 16],
}

#[no_mangle]
pub unsafe extern "C" fn pthread_barrier_init(b: *mut Barrier, _a: *const c_void, n: u32) -> c_int {
    if n == 0 {
        return EINVAL;
    }
    ptr::write_bytes(b as *mut u8, 0, 32);
    (*b).count = n;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_barrier_destroy(_b: *mut Barrier) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_barrier_wait(b: *mut Barrier) -> c_int {
    let seq = (*b).seq.load(Ordering::Acquire);
    let arrived = (*b).waiting.fetch_add(1, Ordering::AcqRel) + 1;
    if arrived == (*b).count {
        (*b).waiting.store(0, Ordering::Relaxed);
        (*b).seq.fetch_add(1, Ordering::Release);
        futex_wake(&(*b).seq, i32::MAX, false);
        return -1; // PTHREAD_BARRIER_SERIAL_THREAD
    }
    while (*b).seq.load(Ordering::Acquire) == seq {
        futex_wait(&(*b).seq, seq, ptr::null(), 0, false);
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_barrierattr_init(a: *mut c_int) -> c_int {
    *a = 0;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_barrierattr_destroy(_a: *mut c_int) -> c_int {
    0
}

// --- spin locks ----------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn pthread_spin_init(l: *mut AtomicU32, _p: c_int) -> c_int {
    (*l).store(0, Ordering::Relaxed);
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_spin_destroy(_l: *mut AtomicU32) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_spin_lock(l: *mut AtomicU32) -> c_int {
    while (*l).compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_spin_trylock(l: *mut AtomicU32) -> c_int {
    if (*l).compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() { 0 } else { EBUSY }
}
#[no_mangle]
pub unsafe extern "C" fn pthread_spin_unlock(l: *mut AtomicU32) -> c_int {
    (*l).store(0, Ordering::Release);
    0
}

// --- semaphores (32 bytes) ----------------------------------------------------------------

#[repr(C)]
pub struct Sem {
    value: AtomicU32,
    waiters: AtomicU32,
    shared: c_int,
    _pad: [u8; 20],
}

#[no_mangle]
pub unsafe extern "C" fn sem_init(s: *mut Sem, shared: c_int, v: u32) -> c_int {
    if v > i32::MAX as u32 {
        fl::errno::set(EINVAL);
        return -1;
    }
    ptr::write_bytes(s as *mut u8, 0, 32);
    (*s).value.store(v, Ordering::Relaxed);
    (*s).shared = shared;
    0
}
#[no_mangle]
pub unsafe extern "C" fn sem_destroy(_s: *mut Sem) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn sem_post(s: *mut Sem) -> c_int {
    (*s).value.fetch_add(1, Ordering::Release);
    if (*s).waiters.load(Ordering::Relaxed) > 0 {
        futex_wake(&(*s).value, 1, (*s).shared != 0);
    }
    0
}
unsafe fn sem_wait_impl(s: *mut Sem, abs: *const Timespec, clock: c_int, try_: bool) -> c_int {
    loop {
        let v = (*s).value.load(Ordering::Relaxed);
        if v > 0 {
            if (*s).value.compare_exchange_weak(v, v - 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return 0;
            }
            continue;
        }
        if try_ {
            fl::errno::set(EAGAIN);
            return -1;
        }
        if !valid_ts(abs) {
            fl::errno::set(EINVAL);
            return -1;
        }
        (*s).waiters.fetch_add(1, Ordering::Relaxed);
        let r = futex_wait(&(*s).value, 0, abs, clock, (*s).shared != 0);
        (*s).waiters.fetch_sub(1, Ordering::Relaxed);
        if r == ETIMEDOUT || r == EINTR {
            fl::errno::set(r);
            return -1;
        }
    }
}
#[no_mangle]
pub unsafe extern "C" fn sem_wait(s: *mut Sem) -> c_int {
    sem_wait_impl(s, ptr::null(), 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn sem_trywait(s: *mut Sem) -> c_int {
    sem_wait_impl(s, ptr::null(), 0, true)
}
#[no_mangle]
pub unsafe extern "C" fn sem_timedwait(s: *mut Sem, abs: *const Timespec) -> c_int {
    sem_wait_impl(s, abs, 0, false)
}
#[no_mangle]
pub unsafe extern "C" fn sem_clockwait(s: *mut Sem, c: c_int, abs: *const Timespec) -> c_int {
    sem_wait_impl(s, abs, c, false)
}
#[no_mangle]
pub unsafe extern "C" fn sem_getvalue(s: *mut Sem, v: *mut c_int) -> c_int {
    *v = (*s).value.load(Ordering::Relaxed) as c_int;
    0
}

// --- threads: attributes, scheduling, signals ---------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setinheritsched(_a: *mut c_void, _i: c_int) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getinheritsched(_a: *const c_void, i: *mut c_int) -> c_int {
    *i = 0;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setschedparam(_a: *mut c_void, _p: *const c_int) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getschedparam(_a: *const c_void, p: *mut c_int) -> c_int {
    *p = 0;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setschedpolicy(_a: *mut c_void, _p: c_int) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getschedpolicy(_a: *const c_void, p: *mut c_int) -> c_int {
    *p = 0;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setscope(_a: *mut c_void, s: c_int) -> c_int {
    if s == 0 { 0 } else { 95 /* ENOTSUP */ }
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getscope(_a: *const c_void, s: *mut c_int) -> c_int {
    *s = 0;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getdetachstate(a: *const c_void, d: *mut c_int) -> c_int {
    // flint-libc's attr: stack_size, guard_size, stack_addr, detached
    *d = *((a as *const u8).add(24) as *const c_int);
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setstack(a: *mut c_void, addr: *mut c_void, size: usize) -> c_int {
    // the thread's own stack is always ours; take the size
    let _ = addr;
    fl::thread::pthread_attr_setstacksize(a as *mut fl::thread::Attr, size)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setaffinity_np(_a: *mut c_void, _n: usize, _s: *const c_void) -> c_int {
    0
}

unsafe fn thread_tid(t: pthread_t) -> i32 {
    (*(t as *mut fl::tls::Thread)).tid.load(Ordering::Relaxed)
}

#[no_mangle]
pub unsafe extern "C" fn pthread_getschedparam(t: pthread_t, policy: *mut c_int, param: *mut c_int) -> c_int {
    let tid = thread_tid(t);
    let p = fl::sys::sc1(145 /* sched_getscheduler */, tid as usize);
    if p < 0 {
        return -p as c_int;
    }
    *policy = p as c_int;
    let r = fl::sys::sc2(143 /* sched_getparam */, tid as usize, param as usize);
    if r < 0 { -r as c_int } else { 0 }
}
#[no_mangle]
pub unsafe extern "C" fn pthread_setschedparam(t: pthread_t, policy: c_int, param: *const c_int) -> c_int {
    let r = fl::sys::sc3(144 /* sched_setscheduler */, thread_tid(t) as usize, policy as usize, param as usize);
    if r < 0 { -r as c_int } else { 0 }
}
#[no_mangle]
pub unsafe extern "C" fn pthread_setschedprio(t: pthread_t, prio: c_int) -> c_int {
    let p = prio;
    let r = fl::sys::sc2(142 /* sched_setparam */, thread_tid(t) as usize, &p as *const c_int as usize);
    if r < 0 { -r as c_int } else { 0 }
}
#[no_mangle]
pub unsafe extern "C" fn pthread_getaffinity_np(t: pthread_t, n: usize, set: *mut c_void) -> c_int {
    let r = fl::sys::sc3(fl::sys::SCHED_GETAFFINITY, thread_tid(t) as usize, n, set as usize);
    if r < 0 {
        return -r as c_int;
    }
    if (r as usize) < n {
        ptr::write_bytes((set as *mut u8).add(r as usize), 0, n - r as usize);
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_setaffinity_np(t: pthread_t, n: usize, set: *const c_void) -> c_int {
    let r = fl::sys::sc3(203 /* sched_setaffinity */, thread_tid(t) as usize, n, set as usize);
    if r < 0 { -r as c_int } else { 0 }
}
#[no_mangle]
pub unsafe extern "C" fn pthread_getcpuclockid(t: pthread_t, c: *mut c_int) -> c_int {
    *c = ((!thread_tid(t)) << 3) | 6;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_kill(t: pthread_t, sig: c_int) -> c_int {
    let tid = thread_tid(t);
    if tid == 0 {
        return 3; // ESRCH
    }
    let r = fl::sys::sc3(234 /* tgkill */, fl::sys::sc0(39) as usize, tid as usize, sig as usize);
    if r < 0 { -r as c_int } else { 0 }
}
#[no_mangle]
pub unsafe extern "C" fn pthread_getname_np(t: pthread_t, buf: *mut u8, n: usize) -> c_int {
    let mut path = *b"/proc/self/task/0000000000/comm\0";
    let mut tid = thread_tid(t) as u32;
    for i in (16..26).rev() {
        path[i] = b'0' + (tid % 10) as u8;
        tid /= 10;
    }
    let fd = fl::sys::sc3(fl::sys::OPEN, path.as_ptr() as usize, 0o2000000, 0);
    if fd < 0 {
        return -fd as c_int;
    }
    let r = fl::sys::sc3(fl::sys::READ, fd as usize, buf as usize, n);
    fl::sys::sc1(fl::sys::CLOSE, fd as usize);
    if r <= 0 {
        return 5;
    }
    let mut l = r as usize;
    if l > 0 && *buf.add(l - 1) == b'\n' {
        l -= 1;
    }
    if l >= n {
        return 34;
    }
    *buf.add(l) = 0;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_yield() -> c_int {
    fl::sys::sc0(24 /* sched_yield */);
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_getconcurrency() -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_setconcurrency(_n: c_int) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_tryjoin_np(t: pthread_t, ret: *mut *mut c_void) -> c_int {
    if thread_tid(t) != 0 {
        return EBUSY;
    }
    fl::thread::pthread_join(t, ret)
}
#[no_mangle]
pub unsafe extern "C" fn pthread_timedjoin_np(t: pthread_t, ret: *mut *mut c_void, _abs: *const Timespec) -> c_int {
    fl::thread::pthread_join(t, ret)
}

// --- cancellation: threads are never cancelled asynchronously here --------------------

#[no_mangle]
pub unsafe extern "C" fn pthread_setcancelstate(s: c_int, old: *mut c_int) -> c_int {
    if !old.is_null() {
        *old = 0;
    }
    let _ = s;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_setcanceltype(t: c_int, old: *mut c_int) -> c_int {
    if !old.is_null() {
        *old = 0;
    }
    let _ = t;
    0
}
#[no_mangle]
pub unsafe extern "C" fn pthread_testcancel() {}
#[no_mangle]
pub unsafe extern "C" fn pthread_cancel(_t: pthread_t) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn __pthread_register_cancel(_buf: *mut c_void) {}
#[no_mangle]
pub unsafe extern "C" fn __pthread_unregister_cancel(_buf: *mut c_void) {}
#[no_mangle]
pub unsafe extern "C" fn __pthread_unwind_next(_buf: *mut c_void) -> ! {
    fl::start::abort()
}

#[no_mangle]
pub unsafe extern "C" fn __pthread_key_create(k: *mut u32, d: Option<unsafe extern "C" fn(*mut c_void)>) -> c_int {
    fl::thread::pthread_key_create(k, d)
}

// --- fork handlers ---------------------------------------------------------------------------

type Hook = Option<unsafe extern "C" fn()>;
static mut ATFORK: Vec<(Hook, Hook, Hook)> = Vec::new();
static ATFORK_LOCK: fl::lock::Lock = fl::lock::Lock::new();

unsafe fn run_prepare() {
    let l = (*ptr::addr_of!(ATFORK)).clone();
    for (p, _, _) in l.iter().rev() {
        if let Some(f) = p {
            f();
        }
    }
}
unsafe fn run_parent() {
    let l = (*ptr::addr_of!(ATFORK)).clone();
    for (_, p, _) in l.iter() {
        if let Some(f) = p {
            f();
        }
    }
}
unsafe fn run_child() {
    let l = (*ptr::addr_of!(ATFORK)).clone();
    for (_, _, c) in l.iter() {
        if let Some(f) = c {
            f();
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_atfork(prepare: Hook, parent: Hook, child: Hook) -> c_int {
    let _g = ATFORK_LOCK.guard();
    (*ptr::addr_of_mut!(ATFORK)).push((prepare, parent, child));
    fl::process::FORK_HOOKS = (Some(run_prepare), Some(run_parent), Some(run_child));
    0
}
#[no_mangle]
pub unsafe extern "C" fn __register_atfork(prepare: Hook, parent: Hook, child: Hook, _dso: *mut c_void) -> c_int {
    pthread_atfork(prepare, parent, child)
}

#[no_mangle]
pub unsafe extern "C" fn __sched_cpucount(size: usize, set: *const u8) -> c_int {
    let mut n = 0;
    for i in 0..size {
        n += (*set.add(i)).count_ones() as c_int;
    }
    n
}
