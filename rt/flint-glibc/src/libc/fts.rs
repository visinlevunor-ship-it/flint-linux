//! fts: walking file hierarchies (fts_open, fts_read, fts_children, fts_set, fts_close and
//! their *64 names), with glibc's FTS and FTSENT layouts. The walk always works with full
//! paths (as with FTS_NOCHDIR), so fts_accpath is fts_path.

use crate::fl;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;

const FTS_COMFOLLOW: c_int = 0x1;
const FTS_LOGICAL: c_int = 0x2;
const FTS_NOSTAT: c_int = 0x8;
const FTS_SEEDOT: c_int = 0x20;
const FTS_XDEV: c_int = 0x40;
const FTS_NAMEONLY: c_int = 0x100;

const FTS_D: u16 = 1;
const FTS_DC: u16 = 2;
const FTS_DEFAULT: u16 = 3;
const FTS_DNR: u16 = 4;
const FTS_DOT: u16 = 5;
const FTS_DP: u16 = 6;
const FTS_ERR: u16 = 7;
const FTS_F: u16 = 8;
const FTS_NS: u16 = 10;
const FTS_NSOK: u16 = 11;
const FTS_SL: u16 = 12;
const FTS_SLNONE: u16 = 13;

const FTS_AGAIN: u16 = 1;
const FTS_FOLLOW: u16 = 2;
const FTS_SKIP: u16 = 4;

const FTS_ROOTLEVEL: i16 = 0;

/// FTSENT (FTSENT64 has the same layout on x86-64).
#[repr(C)]
pub struct Ent {
    cycle: *mut Ent,
    parent: *mut Ent,
    link: *mut Ent,
    number: i64,
    pointer: *mut c_void,
    accpath: *mut c_char,
    path: *mut c_char,
    errno: c_int,
    symfd: c_int,
    pathlen: u16,
    namelen: u16,
    ino: u64,
    dev: u64,
    nlink: u64,
    level: i16,
    info: u16,
    flags: u16,
    instr: u16,
    statp: *mut u8,
    name: [u8; 1],
}

/// FTS, followed by our walk state.
#[repr(C)]
pub struct Fts {
    cur: *mut Ent,
    child: *mut Ent,
    array: *mut *mut Ent,
    dev: u64,
    path: *mut c_char,
    rfd: c_int,
    pathlen: c_int,
    nitems: c_int,
    compar: Option<unsafe extern "C" fn(*const *const Ent, *const *const Ent) -> c_int>,
    options: c_int,
    // ---
    stack: Vec<Frame>,
    roots: Vec<*mut Ent>,
    root_i: usize,
    all: Vec<*mut Ent>,
    started: bool,
}

struct Frame {
    dir: *mut Ent,
    children: Vec<*mut Ent>,
    next: usize,
}

const STAT_SIZE: usize = 144;

/// A new entry for `path`, whose name is the last component.
unsafe fn new_ent(path: &[u8], level: i16, parent: *mut Ent) -> *mut Ent {
    let name = match path.iter().rposition(|&c| c == b'/') {
        Some(i) if i + 1 < path.len() => &path[i + 1..],
        _ => path,
    };
    let size = core::mem::size_of::<Ent>() + name.len() + 1 + path.len() + 1 + STAT_SIZE + 8;
    let e = fl::malloc::imalloc(size) as *mut Ent;
    if e.is_null() {
        return e;
    }
    ptr::write_bytes(e as *mut u8, 0, size);
    let base = e as *mut u8;
    let name_p = ptr::addr_of_mut!((*e).name) as *mut u8;
    ptr::copy_nonoverlapping(name.as_ptr(), name_p, name.len());
    let path_p = name_p.add(name.len() + 1);
    ptr::copy_nonoverlapping(path.as_ptr(), path_p, path.len());
    let st = ((path_p.add(path.len() + 1) as usize + 7) & !7) as *mut u8;
    debug_assert!(st.add(STAT_SIZE) <= base.add(size));
    (*e).path = path_p;
    (*e).accpath = path_p;
    (*e).pathlen = path.len().min(u16::MAX as usize) as u16;
    (*e).namelen = name.len().min(u16::MAX as usize) as u16;
    (*e).level = level;
    (*e).parent = parent;
    (*e).statp = st;
    e
}

unsafe fn path_of(e: *mut Ent) -> &'static [u8] {
    fl::mem::cstr((*e).path)
}

fn mode(st: *const u8) -> u32 {
    // SAFETY: a struct stat.
    unsafe { u32::from_ne_bytes([*st.add(24), *st.add(25), *st.add(26), *st.add(27)]) }
}

