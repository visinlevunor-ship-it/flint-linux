//! Users and groups beyond flint-libc's getpwuid_r/getpwnam_r: the non-reentrant forms,
//! /etc/passwd enumeration, and /etc/group (getgr*, getgrouplist, initgroups).

use crate::fl;
use alloc::vec::Vec;
use core::ptr;

type c_char = u8;
type c_int = i32;
type size_t = usize;

/// glibc's struct passwd (the same as musl's).
#[repr(C)]
pub struct Passwd {
    name: *mut c_char,
    passwd: *mut c_char,
    uid: u32,
    gid: u32,
    gecos: *mut c_char,
    dir: *mut c_char,
    shell: *mut c_char,
}

#[repr(C)]
pub struct Group {
    name: *mut c_char,
    passwd: *mut c_char,
    gid: u32,
    mem: *mut *mut c_char,
}

extern "C" {
    fn getpwuid_r(uid: u32, pw: *mut Passwd, buf: *mut c_char, n: size_t, r: *mut *mut Passwd) -> c_int;
    fn getpwnam_r(name: *const c_char, pw: *mut Passwd, buf: *mut c_char, n: size_t, r: *mut *mut Passwd) -> c_int;
}

static mut PW: Passwd = Passwd {
    name: ptr::null_mut(),
    passwd: ptr::null_mut(),
    uid: 0,
    gid: 0,
    gecos: ptr::null_mut(),
    dir: ptr::null_mut(),
    shell: ptr::null_mut(),
};
static mut PW_BUF: [u8; 1024] = [0; 1024];

#[no_mangle]
pub unsafe extern "C" fn getpwuid(uid: u32) -> *mut Passwd {
    let mut r = ptr::null_mut();
    let e = getpwuid_r(uid, ptr::addr_of_mut!(PW), ptr::addr_of_mut!(PW_BUF) as *mut c_char, 1024, &mut r);
    if e != 0 {
        fl::errno::set(e);
    }
    r
}
#[no_mangle]
pub unsafe extern "C" fn getpwnam(name: *const c_char) -> *mut Passwd {
    let mut r = ptr::null_mut();
    let e = getpwnam_r(name, ptr::addr_of_mut!(PW), ptr::addr_of_mut!(PW_BUF) as *mut c_char, 1024, &mut r);
    if e != 0 {
        fl::errno::set(e);
    }
    r
}

/// A small file read whole (the user databases are small).
unsafe fn read(path: &[u8]) -> Vec<u8> {
    let mut v = alloc::vec![0u8; 1 << 16];
    let n = fl::resolve::read_file(path, &mut v);
    v.truncate(n);
    v
}

fn num(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 10 || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    s.iter().try_fold(0u32, |a, &c| a.checked_mul(10)?.checked_add((c - b'0') as u32))
}

// --- /etc/passwd enumeration -------------------------------------------------------------------

static mut PW_ENT: usize = 0;

#[no_mangle]
pub unsafe extern "C" fn setpwent() {
    PW_ENT = 0;
}
#[no_mangle]
pub unsafe extern "C" fn endpwent() {
    PW_ENT = 0;
}
#[no_mangle]
pub unsafe extern "C" fn getpwent() -> *mut Passwd {
    let data = read(b"/etc/passwd\0");
    let lines: Vec<&[u8]> = data.split(|&c| c == b'\n').filter(|l| l.splitn(7, |&c| c == b':').count() == 7).collect();
    while PW_ENT < lines.len() {
        let l = lines[PW_ENT];
        PW_ENT += 1;
        let name = l.split(|&c| c == b':').next().unwrap_or(b"");
        let mut n: Vec<u8> = name.to_vec();
        n.push(0);
        let r = getpwnam(n.as_ptr());
        if !r.is_null() {
            return r;
        }
    }
    ptr::null_mut()
}

// --- /etc/group --------------------------------------------------------------------------------

enum Key<'a> {
    Gid(u32),
    Name(&'a [u8]),
    Index(usize),
}

