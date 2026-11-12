//! opendir, fdopendir, readdir, closedir, dirfd.

use crate::imp::sys::{self, ret, sc2, sc3, sc4};
use crate::imp::types::*;
use crate::imp::{errno, malloc};
use core::ptr;

const BUF: usize = 8192;

#[repr(C)]
pub struct Dir {
    fd: c_int,
    pos: usize,
    end: usize,
    buf: [u8; BUF],
}

const O_RDONLY: usize = 0;
const O_DIRECTORY: usize = 0o200000;
const O_CLOEXEC: usize = 0o2000000;

#[no_mangle]
pub unsafe extern "C" fn fdopendir(fd: c_int) -> *mut Dir {
    let mut st = [0u64; 18];
    if ret(sc2(sys::FSTAT, fd as isize as usize, st.as_mut_ptr() as usize)) < 0 {
        return ptr::null_mut();
    }
    let mode = (st[3] & 0xffff_ffff) as u32; // st_mode is the low half of the 4th word
    if mode & 0o170000 != 0o040000 {
        errno::set(errno::ENOTDIR);
        return ptr::null_mut();
    }
    let d = malloc::malloc(core::mem::size_of::<Dir>()) as *mut Dir;
    if d.is_null() {
        return d;
    }
    (*d).fd = fd;
    (*d).pos = 0;
    (*d).end = 0;
    sc3(sys::FCNTL, fd as isize as usize, 2 /* F_SETFD */, 1 /* FD_CLOEXEC */);
    d
}

#[no_mangle]
pub unsafe extern "C" fn opendir(path: *const c_char) -> *mut Dir {
    let fd = ret(sc4(sys::OPENAT, -100isize as usize, path as usize, O_RDONLY | O_DIRECTORY | O_CLOEXEC, 0));
    if fd < 0 {
        return ptr::null_mut();
    }
    let d = fdopendir(fd as c_int);
    if d.is_null() {
        sys::sc1(sys::CLOSE, fd as usize);
    }
    d
}

#[no_mangle]
pub unsafe extern "C" fn readdir(d: *mut Dir) -> *mut c_void {
    let d = &mut *d;
    if d.pos >= d.end {
        let n = sc3(sys::GETDENTS64, d.fd as isize as usize, d.buf.as_mut_ptr() as usize, BUF);
        if n <= 0 {
            if n < 0 && n != -(errno::ENOENT as isize) {
                errno::set((-n) as i32);
            }
            return ptr::null_mut(); // end of directory (errno untouched) or error
        }
        d.pos = 0;
        d.end = n as usize;
    }
    // linux_dirent64 { u64 ino; i64 off; u16 reclen; u8 type; char name[]; } is musl's dirent
    let e = d.buf.as_mut_ptr().add(d.pos);
    let reclen = ptr::read_unaligned(e.add(16) as *const u16) as usize;
    d.pos += reclen;
    e as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn closedir(d: *mut Dir) -> c_int {
    let fd = (*d).fd;
    malloc::free(d as *mut u8);
    ret(sys::sc1(sys::CLOSE, fd as isize as usize)) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn dirfd(d: *mut Dir) -> c_int {
    (*d).fd
}

#[no_mangle]
pub unsafe extern "C" fn rewinddir(d: *mut Dir) {
    sys::sc3(sys::LSEEK, (*d).fd as isize as usize, 0, 0);
    (*d).pos = 0;
    (*d).end = 0;
}