/// Stat the entry and classify it.
unsafe fn classify(fts: &Fts, e: *mut Ent, follow: bool) {
    let mut p = path_of(e).to_vec();
    p.push(0);
    let st = (*e).statp;
    let r = if follow {
        fl::file::stat(p.as_ptr() as *const _, st as *mut c_void)
    } else {
        fl::file::lstat(p.as_ptr() as *const _, st as *mut c_void)
    };
    if r != 0 {
        let err = fl::errno::get();
        // a dangling symlink when following
        if follow && fl::file::lstat(p.as_ptr() as *const _, st as *mut c_void) == 0 {
            (*e).info = FTS_SLNONE;
            fill_ids(e);
            return;
        }
        (*e).errno = err;
        (*e).info = FTS_NS;
        return;
    }
    fill_ids(e);
    let m = mode(st) & 0o170000;
    (*e).info = match m {
        0o040000 => {
            let nm = fl::mem::cstr(ptr::addr_of!((*e).name) as *const u8);
            if (nm == b"." || nm == b"..") && (*e).level > FTS_ROOTLEVEL {
                FTS_DOT
            } else {
                // a cycle: an ancestor with the same device and inode
                let mut a = (*e).parent;
                while !a.is_null() && (*a).level >= FTS_ROOTLEVEL {
                    if (*a).dev == (*e).dev && (*a).ino == (*e).ino {
                        (*e).cycle = a;
                        (*e).info = FTS_DC;
                        return;
                    }
                    a = (*a).parent;
                }
                FTS_D
            }
        }
        0o120000 => FTS_SL,
        0o100000 => FTS_F,
        _ => FTS_DEFAULT,
    };
    let _ = fts;
}

unsafe fn fill_ids(e: *mut Ent) {
    let st = (*e).statp;
    (*e).dev = u64::from_ne_bytes(core::slice::from_raw_parts(st, 8).try_into().unwrap());
    (*e).ino = u64::from_ne_bytes(core::slice::from_raw_parts(st.add(8), 8).try_into().unwrap());
    (*e).nlink = u64::from_ne_bytes(core::slice::from_raw_parts(st.add(16), 8).try_into().unwrap());
}

unsafe fn sort(fts: &Fts, v: &mut Vec<*mut Ent>) {
    if let Some(c) = fts.compar {
        // a stable insertion of the comparator's order (directories are small)
        let mut i = 1;
        while i < v.len() {
            let mut j = i;
            while j > 0 && c(&(v[j - 1] as *const Ent), &(v[j] as *const Ent)) > 0 {
                v.swap(j - 1, j);
                j -= 1;
            }
            i += 1;
        }
    }
}

/// Read the children of directory entry `d`.
unsafe fn read_children(fts: &mut Fts, d: *mut Ent) -> Option<Vec<*mut Ent>> {
    let mut p = path_of(d).to_vec();
    p.push(0);
    let dir = fl::dir::opendir(p.as_ptr() as *const _);
    if dir.is_null() {
        (*d).errno = fl::errno::get();
        return None;
    }
    p.pop();
    let mut out = Vec::new();
    loop {
        let de = fl::dir::readdir(dir) as *const u8;
        if de.is_null() {
            break;
        }
        let name = fl::mem::cstr(de.add(19));
        if (name == b"." || name == b"..") && fts.options & FTS_SEEDOT == 0 {
            continue;
        }
        let mut cp = p.clone();
        if cp.last() != Some(&b'/') {
            cp.push(b'/');
        }
        cp.extend_from_slice(name);
        let e = new_ent(&cp, (*d).level + 1, d);
        if e.is_null() {
            break;
        }
        fts.all.push(e);
        if fts.options & FTS_NOSTAT != 0 && *de.add(18) != 4 /* DT_DIR */ && *de.add(18) != 0 {
            (*e).info = FTS_NSOK;
        } else {
            classify(fts, e, fts.options & FTS_LOGICAL != 0);
        }
        out.push(e);
    }
    fl::dir::closedir(dir);
    sort(fts, &mut out);
    for w in out.windows(2) {
        (*w[0]).link = w[1];
    }
    Some(out)
}

#[no_mangle]
pub unsafe extern "C" fn fts_open(paths: *const *const c_char, options: c_int, compar: Option<unsafe extern "C" fn(*const *const Ent, *const *const Ent) -> c_int>) -> *mut Fts {
    if options & !0x1ff != 0 {
        fl::errno::set(22);
        return ptr::null_mut();
    }
    let fts = Box::new(Fts {
        cur: ptr::null_mut(),
        child: ptr::null_mut(),
        array: ptr::null_mut(),
        dev: 0,
        path: ptr::null_mut(),
        rfd: -1,
        pathlen: 0,
        nitems: 0,
        compar,
        options,
        stack: Vec::new(),
        roots: Vec::new(),
        root_i: 0,
        all: Vec::new(),
        started: false,
    });
    let fts = Box::into_raw(fts);
    let f = &mut *fts;
    let mut i = 0;
    while !(*paths.add(i)).is_null() {
        let p = fl::mem::cstr(*paths.add(i));
        let e = new_ent(p, FTS_ROOTLEVEL, ptr::null_mut());
        if e.is_null() {
            break;
        }
        f.all.push(e);
        classify(f, e, options & (FTS_LOGICAL | FTS_COMFOLLOW) != 0);
        f.roots.push(e);
        i += 1;
    }
    let mut roots = core::mem::take(&mut f.roots);
    sort(f, &mut roots);
    f.roots = roots;
    fts
}

