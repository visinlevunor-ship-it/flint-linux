//! Signals: sigaction, signal, sigprocmask, sigset helpers, sigaltstack, sigtimedwait, raise,
//! kill, pause, alarm.

use crate::imp::errno;
use crate::imp::sys::{self, ret_i, sc1, sc2, sc4};
use crate::imp::types::*;

core::arch::global_asm!(
    // the kernel returns from a signal handler into this, which asks it to restore state
    ".globl __flint_restore_rt",
    ".type __flint_restore_rt,@function",
    "__flint_restore_rt:",
    "mov eax, 15", // rt_sigreturn
    "syscall",
    "hlt",
);

extern "C" {
    fn __flint_restore_rt();
}

#[repr(C)]
struct KSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

const SA_RESTORER: u64 = 0x0400_0000;
const SA_RESTART: c_int = 0x1000_0000;

#[no_mangle]
pub unsafe extern "C" fn sigaction(sig: c_int, act: *const sigaction, old: *mut sigaction) -> c_int {
    if !(1..=64).contains(&sig) {
        errno::set(errno::EINVAL);
        return -1;
    }
    let mut k = KSigaction { handler: 0, flags: 0, restorer: 0, mask: 0 };
    let kp = if let Some(a) = act.as_ref() {
        k.handler = a.handler;
        k.flags = a.flags as u32 as u64 | SA_RESTORER;
        k.restorer = __flint_restore_rt as *const () as usize;
        k.mask = a.mask.val[0];
        &k as *const KSigaction as usize
    } else {
        0
    };
    let mut ko = KSigaction { handler: 0, flags: 0, restorer: 0, mask: 0 };
    let r = sc4(sys::RT_SIGACTION, sig as usize, kp, &mut ko as *mut KSigaction as usize, 8);
    if r < 0 {
        return ret_i(r);
    }
    if let Some(o) = old.as_mut() {
        o.handler = ko.handler;
        o.flags = (ko.flags & !SA_RESTORER) as c_int;
        o.restorer = 0;
        o.mask = sigset_t { val: [0; 16] };
        o.mask.val[0] = ko.mask;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn signal(sig: c_int, handler: usize) -> usize {
    let new = sigaction { handler, mask: sigset_t { val: [0; 16] }, flags: SA_RESTART, restorer: 0 };
    let mut old = sigaction { handler: 0, mask: sigset_t { val: [0; 16] }, flags: 0, restorer: 0 };
    if sigaction(sig, &new, &mut old) < 0 {
        return usize::MAX; // SIG_ERR
    }
    old.handler
}

#[no_mangle]
pub unsafe extern "C" fn sigprocmask(how: c_int, set: *const sigset_t, old: *mut sigset_t) -> c_int {
    let s = set.as_ref().map(|s| &s.val[0] as *const u64 as usize).unwrap_or(0);
    let mut o: u64 = 0;
    let r = sc4(sys::RT_SIGPROCMASK, how as usize, s, &mut o as *mut u64 as usize, 8);
    if r < 0 {
        return ret_i(r);
    }
    if let Some(old) = old.as_mut() {
        old.val = [0; 16];
        old.val[0] = o;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_sigmask(how: c_int, set: *const sigset_t, old: *mut sigset_t) -> c_int {
    if sigprocmask(how, set, old) < 0 {
        errno::get()
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn sigemptyset(set: *mut sigset_t) -> c_int {
    (*set).val = [0; 16];
    0
}

#[no_mangle]
pub unsafe extern "C" fn sigfillset(set: *mut sigset_t) -> c_int {
    (*set).val = [0; 16];
    (*set).val[0] = !0;
    0
}

#[no_mangle]
pub unsafe extern "C" fn sigaddset(set: *mut sigset_t, sig: c_int) -> c_int {
    if !(1..=64).contains(&sig) {
        errno::set(errno::EINVAL);
        return -1;
    }
    (*set).val[0] |= 1u64 << (sig - 1);
    0
}

#[no_mangle]
pub unsafe extern "C" fn sigdelset(set: *mut sigset_t, sig: c_int) -> c_int {
    if !(1..=64).contains(&sig) {
        errno::set(errno::EINVAL);
        return -1;
    }
    (*set).val[0] &= !(1u64 << (sig - 1));
    0
}

#[no_mangle]
pub unsafe extern "C" fn sigismember(set: *const sigset_t, sig: c_int) -> c_int {
    if !(1..=64).contains(&sig) {
        return 0;
    }
    (((*set).val[0] >> (sig - 1)) & 1) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn sigaltstack(ss: *const c_void, old: *mut c_void) -> c_int {
    ret_i(sc2(sys::SIGALTSTACK, ss as usize, old as usize))
}

#[no_mangle]
pub unsafe extern "C" fn sigtimedwait(set: *const sigset_t, info: *mut c_void, timeout: *const timespec) -> c_int {
    ret_i(sc4(sys::RT_SIGTIMEDWAIT, &(*set).val[0] as *const u64 as usize, info as usize, timeout as usize, 8))
}

#[no_mangle]
pub unsafe extern "C" fn kill(pid: pid_t, sig: c_int) -> c_int {
    ret_i(sc2(sys::KILL, pid as isize as usize, sig as usize))
}

#[no_mangle]
pub unsafe extern "C" fn raise(sig: c_int) -> c_int {
    let tid = sys::sc0(sys::GETTID);
    ret_i(sc2(sys::TKILL, tid as usize, sig as usize))
}

#[no_mangle]
pub unsafe extern "C" fn pause() -> c_int {
    ret_i(sys::sc0(sys::PAUSE))
}

#[no_mangle]
pub unsafe extern "C" fn alarm(secs: c_uint) -> c_uint {
    sc1(sys::ALARM, secs as usize) as c_uint
}
