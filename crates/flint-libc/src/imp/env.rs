//! The environment: environ, getenv, setenv, unsetenv, clearenv.

use crate::imp::{errno, lock::Lock, malloc, mem};
use core::ptr;

#[no_mangle]
pub static mut environ: *mut *mut u8 = ptr::null_mut();

static LOCK: Lock = Lock::new();
/// The array environ points at, when setenv made it (so it can be grown in place).
static mut OWNED: *mut *mut u8 = ptr::null_mut();
static mut OWNED_CAP: usize = 0;

pub unsafe fn init(envp: *mut *mut u8) {
    environ = envp;
}

unsafe fn find(name: &[u8]) -> Option<usize> {
    let env = environ;
    if env.is_null() {
        return None;
    }
    let mut i = 0;
    loop {
        let e = *env.add(i);
        if e.is_null() {
            return None;
        }
        let entry = mem::cstr(e);
        if entry.len() > name.len() && entry[name.len()] == b'=' && &entry[..name.len()] == name {
            return Some(i);
        }
        i += 1;
    }
}

unsafe fn count() -> usize {
    let env = environ;
    let mut n = 0;
    if !env.is_null() {
        while !(*env.add(n)).is_null() {
            n += 1;
        }
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn getenv(name: *const u8) -> *mut u8 {
    let name = mem::cstr(name);
    if name.is_empty() || name.contains(&b'=') {
        return ptr::null_mut();
    }
    match find(name) {
        Some(i) => (*environ.add(i)).add(name.len() + 1),
        None => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn setenv(name: *const u8, value: *const u8, overwrite: i32) -> i32 {
    let n = mem::cstr(name);
    if n.is_empty() || n.contains(&b'=') {
        errno::set(errno::EINVAL);
        return -1;
    }
    let v = mem::cstr(value);
    let _g = LOCK.guard();
    let at = find(n);
    if at.is_some() && overwrite == 0 {
        return 0;
    }
    let s = malloc::malloc(n.len() + v.len() + 2);
    if s.is_null() {
        return -1;
    }
    ptr::copy_nonoverlapping(n.as_ptr(), s, n.len());
    *s.add(n.len()) = b'=';
    ptr::copy_nonoverlapping(v.as_ptr(), s.add(n.len() + 1), v.len());
    *s.add(n.len() + 1 + v.len()) = 0;
    // the old string is not freed: someone may still hold a pointer from getenv
    if let Some(i) = at {
        *environ.add(i) = s;
        return 0;
    }
    let len = count();
    if environ != OWNED || len + 2 > OWNED_CAP {
        let cap = (len + 2).max(16) * 2;
        let a = malloc::malloc(cap * 8) as *mut *mut u8;
        if a.is_null() {
            malloc::free(s);
            return -1;
        }
        if len > 0 {
            ptr::copy_nonoverlapping(environ, a, len);
        }
        if environ == OWNED && !OWNED.is_null() {
            malloc::free(OWNED as *mut u8);
        }
        OWNED = a;
        OWNED_CAP = cap;
        environ = a;
    }
    *environ.add(len) = s;
    *environ.add(len + 1) = ptr::null_mut();
    0
}

#[no_mangle]
pub unsafe extern "C" fn unsetenv(name: *const u8) -> i32 {
    let n = mem::cstr(name);
    if n.is_empty() || n.contains(&b'=') {
        errno::set(errno::EINVAL);
        return -1;
    }
    let _g = LOCK.guard();
    while let Some(i) = find(n) {
        let mut j = i;
        while !(*environ.add(j)).is_null() {
            *environ.add(j) = *environ.add(j + 1);
            j += 1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn clearenv() -> i32 {
    let _g = LOCK.guard();
    if !environ.is_null() {
        *environ = ptr::null_mut();
    }
    0
}