#[no_mangle]
pub unsafe extern "C" fn fts_read(fts: *mut Fts) -> *mut Ent {
    let f = &mut *fts;
    // act on the instruction left on the entry returned last
    let cur = f.cur;
    if !cur.is_null() {
        let instr = (*cur).instr;
        (*cur).instr = 0;
        if instr == FTS_AGAIN {
            classify(f, cur, false);
            return cur;
        }
        if instr == FTS_FOLLOW && ((*cur).info == FTS_SL || (*cur).info == FTS_SLNONE) {
            classify(f, cur, true);
            if (*cur).info == FTS_D || (*cur).info == FTS_SLNONE {
                return cur;
            }
        }
        if (*cur).info == FTS_D {
            if instr == FTS_SKIP || (f.options & FTS_XDEV != 0 && (*cur).dev != f.dev) {
                // skipped: not descended; the post-order visit comes right away
                (*cur).info = FTS_DP;
                return cur;
            } else {
                let kids = match read_children(f, cur) {
                    Some(k) => k,
                    None => {
                        (*cur).info = FTS_DNR;
                        f.stack.push(Frame { dir: cur, children: Vec::new(), next: 0 });
                        return cur;
                    }
                };
                f.stack.push(Frame { dir: cur, children: kids, next: 0 });
            }
        }
    } else if f.started {
        return ptr::null_mut();
    }
    f.started = true;
    loop {
        if let Some(fr) = f.stack.last_mut() {
            if fr.next < fr.children.len() {
                let e = fr.children[fr.next];
                fr.next += 1;
                f.cur = e;
                return e;
            }
            let d = fr.dir;
            f.stack.pop();
            if (*d).info == FTS_DNR {
                continue;
            }
            (*d).info = FTS_DP;
            f.cur = d;
            return d;
        }
        if f.root_i < f.roots.len() {
            let e = f.roots[f.root_i];
            f.root_i += 1;
            f.dev = (*e).dev;
            f.cur = e;
            return e;
        }
        f.cur = ptr::null_mut();
        fl::errno::set(0);
        return ptr::null_mut();
    }
}

#[no_mangle]
pub unsafe extern "C" fn fts_children(fts: *mut Fts, options: c_int) -> *mut Ent {
    let f = &mut *fts;
    if options != 0 && options != FTS_NAMEONLY {
        fl::errno::set(22);
        return ptr::null_mut();
    }
    let cur = f.cur;
    if cur.is_null() {
        // before the first read: the roots
        for w in f.roots.windows(2) {
            (*w[0]).link = w[1];
        }
        return f.roots.first().copied().unwrap_or(ptr::null_mut());
    }
    fl::errno::set(0);
    if (*cur).info != FTS_D {
        return ptr::null_mut();
    }
    match read_children(f, cur) {
        Some(k) => {
            let first = k.first().copied().unwrap_or(ptr::null_mut());
            // fts_read will use these when it descends
            f.child = first;
            first
        }
        None => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn fts_set(_fts: *mut Fts, e: *mut Ent, instr: c_int) -> c_int {
    if !(0..=4).contains(&instr) {
        fl::errno::set(22);
        return 1;
    }
    (*e).instr = instr as u16;
    0
}

#[no_mangle]
pub unsafe extern "C" fn fts_close(fts: *mut Fts) -> c_int {
    if fts.is_null() {
        return 0;
    }
    let f = Box::from_raw(fts);
    for e in f.all.iter() {
        fl::malloc::ifree(*e as *mut u8);
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn fts64_open(p: *const *const c_char, o: c_int, c: Option<unsafe extern "C" fn(*const *const Ent, *const *const Ent) -> c_int>) -> *mut Fts {
    fts_open(p, o, c)
}
#[no_mangle]
pub unsafe extern "C" fn fts64_read(f: *mut Fts) -> *mut Ent {
    fts_read(f)
}
#[no_mangle]
pub unsafe extern "C" fn fts64_children(f: *mut Fts, o: c_int) -> *mut Ent {
    fts_children(f, o)
}
#[no_mangle]
pub unsafe extern "C" fn fts64_set(f: *mut Fts, e: *mut Ent, i: c_int) -> c_int {
    fts_set(f, e, i)
}
#[no_mangle]
pub unsafe extern "C" fn fts64_close(f: *mut Fts) -> c_int {
    fts_close(f)
}

#[allow(dead_code)]
const _UNUSED: [u16; 2] = [FTS_ERR, FTS_DOT];