/// Fill `gr` from the matching line, its strings and member list in `buf`.
unsafe fn group_lookup(key: Key, gr: *mut Group, buf: *mut c_char, buflen: size_t, result: *mut *mut Group) -> c_int {
    *result = ptr::null_mut();
    let data = read(b"/etc/group\0");
    let mut idx = 0;
    for line in data.split(|&c| c == b'\n') {
        let f: Vec<&[u8]> = line.splitn(4, |&c| c == b':').collect();
        if f.len() != 4 {
            continue;
        }
        let Some(gid) = num(f[2]) else { continue };
        let hit = match key {
            Key::Gid(g) => g == gid,
            Key::Name(n) => n == f[0],
            Key::Index(i) => {
                idx += 1;
                i + 1 == idx
            }
        };
        if !hit {
            continue;
        }
        let members: Vec<&[u8]> = f[3].split(|&c| c == b',').filter(|m| !m.is_empty()).collect();
        // pointer array first (aligned), then the strings
        let base = buf as usize;
        let aligned = (base + 7) & !7;
        let ptrs = (members.len() + 1) * 8;
        let strs: usize = f[0].len() + 1 + f[1].len() + 1 + members.iter().map(|m| m.len() + 1).sum::<usize>();
        if aligned - base + ptrs + strs > buflen {
            return 34; // ERANGE
        }
        let arr = aligned as *mut *mut c_char;
        let mut p = (aligned + ptrs) as *mut u8;
        let mut put = |s: &[u8]| -> *mut c_char {
            let st = p;
            ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            *p.add(s.len()) = 0;
            p = p.add(s.len() + 1);
            st
        };
        (*gr).name = put(f[0]);
        (*gr).passwd = put(f[1]);
        (*gr).gid = gid;
        for (i, m) in members.iter().enumerate() {
            *arr.add(i) = put(m);
        }
        *arr.add(members.len()) = ptr::null_mut();
        (*gr).mem = arr;
        *result = gr;
        return 0;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn getgrgid_r(gid: u32, gr: *mut Group, buf: *mut c_char, n: size_t, r: *mut *mut Group) -> c_int {
    group_lookup(Key::Gid(gid), gr, buf, n, r)
}
#[no_mangle]
pub unsafe extern "C" fn getgrnam_r(name: *const c_char, gr: *mut Group, buf: *mut c_char, n: size_t, r: *mut *mut Group) -> c_int {
    group_lookup(Key::Name(fl::mem::cstr(name)), gr, buf, n, r)
}

static mut GR: Group = Group { name: ptr::null_mut(), passwd: ptr::null_mut(), gid: 0, mem: ptr::null_mut() };
static mut GR_BUF: [u8; 4096] = [0; 4096];
static mut GR_ENT: usize = 0;

unsafe fn group_static(key: Key) -> *mut Group {
    let mut r = ptr::null_mut();
    let e = group_lookup(key, ptr::addr_of_mut!(GR), ptr::addr_of_mut!(GR_BUF) as *mut c_char, 4096, &mut r);
    if e != 0 {
        fl::errno::set(e);
    }
    r
}
#[no_mangle]
pub unsafe extern "C" fn getgrgid(gid: u32) -> *mut Group {
    group_static(Key::Gid(gid))
}
#[no_mangle]
pub unsafe extern "C" fn getgrnam(name: *const c_char) -> *mut Group {
    group_static(Key::Name(fl::mem::cstr(name)))
}
#[no_mangle]
pub unsafe extern "C" fn setgrent() {
    GR_ENT = 0;
}
#[no_mangle]
pub unsafe extern "C" fn endgrent() {
    GR_ENT = 0;
}
#[no_mangle]
pub unsafe extern "C" fn getgrent() -> *mut Group {
    let r = group_static(Key::Index(GR_ENT));
    GR_ENT += 1;
    r
}

/// The groups `user` belongs to: `group` first, then every /etc/group line listing the user.
unsafe fn groups_of(user: &[u8], group: u32) -> Vec<u32> {
    let mut out = alloc::vec![group];
    let data = read(b"/etc/group\0");
    for line in data.split(|&c| c == b'\n') {
        let f: Vec<&[u8]> = line.splitn(4, |&c| c == b':').collect();
        if f.len() != 4 {
            continue;
        }
        let Some(gid) = num(f[2]) else { continue };
        if f[3].split(|&c| c == b',').any(|m| m == user) && !out.contains(&gid) {
            out.push(gid);
        }
    }
    out
}

#[no_mangle]
pub unsafe extern "C" fn getgrouplist(user: *const c_char, group: u32, groups: *mut u32, ngroups: *mut c_int) -> c_int {
    let g = groups_of(fl::mem::cstr(user), group);
    let room = (*ngroups).max(0) as usize;
    for (i, &x) in g.iter().take(room).enumerate() {
        *groups.add(i) = x;
    }
    let n = g.len() as c_int;
    let fits = g.len() <= room;
    *ngroups = n;
    if fits { n } else { -1 }
}

#[no_mangle]
pub unsafe extern "C" fn initgroups(user: *const c_char, group: u32) -> c_int {
    let g = groups_of(fl::mem::cstr(user), group);
    fl::sys::ret_i(fl::sys::sc2(116 /* setgroups */, g.len(), g.as_ptr() as usize))
}
