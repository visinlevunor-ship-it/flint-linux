//! Users: getpwuid_r, getpwnam_r (from /etc/passwd), and scheduling: sched_yield,
//! sched_getaffinity.

use crate::imp::sys::{self, ret_i, sc0, sc1, sc3};
use crate::imp::types::*;
use crate::imp::{errno, mem};
use core::ptr;

/// musl's struct passwd.
#[repr(C)]
pub struct Passwd {
    name: *mut c_char,
    passwd: *mut c_char,
    uid: uid_t,
    gid: gid_t,
    gecos: *mut c_char,
    dir: *mut c_char,
    shell: *mut c_char,
}

enum Key<'a> {
    Uid(uid_t),
    Name(&'a [u8]),
}

fn parse_num(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 10 || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    s.iter().try_fold(0u32, |a, &c| a.checked_mul(10)?.checked_add((c - b'0') as u32))
}

/// Find the entry and copy its fields into `buf`. Returns 0, ERANGE, or an errno; sets
/// *result to `pw` when found, NULL when not.
unsafe fn lookup(key: Key, pw: *mut Passwd, buf: *mut c_char, buflen: size_t, result: *mut *mut Passwd) -> c_int {
    *result = ptr::null_mut();
    let fd = sc3(sys::OPEN, b"/etc/passwd\0".as_ptr() as usize, 0o2000000, 0);
    if fd < 0 {
        return if fd == -(errno::ENOENT as isize) { 0 } else { (-fd) as c_int };
    }
    // the file is small: read it whole, a page at a time into a stack buffer per line
    let mut data = [0u8; 65536];
    let mut len = 0;
    loop {
        let r = sc3(sys::READ, fd as usize, data.as_mut_ptr().add(len) as usize, data.len() - len);
        if r <= 0 {
            break;
        }
        len += r as usize;
        if len == data.len() {
            break;
        }
    }
    sc1(sys::CLOSE, fd as usize);
    for line in data[..len].split(|&c| c == b'\n') {
        let f: [&[u8]; 7] = {
            let mut it = line.splitn(7, |&c| c == b':');
            let mut out: [&[u8]; 7] = [&[]; 7];
            let mut n = 0;
            for (o, v) in out.iter_mut().zip(&mut it) {
                *o = v;
                n += 1;
            }
            if n != 7 {
                continue;
            }
            out
        };
        let (Some(uid), Some(gid)) = (parse_num(f[2]), parse_num(f[3])) else { continue };
        let hit = match key {
            Key::Uid(u) => u == uid,
            Key::Name(n) => n == f[0],
        };
        if !hit {
            continue;
        }
        let need: usize = [0usize, 1, 4, 5, 6].iter().map(|&i| f[i].len() + 1).sum();
        if need > buflen {
            return errno::ERANGE;
        }
        let mut p = buf as *mut u8;
        let mut put = |s: &[u8]| -> *mut c_char {
            let start = p;
            ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            *p.add(s.len()) = 0;
            p = p.add(s.len() + 1);
            start as *mut c_char
        };
        (*pw).name = put(f[0]);
        (*pw).passwd = put(f[1]);
        (*pw).gecos = put(f[4]);
        (*pw).dir = put(f[5]);
        (*pw).shell = put(f[6]);
        (*pw).uid = uid;
        (*pw).gid = gid;
        *result = pw;
        return 0;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn getpwuid_r(uid: uid_t, pw: *mut Passwd, buf: *mut c_char, buflen: size_t, result: *mut *mut Passwd) -> c_int {
    lookup(Key::Uid(uid), pw, buf, buflen, result)
}

#[no_mangle]
pub unsafe extern "C" fn getpwnam_r(name: *const c_char, pw: *mut Passwd, buf: *mut c_char, buflen: size_t, result: *mut *mut Passwd) -> c_int {
    lookup(Key::Name(mem::cstr(name as *const u8)), pw, buf, buflen, result)
}

#[no_mangle]
pub unsafe extern "C" fn sched_yield() -> c_int {
    ret_i(sc0(sys::SCHED_YIELD))
}

/// Returns 0 or -1 (the kernel returns the mask size; glibc and musl return 0).
#[no_mangle]
pub unsafe extern "C" fn sched_getaffinity(pid: pid_t, size: size_t, set: *mut c_void) -> c_int {
    let r = sc3(sys::SCHED_GETAFFINITY, pid as usize, size, set as usize);
    if r < 0 {
        return ret_i(r);
    }
    // zero the part of the caller's set the kernel did not fill
    if (r as usize) < size {
        ptr::write_bytes((set as *mut u8).add(r as usize), 0, size - r as usize);
    }
    0
}
