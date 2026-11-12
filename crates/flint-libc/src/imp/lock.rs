//! A small futex-based lock for flint-libc's own shared state (the heap, the environment,
//! thread keys). Uncontended: one atomic swap to take it and one to release.

use crate::imp::sys;
use core::sync::atomic::{AtomicI32, Ordering};

pub struct Lock(AtomicI32); // 0 free, 1 taken, 2 taken with waiters

impl Lock {
    pub const fn new() -> Lock {
        Lock(AtomicI32::new(0))
    }

    #[inline]
    pub fn lock(&self) {
        if self.0.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
            self.lock_slow();
        }
    }

    #[cold]
    fn lock_slow(&self) {
        let mut spins = 0;
        loop {
            // spin a little before sleeping: most holds are a few instructions long
            if spins < 100 {
                if self.0.load(Ordering::Relaxed) == 0
                    && self.0.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok()
                {
                    return;
                }
                spins += 1;
                core::hint::spin_loop();
                continue;
            }
            if self.0.swap(2, Ordering::Acquire) == 0 {
                return;
            }
            futex_wait(&self.0, 2);
        }
    }

    #[inline]
    pub fn unlock(&self) {
        if self.0.swap(0, Ordering::Release) == 2 {
            futex_wake(&self.0, 1);
        }
    }

    pub fn guard(&self) -> Guard<'_> {
        self.lock();
        Guard(self)
    }

    /// After fork, the child is the only thread: whoever held the lock is gone.
    pub fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

pub struct Guard<'a>(&'a Lock);

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.unlock();
    }
}

const FUTEX_WAIT_PRIVATE: usize = 128;
const FUTEX_WAKE_PRIVATE: usize = 129;

pub fn futex_wait(a: &AtomicI32, val: i32) {
    // SAFETY: a valid futex word; errors (EAGAIN, EINTR) just mean "check again".
    unsafe { sys::sc4(sys::FUTEX, a as *const _ as usize, FUTEX_WAIT_PRIVATE, val as u32 as usize, 0) };
}

pub fn futex_wake(a: &AtomicI32, n: i32) {
    // SAFETY: as above.
    unsafe { sys::sc3(sys::FUTEX, a as *const _ as usize, FUTEX_WAKE_PRIVATE, n as usize) };
}
