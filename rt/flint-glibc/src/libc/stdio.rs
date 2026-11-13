//! stdio with glibc's FILE layout.
//!
//! Programs built against glibc's headers use macros that reach into a FILE: getc_unlocked
//! reads `*_IO_read_ptr++` until `_IO_read_end` and then calls __uflow; putc_unlocked writes
//! `*_IO_write_ptr++` until `_IO_write_end` and then calls __overflow; feof/ferror test
//! bits in `_flags`. So the buffer pointers here mean exactly what they mean in glibc:
//!
//!  - reading: buf_base <= read_ptr <= read_end hold unread data; write_ptr == write_end, so
//!    any putc goes through __overflow, which switches to writing;
//!  - writing: write_base..write_ptr is pending output; write_end is buf_end when fully
//!    buffered, and write_ptr (every putc is a call) when line-buffered or unbuffered;
//!    read_ptr == read_end, so any getc goes through __uflow, which switches to reading.

use super::umem;
use crate::fl;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicI32, Ordering};

type c_char = u8;
type c_int = i32;
type size_t = usize;
type off_t = i64;

pub const EOF: c_int = -1;
const BUFSIZ: usize = 8192;

const MAGIC: i32 = 0xFBAD0000u32 as i32;
const F_UNBUFFERED: i32 = 0x0002;
const F_NO_READS: i32 = 0x0004;
const F_NO_WRITES: i32 = 0x0008;
pub const F_EOF: i32 = 0x0010;
pub const F_ERR: i32 = 0x0020;
const F_IN_BACKUP: i32 = 0x0100;
const F_LINE_BUF: i32 = 0x0200;
const F_CURRENTLY_PUTTING: i32 = 0x0800;
const F_APPENDING: i32 = 0x1000;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Fd,
    Cookie,
    MemStream, // open_memstream: grows, reports through *ptr and *size
    FixedMem,  // fmemopen
}

#[repr(C)]
pub struct CookieFns {
    read: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, size_t) -> isize>,
    write: Option<unsafe extern "C" fn(*mut c_void, *const c_char, size_t) -> isize>,
    seek: Option<unsafe extern "C" fn(*mut c_void, *mut off_t, c_int) -> c_int>,
    close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

#[repr(C)]
pub struct FILE {
    // --- glibc's struct _IO_FILE (216 bytes) ---
    pub flags: i32,
    _pad0: i32,
    pub read_ptr: *mut u8,
    pub read_end: *mut u8,
    pub read_base: *mut u8,
    pub write_base: *mut u8,
    pub write_ptr: *mut u8,
    pub write_end: *mut u8,
    pub buf_base: *mut u8,
    pub buf_end: *mut u8,
    save_base: *mut u8,
    backup_base: *mut u8,
    save_end: *mut u8,
    markers: *mut c_void,
    chain: *mut FILE,
    pub fileno: i32,
    flags2: i32,
    old_offset: i64,
    cur_column: u16,
    vtable_offset: i8,
    shortbuf: [u8; 1],
    _pad1: i32,
    lock: *mut c_void,
    offset: i64,
    codecvt: *mut c_void,
    wide_data: *mut c_void,
    freeres_list: *mut c_void,
    freeres_buf: *mut c_void,
    pad5: usize,
    mode: i32,
    unused2: [u8; 20],
    // --- _IO_FILE_plus ---
    vtable: *const c_void,
    // --- flint ---
    kind: Kind,
    owns_buf: bool,
    owns_file: bool, // allocated by us (fclose frees it)
    no_close: bool,
    lock_owner: AtomicI32,
    lock_count: u32,
    lock_word: fl::lock::Lock,
    cookie: *mut c_void,
    fns: CookieFns,
    // open_memstream
    ms_ptr: *mut *mut c_char,
    ms_size: *mut size_t,
    mem: Vec<u8>,
    mem_pos: usize,
    mem_len: usize, // fmemopen: current size of the contents
    mem_user: *mut u8,
    mem_cap: usize,
    // ungetc beyond the start of the buffer
    backup: [u8; 8],
    saved: (*mut u8, *mut u8, *mut u8),
    pub pid: i32, // popen
    pub wide: i32,
}

unsafe impl Sync for FILE {}

const fn blank(fd: i32, flags: i32) -> FILE {
    FILE {
        flags: MAGIC | flags,
        _pad0: 0,
        read_ptr: ptr::null_mut(),
        read_end: ptr::null_mut(),
        read_base: ptr::null_mut(),
        write_base: ptr::null_mut(),
        write_ptr: ptr::null_mut(),
        write_end: ptr::null_mut(),
        buf_base: ptr::null_mut(),
        buf_end: ptr::null_mut(),
        save_base: ptr::null_mut(),
        backup_base: ptr::null_mut(),
        save_end: ptr::null_mut(),
        markers: ptr::null_mut(),
        chain: ptr::null_mut(),
        fileno: fd,
        flags2: 0,
        old_offset: -1,
        cur_column: 0,
        vtable_offset: 0,
        shortbuf: [0],
        _pad1: 0,
        lock: ptr::null_mut(),
        offset: -1,
        codecvt: ptr::null_mut(),
        wide_data: ptr::null_mut(),
        freeres_list: ptr::null_mut(),
        freeres_buf: ptr::null_mut(),
        pad5: 0,
        mode: 0,
        unused2: [0; 20],
        vtable: ptr::null(),
        kind: Kind::Fd,
        owns_buf: false,
        owns_file: false,
        no_close: false,
        lock_owner: AtomicI32::new(0),
        lock_count: 0,
        lock_word: fl::lock::Lock::new(),
        cookie: ptr::null_mut(),
        fns: CookieFns { read: None, write: None, seek: None, close: None },
        ms_ptr: ptr::null_mut(),
        ms_size: ptr::null_mut(),
        mem: Vec::new(),
        mem_pos: 0,
        mem_len: 0,
        mem_user: ptr::null_mut(),
        mem_cap: 0,
        backup: [0; 8],
        saved: (ptr::null_mut(), ptr::null_mut(), ptr::null_mut()),
        pid: 0,
        wide: 0,
    }
}

#[no_mangle]
pub static mut _IO_2_1_stdin_: FILE = blank(0, F_NO_WRITES);
#[no_mangle]
pub static mut _IO_2_1_stdout_: FILE = blank(1, F_NO_READS);
#[no_mangle]
pub static mut _IO_2_1_stderr_: FILE = blank(2, F_NO_READS | F_UNBUFFERED);
#[no_mangle]
pub static mut stdin: *mut FILE = unsafe { ptr::addr_of_mut!(_IO_2_1_stdin_) };
#[no_mangle]
pub static mut stdout: *mut FILE = unsafe { ptr::addr_of_mut!(_IO_2_1_stdout_) };
#[no_mangle]
pub static mut stderr: *mut FILE = unsafe { ptr::addr_of_mut!(_IO_2_1_stderr_) };

pub fn stdout_file() -> *mut FILE {
    // SAFETY: a static's address.
    unsafe { ptr::addr_of_mut!(_IO_2_1_stdout_) }
}
pub fn stderr_file() -> *mut FILE {
    // SAFETY: as above.
    unsafe { ptr::addr_of_mut!(_IO_2_1_stderr_) }
}
pub fn stdin_file() -> *mut FILE {
    // SAFETY: as above.
    unsafe { ptr::addr_of_mut!(_IO_2_1_stdin_) }
}

static LIST_LOCK: fl::lock::Lock = fl::lock::Lock::new();
static mut LIST: Vec<*mut FILE> = Vec::new();

pub unsafe fn init() {
    // stdout is line-buffered on a terminal
    let mut ws = [0u16; 4];
    if fl::sys::sc3(fl::sys::IOCTL, 1, 0x5413 /* TIOCGWINSZ */, ws.as_mut_ptr() as usize) == 0 {
        _IO_2_1_stdout_.flags |= F_LINE_BUF;
    }
    let l = &mut *ptr::addr_of_mut!(LIST);
    l.push(stdin_file());
    l.push(stdout_file());
    l.push(stderr_file());
}

// --- locking --------------------------------------------------------------------------------

unsafe fn tid() -> i32 {
    (*fl::tls::current()).tid.load(Ordering::Relaxed)
}

#[no_mangle]
pub unsafe extern "C" fn flockfile(f: *mut FILE) {
    let me = tid();
    if (*f).lock_owner.load(Ordering::Relaxed) == me {
        (*f).lock_count += 1;
        return;
    }
    (*f).lock_word.lock();
    (*f).lock_owner.store(me, Ordering::Relaxed);
    (*f).lock_count = 1;
}

#[no_mangle]
pub unsafe extern "C" fn ftrylockfile(f: *mut FILE) -> c_int {
    let me = tid();
    if (*f).lock_owner.load(Ordering::Relaxed) == me {
        (*f).lock_count += 1;
        return 0;
    }
    if !(*f).lock_word.try_lock() {
        return 1;
    }
    (*f).lock_owner.store(me, Ordering::Relaxed);
    (*f).lock_count = 1;
    0
}

#[no_mangle]
pub unsafe extern "C" fn funlockfile(f: *mut FILE) {
    (*f).lock_count -= 1;
    if (*f).lock_count == 0 {
        (*f).lock_owner.store(0, Ordering::Relaxed);
        (*f).lock_word.unlock();
    }
}

pub struct Locked(*mut FILE);
impl Drop for Locked {
    fn drop(&mut self) {
        // SAFETY: locked by `locked`.
        unsafe { funlockfile(self.0) }
    }
}
pub unsafe fn locked(f: *mut FILE) -> Locked {
    flockfile(f);
    Locked(f)
}

// --- the backends ---------------------------------------------------------------------------

unsafe fn raw_read(f: *mut FILE, buf: *mut u8, n: usize) -> isize {
    match (*f).kind {
        Kind::Fd => loop {
            let r = fl::sys::sc3(fl::sys::READ, (*f).fileno as usize, buf as usize, n);
            if r == -4 {
                continue;
            }
            if r < 0 {
                fl::errno::set(-r as i32);
                return -1;
            }
            return r;
        },
        Kind::Cookie => match (*f).fns.read {
            Some(rf) => rf((*f).cookie, buf, n),
            None => 0,
        },
        Kind::MemStream => 0,
        Kind::FixedMem => {
            let avail = (*f).mem_len.saturating_sub((*f).mem_pos);
            let k = avail.min(n);
            ptr::copy_nonoverlapping((*f).mem_user.add((*f).mem_pos), buf, k);
            (*f).mem_pos += k;
            k as isize
        }
    }
}

unsafe fn raw_write(f: *mut FILE, buf: *const u8, n: usize) -> isize {
    match (*f).kind {
        Kind::Fd => {
            let mut done = 0;
            while done < n {
                let r = fl::sys::sc3(fl::sys::WRITE, (*f).fileno as usize, buf as usize + done, n - done);
                if r == -4 {
                    continue;
                }
                if r < 0 {
                    fl::errno::set(-r as i32);
                    return if done > 0 { done as isize } else { -1 };
                }
                done += r as usize;
            }
            done as isize
        }
        Kind::Cookie => match (*f).fns.write {
            Some(wf) => wf((*f).cookie, buf, n),
            None => n as isize,
        },
        Kind::MemStream => {
            let pos = (*f).mem_pos;
            let m = &mut (*f).mem;
            if m.len() < pos + n {
                m.resize(pos + n, 0);
            }
            ptr::copy_nonoverlapping(buf, m.as_mut_ptr().add(pos), n);
            (*f).mem_pos += n;
            memstream_publish(f);
            n as isize
        }
        Kind::FixedMem => {
            let room = (*f).mem_cap.saturating_sub((*f).mem_pos);
            let k = room.min(n);
            ptr::copy_nonoverlapping(buf, (*f).mem_user.add((*f).mem_pos), k);
            (*f).mem_pos += k;
            if (*f).mem_pos > (*f).mem_len {
                (*f).mem_len = (*f).mem_pos;
                if (*f).mem_len < (*f).mem_cap {
                    *(*f).mem_user.add((*f).mem_len) = 0;
                }
            }
            k as isize
        }
    }
}

unsafe fn raw_seek(f: *mut FILE, off: off_t, whence: c_int) -> off_t {
    match (*f).kind {
        Kind::Fd => {
            let r = fl::sys::sc3(fl::sys::LSEEK, (*f).fileno as usize, off as usize, whence as usize);
            if r < 0 {
                fl::errno::set(-r as i32);
                -1
            } else {
                r as off_t
            }
        }
        Kind::Cookie => match (*f).fns.seek {
            Some(sf) => {
                let mut o = off;
                if sf((*f).cookie, &mut o, whence) < 0 {
                    -1
                } else {
                    o
                }
            }
            None => {
                fl::errno::set(29 /* ESPIPE */);
                -1
            }
        },
        Kind::MemStream | Kind::FixedMem => {
            let len = if (*f).kind == Kind::MemStream { (*f).mem.len() } else { (*f).mem_len } as i64;
            let base = match whence {
                0 => 0,
                1 => (*f).mem_pos as i64,
                2 => len,
                _ => {
                    fl::errno::set(fl::errno::EINVAL);
                    return -1;
                }
            };
            let p = base + off;
            if p < 0 || ((*f).kind == Kind::FixedMem && p as usize > (*f).mem_cap) {
                fl::errno::set(fl::errno::EINVAL);
                return -1;
            }
            (*f).mem_pos = p as usize;
            if (*f).kind == Kind::MemStream && (*f).mem.len() < (*f).mem_pos {
                (*f).mem.resize((*f).mem_pos, 0);
                memstream_publish(f);
            }
            p
        }
    }
}

/// open_memstream: the caller's pointer always holds the contents, NUL-terminated.
unsafe fn memstream_publish(f: *mut FILE) {
    let m = &(*f).mem;
    let len = m.len();
    let old = *(*f).ms_ptr;
    let p = umem::realloc(old as *mut c_void, len + 1) as *mut u8;
    if p.is_null() {
        return;
    }
    ptr::copy_nonoverlapping(m.as_ptr(), p, len);
    *p.add(len) = 0;
    *(*f).ms_ptr = p;
    *(*f).ms_size = (*f).mem_pos.min(len);
}

// --- buffering -----------------------------------------------------------------------------

unsafe fn ensure_buf(f: *mut FILE) -> bool {
    if !(*f).buf_base.is_null() {
        return true;
    }
    let p = fl::malloc::imalloc(BUFSIZ);
    if p.is_null() {
        // fall back to the one-byte buffer
        (*f).buf_base = (*f).shortbuf.as_mut_ptr();
        (*f).buf_end = (*f).buf_base.add(1);
        (*f).flags |= F_UNBUFFERED;
    } else {
        (*f).buf_base = p;
        (*f).buf_end = p.add(BUFSIZ);
        (*f).owns_buf = true;
    }
    let b = (*f).buf_base;
    (*f).read_base = b;
    (*f).read_ptr = b;
    (*f).read_end = b;
    (*f).write_base = b;
    (*f).write_ptr = b;
    (*f).write_end = b;
    true
}

/// Write out pending output. 0 or EOF.
unsafe fn flush_out(f: *mut FILE) -> c_int {
    if (*f).flags & F_CURRENTLY_PUTTING == 0 {
        return 0;
    }
    let n = (*f).write_ptr as usize - (*f).write_base as usize;
    if n > 0 {
        let r = raw_write(f, (*f).write_base, n);
        if r < n as isize {
            (*f).flags |= F_ERR;
            // keep what was not written
            let w = r.max(0) as usize;
            ptr::copy((*f).write_base.add(w), (*f).write_base, n - w);
            (*f).write_ptr = (*f).write_base.add(n - w);
            return EOF;
        }
    }
    (*f).write_ptr = (*f).write_base;
    if (*f).flags & (F_LINE_BUF | F_UNBUFFERED) != 0 {
        (*f).write_end = (*f).write_ptr;
    }
    0
}

/// Leave reading mode: give back unread data by seeking (when possible).
unsafe fn drop_read(f: *mut FILE) {
    if (*f).flags & F_IN_BACKUP != 0 {
        leave_backup(f);
    }
    let unread = (*f).read_end as usize - (*f).read_ptr as usize;
    if unread > 0 {
        if raw_seek(f, -(unread as i64), 1) < 0 {
            fl::errno::set(0);
        }
    }
    let b = (*f).buf_base;
    (*f).read_base = b;
    (*f).read_ptr = b;
    (*f).read_end = b;
}

unsafe fn to_write_mode(f: *mut FILE) -> bool {
    if (*f).flags & F_NO_WRITES != 0 {
        (*f).flags |= F_ERR;
        fl::errno::set(fl::errno::EBADF);
        return false;
    }
    if !ensure_buf(f) {
        return false;
    }
    if (*f).flags & F_CURRENTLY_PUTTING == 0 {
        drop_read(f);
        if (*f).flags & F_APPENDING != 0 && (*f).kind == Kind::Fd {
            raw_seek(f, 0, 2);
        }
        (*f).flags |= F_CURRENTLY_PUTTING;
        let b = (*f).buf_base;
        (*f).write_base = b;
        (*f).write_ptr = b;
        (*f).write_end = if (*f).flags & (F_LINE_BUF | F_UNBUFFERED) != 0 { b } else { (*f).buf_end };
        (*f).read_ptr = b;
        (*f).read_end = b;
        (*f).read_base = b;
    }
    true
}

unsafe fn to_read_mode(f: *mut FILE) -> bool {
    if (*f).flags & F_NO_READS != 0 {
        (*f).flags |= F_ERR;
        fl::errno::set(fl::errno::EBADF);
        return false;
    }
    if !ensure_buf(f) {
        return false;
    }
    if (*f).flags & F_CURRENTLY_PUTTING != 0 {
        if flush_out(f) != 0 {
            return false;
        }
        (*f).flags &= !F_CURRENTLY_PUTTING;
        let b = (*f).buf_base;
        (*f).write_base = b;
        (*f).write_ptr = b;
        (*f).write_end = b;
    }
    true
}

unsafe fn leave_backup(f: *mut FILE) {
    let (b, p, e) = (*f).saved;
    (*f).read_base = b;
    (*f).read_ptr = p;
    (*f).read_end = e;
    (*f).flags &= !F_IN_BACKUP;
}

/// Refill the read buffer. Returns false at end of file or error.
unsafe fn refill(f: *mut FILE) -> bool {
    if (*f).flags & F_IN_BACKUP != 0 {
        leave_backup(f);
        if (*f).read_ptr < (*f).read_end {
            return true;
        }
    }
    if !to_read_mode(f) {
        return false;
    }
    // reading from a terminal-ish stream: show pending line-buffered output first
    if f == stdin_file() {
        let o = stdout_file();
        if (*o).flags & F_LINE_BUF != 0 && (*o).flags & F_CURRENTLY_PUTTING != 0 {
            flush_out(o);
        }
    }
    let b = (*f).buf_base;
    let cap = (*f).buf_end as usize - b as usize;
    let want = if (*f).flags & F_UNBUFFERED != 0 { 1 } else { cap };
    let r = raw_read(f, b, want);
    (*f).read_base = b;
    (*f).read_ptr = b;
    if r <= 0 {
        (*f).read_end = b;
        (*f).flags |= if r == 0 { F_EOF } else { F_ERR };
        return false;
    }
    (*f).read_end = b.add(r as usize);
    true
}

#[no_mangle]
pub unsafe extern "C" fn __uflow(f: *mut FILE) -> c_int {
    if (*f).read_ptr < (*f).read_end && (*f).flags & F_CURRENTLY_PUTTING == 0 {
        let c = *(*f).read_ptr;
        (*f).read_ptr = (*f).read_ptr.add(1);
        return c as c_int;
    }
    if !refill(f) {
        return EOF;
    }
    let c = *(*f).read_ptr;
    (*f).read_ptr = (*f).read_ptr.add(1);
    c as c_int
}

#[no_mangle]
pub unsafe extern "C" fn __underflow(f: *mut FILE) -> c_int {
    if (*f).read_ptr < (*f).read_end && (*f).flags & F_CURRENTLY_PUTTING == 0 {
        return *(*f).read_ptr as c_int;
    }
    if !refill(f) {
        return EOF;
    }
    *(*f).read_ptr as c_int
}

#[no_mangle]
pub unsafe extern "C" fn __overflow(f: *mut FILE, c: c_int) -> c_int {
    if !to_write_mode(f) {
        return EOF;
    }
    if c == EOF {
        return flush_out(f);
    }
    if (*f).write_ptr >= (*f).buf_end && flush_out(f) != 0 {
        return EOF;
    }
    *(*f).write_ptr = c as u8;
    (*f).write_ptr = (*f).write_ptr.add(1);
    if (*f).flags & F_UNBUFFERED != 0 || ((*f).flags & F_LINE_BUF != 0 && c as u8 == b'\n') {
        if flush_out(f) != 0 {
            return EOF;
        }
    } else if (*f).flags & F_LINE_BUF != 0 {
        (*f).write_end = (*f).write_ptr;
    }
    c as u8 as c_int
}

// --- reading and writing --------------------------------------------------------------------

pub unsafe fn put_bytes(f: *mut FILE, mut s: &[u8]) -> usize {
    if s.is_empty() {
        return 0;
    }
    if !to_write_mode(f) {
        return 0;
    }
    let total = s.len();
    let line = (*f).flags & F_LINE_BUF != 0;
    if (*f).flags & F_UNBUFFERED != 0 {
        if flush_out(f) != 0 {
            return 0;
        }
        let r = raw_write(f, s.as_ptr(), s.len());
        if r < s.len() as isize {
            (*f).flags |= F_ERR;
            return r.max(0) as usize;
        }
        return total;
    }
    while !s.is_empty() {
        let room = (*f).buf_end as usize - (*f).write_ptr as usize;
        if room == 0 {
            if flush_out(f) != 0 {
                return total - s.len();
            }
            continue;
        }
        // big writes skip the buffer
        if (*f).write_ptr == (*f).write_base && s.len() >= ((*f).buf_end as usize - (*f).buf_base as usize) && !line {
            let r = raw_write(f, s.as_ptr(), s.len());
            if r < s.len() as isize {
                (*f).flags |= F_ERR;
                return total - s.len() + r.max(0) as usize;
            }
            return total;
        }
        let k = room.min(s.len());
        ptr::copy_nonoverlapping(s.as_ptr(), (*f).write_ptr, k);
        (*f).write_ptr = (*f).write_ptr.add(k);
        s = &s[k..];
    }
    if line {
        let pending = core::slice::from_raw_parts((*f).write_base, (*f).write_ptr as usize - (*f).write_base as usize);
        if pending.contains(&b'\n') && flush_out(f) != 0 {
            return total;
        }
        (*f).write_end = (*f).write_ptr;
    }
    total
}

pub unsafe fn get_bytes(f: *mut FILE, buf: *mut u8, n: usize) -> usize {
    let mut done = 0;
    while done < n {
        let avail = (*f).read_end as usize - (*f).read_ptr as usize;
        if avail > 0 && (*f).flags & F_CURRENTLY_PUTTING == 0 {
            let k = avail.min(n - done);
            ptr::copy_nonoverlapping((*f).read_ptr, buf.add(done), k);
            (*f).read_ptr = (*f).read_ptr.add(k);
            done += k;
            continue;
        }
        if (*f).flags & F_IN_BACKUP != 0 {
            leave_backup(f);
            continue;
        }
        if !to_read_mode(f) {
            break;
        }
        // big reads go straight into the caller's buffer
        let cap = (*f).buf_end as usize - (*f).buf_base as usize;
        if n - done >= cap && (*f).flags & F_UNBUFFERED == 0 {
            let r = raw_read(f, buf.add(done), n - done);
            if r <= 0 {
                (*f).flags |= if r == 0 { F_EOF } else { F_ERR };
                break;
            }
            done += r as usize;
            continue;
        }
        if !refill(f) {
            break;
        }
    }
    done
}

#[no_mangle]
pub unsafe extern "C" fn fwrite(p: *const c_void, size: size_t, n: size_t, f: *mut FILE) -> size_t {
    let total = size.saturating_mul(n);
    if total == 0 {
        return 0;
    }
    let _l = locked(f);
    let w = put_bytes(f, core::slice::from_raw_parts(p as *const u8, total));
    w / size
}

#[no_mangle]
pub unsafe extern "C" fn fwrite_unlocked(p: *const c_void, size: size_t, n: size_t, f: *mut FILE) -> size_t {
    let total = size.saturating_mul(n);
    if total == 0 {
        return 0;
    }
    put_bytes(f, core::slice::from_raw_parts(p as *const u8, total)) / size
}

#[no_mangle]
pub unsafe extern "C" fn fread(p: *mut c_void, size: size_t, n: size_t, f: *mut FILE) -> size_t {
    let total = size.saturating_mul(n);
    if total == 0 {
        return 0;
    }
    let _l = locked(f);
    get_bytes(f, p as *mut u8, total) / size
}

#[no_mangle]
pub unsafe extern "C" fn fread_unlocked(p: *mut c_void, size: size_t, n: size_t, f: *mut FILE) -> size_t {
    let total = size.saturating_mul(n);
    if total == 0 {
        return 0;
    }
    get_bytes(f, p as *mut u8, total) / size
}

#[inline]
pub unsafe fn getc_unl(f: *mut FILE) -> c_int {
    if (*f).read_ptr < (*f).read_end {
        let c = *(*f).read_ptr;
        (*f).read_ptr = (*f).read_ptr.add(1);
        c as c_int
    } else {
        __uflow(f)
    }
}

#[inline]
pub unsafe fn putc_unl(c: c_int, f: *mut FILE) -> c_int {
    if (*f).write_ptr < (*f).write_end {
        *(*f).write_ptr = c as u8;
        (*f).write_ptr = (*f).write_ptr.add(1);
        c as u8 as c_int
    } else {
        __overflow(f, c as u8 as c_int)
    }
}

#[no_mangle]
pub unsafe extern "C" fn fgetc(f: *mut FILE) -> c_int {
    let _l = locked(f);
    getc_unl(f)
}
#[no_mangle]
pub unsafe extern "C" fn getc(f: *mut FILE) -> c_int {
    fgetc(f)
}
#[no_mangle]
pub unsafe extern "C" fn _IO_getc(f: *mut FILE) -> c_int {
    fgetc(f)
}
#[no_mangle]
pub unsafe extern "C" fn fgetc_unlocked(f: *mut FILE) -> c_int {
    getc_unl(f)
}
#[no_mangle]
pub unsafe extern "C" fn getc_unlocked(f: *mut FILE) -> c_int {
    getc_unl(f)
}
#[no_mangle]
pub unsafe extern "C" fn getchar() -> c_int {
    fgetc(stdin_file())
}
#[no_mangle]
pub unsafe extern "C" fn getchar_unlocked() -> c_int {
    getc_unl(stdin_file())
}
#[no_mangle]
pub unsafe extern "C" fn fputc(c: c_int, f: *mut FILE) -> c_int {
    let _l = locked(f);
    putc_unl(c, f)
}
#[no_mangle]
pub unsafe extern "C" fn putc(c: c_int, f: *mut FILE) -> c_int {
    fputc(c, f)
}
#[no_mangle]
pub unsafe extern "C" fn _IO_putc(c: c_int, f: *mut FILE) -> c_int {
    fputc(c, f)
}
#[no_mangle]
pub unsafe extern "C" fn fputc_unlocked(c: c_int, f: *mut FILE) -> c_int {
    putc_unl(c, f)
}
#[no_mangle]
pub unsafe extern "C" fn putc_unlocked(c: c_int, f: *mut FILE) -> c_int {
    putc_unl(c, f)
}
#[no_mangle]
pub unsafe extern "C" fn putchar(c: c_int) -> c_int {
    fputc(c, stdout_file())
}
#[no_mangle]
pub unsafe extern "C" fn putchar_unlocked(c: c_int) -> c_int {
    putc_unl(c, stdout_file())
}

#[no_mangle]
pub unsafe extern "C" fn fputs(s: *const c_char, f: *mut FILE) -> c_int {
    let b = fl::mem::cstr(s);
    let _l = locked(f);
    if put_bytes(f, b) < b.len() {
        EOF
    } else {
        1
    }
}
#[no_mangle]
pub unsafe extern "C" fn fputs_unlocked(s: *const c_char, f: *mut FILE) -> c_int {
    let b = fl::mem::cstr(s);
    if put_bytes(f, b) < b.len() {
        EOF
    } else {
        1
    }
}
#[no_mangle]
pub unsafe extern "C" fn _IO_puts(s: *const c_char) -> c_int {
    puts(s)
}
#[no_mangle]
pub unsafe extern "C" fn puts(s: *const c_char) -> c_int {
    let f = stdout_file();
    let _l = locked(f);
    let b = fl::mem::cstr(s);
    if put_bytes(f, b) < b.len() || putc_unl(b'\n' as c_int, f) == EOF {
        EOF
    } else {
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn fgets(s: *mut c_char, n: c_int, f: *mut FILE) -> *mut c_char {
    let _l = locked(f);
    fgets_unlocked(s, n, f)
}

#[no_mangle]
pub unsafe extern "C" fn fgets_unlocked(s: *mut c_char, n: c_int, f: *mut FILE) -> *mut c_char {
    if n <= 0 {
        return ptr::null_mut();
    }
    let mut i = 0usize;
    while i + 1 < n as usize {
        let c = getc_unl(f);
        if c == EOF {
            break;
        }
        *s.add(i) = c as u8;
        i += 1;
        if c as u8 == b'\n' {
            break;
        }
    }
    if i == 0 && n > 1 {
        return ptr::null_mut();
    }
    *s.add(i) = 0;
    s
}

#[no_mangle]
pub unsafe extern "C" fn getdelim(line: *mut *mut c_char, cap: *mut size_t, delim: c_int, f: *mut FILE) -> isize {
    if line.is_null() || cap.is_null() {
        fl::errno::set(fl::errno::EINVAL);
        return -1;
    }
    let _l = locked(f);
    let mut n = 0usize;
    loop {
        let c = getc_unl(f);
        if c == EOF {
            if n == 0 {
                return -1;
            }
            break;
        }
        if (*line).is_null() || n + 2 > *cap {
            let nc = (*cap).max(120).max((n + 2) * 2);
            let p = umem::realloc(*line as *mut c_void, nc) as *mut c_char;
            if p.is_null() {
                return -1;
            }
            *line = p;
            *cap = nc;
        }
        *(*line).add(n) = c as u8;
        n += 1;
        if c == delim {
            break;
        }
    }
    *(*line).add(n) = 0;
    n as isize
}

#[no_mangle]
pub unsafe extern "C" fn __getdelim(line: *mut *mut c_char, cap: *mut size_t, delim: c_int, f: *mut FILE) -> isize {
    getdelim(line, cap, delim, f)
}

#[no_mangle]
pub unsafe extern "C" fn getline(line: *mut *mut c_char, cap: *mut size_t, f: *mut FILE) -> isize {
    getdelim(line, cap, b'\n' as c_int, f)
}

#[no_mangle]
pub unsafe extern "C" fn ungetc(c: c_int, f: *mut FILE) -> c_int {
    if c == EOF {
        return EOF;
    }
    let _l = locked(f);
    if (*f).flags & F_CURRENTLY_PUTTING != 0 && !to_read_mode(f) {
        return EOF;
    }
    if (*f).buf_base.is_null() {
        ensure_buf(f);
    }
    if (*f).read_ptr > (*f).read_base && (*f).flags & F_IN_BACKUP == 0 {
        (*f).read_ptr = (*f).read_ptr.sub(1);
        *(*f).read_ptr = c as u8;
    } else {
        if (*f).flags & F_IN_BACKUP == 0 {
            (*f).saved = ((*f).read_base, (*f).read_ptr, (*f).read_end);
            let b = (*f).backup.as_mut_ptr();
            (*f).read_base = b;
            (*f).read_end = b.add(8);
            (*f).read_ptr = b.add(8);
            (*f).flags |= F_IN_BACKUP;
        }
        if (*f).read_ptr == (*f).read_base {
            return EOF;
        }
        (*f).read_ptr = (*f).read_ptr.sub(1);
        *(*f).read_ptr = c as u8;
    }
    (*f).flags &= !F_EOF;
    c as u8 as c_int
}

// --- state -------------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn feof(f: *mut FILE) -> c_int {
    ((*f).flags & F_EOF != 0) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn feof_unlocked(f: *mut FILE) -> c_int {
    feof(f)
}
#[no_mangle]
pub unsafe extern "C" fn _IO_feof(f: *mut FILE) -> c_int {
    feof(f)
}
#[no_mangle]
pub unsafe extern "C" fn ferror(f: *mut FILE) -> c_int {
    ((*f).flags & F_ERR != 0) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn ferror_unlocked(f: *mut FILE) -> c_int {
    ferror(f)
}
#[no_mangle]
pub unsafe extern "C" fn _IO_ferror(f: *mut FILE) -> c_int {
    ferror(f)
}
#[no_mangle]
pub unsafe extern "C" fn clearerr(f: *mut FILE) {
    (*f).flags &= !(F_EOF | F_ERR);
}
#[no_mangle]
pub unsafe extern "C" fn clearerr_unlocked(f: *mut FILE) {
    clearerr(f)
}
#[no_mangle]
pub unsafe extern "C" fn fileno(f: *mut FILE) -> c_int {
    if (*f).kind != Kind::Fd {
        fl::errno::set(fl::errno::EBADF);
        return -1;
    }
    (*f).fileno
}
#[no_mangle]
pub unsafe extern "C" fn fileno_unlocked(f: *mut FILE) -> c_int {
    fileno(f)
}

// --- flushing, seeking ---------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn fflush(f: *mut FILE) -> c_int {
    if f.is_null() {
        let list = {
            let _g = LIST_LOCK.guard();
            (*ptr::addr_of!(LIST)).clone()
        };
        let mut r = 0;
        for f in list {
            let _l = locked(f);
            if (*f).flags & F_CURRENTLY_PUTTING != 0 && flush_out(f) != 0 {
                r = EOF;
            }
        }
        return r;
    }
    let _l = locked(f);
    fflush_unlocked(f)
}

#[no_mangle]
pub unsafe extern "C" fn fflush_unlocked(f: *mut FILE) -> c_int {
    if f.is_null() {
        return fflush(f);
    }
    if (*f).flags & F_CURRENTLY_PUTTING != 0 {
        return flush_out(f);
    }
    // a reading stream: drop the buffer, putting the file position back
    if (*f).kind == Kind::Fd && (*f).read_ptr < (*f).read_end {
        drop_read(f);
    }
    0
}

pub unsafe fn flush_all() {
    fflush(ptr::null_mut());
}

#[no_mangle]
pub unsafe extern "C" fn _IO_flush_all() -> c_int {
    fflush(ptr::null_mut())
}

pub unsafe fn seek(f: *mut FILE, off: off_t, whence: c_int) -> c_int {
    if (*f).flags & F_CURRENTLY_PUTTING != 0 && flush_out(f) != 0 {
        return -1;
    }
    let mut off = off;
    if whence == 1 {
        // the stream's position is behind the file's by what is still unread
        if (*f).flags & F_IN_BACKUP != 0 {
            off -= ((*f).read_end as usize - (*f).read_ptr as usize) as i64;
            leave_backup(f);
        }
        off -= ((*f).read_end as usize - (*f).read_ptr as usize) as i64;
    }
    if (*f).flags & F_IN_BACKUP != 0 {
        leave_backup(f);
    }
    if raw_seek(f, off, whence) < 0 {
        return -1;
    }
    let b = (*f).buf_base;
    (*f).read_base = b;
    (*f).read_ptr = b;
    (*f).read_end = b;
    if (*f).flags & F_CURRENTLY_PUTTING != 0 {
        (*f).flags &= !F_CURRENTLY_PUTTING;
        (*f).write_base = b;
        (*f).write_ptr = b;
        (*f).write_end = b;
    }
    (*f).flags &= !F_EOF;
    0
}

#[no_mangle]
pub unsafe extern "C" fn fseeko(f: *mut FILE, off: off_t, whence: c_int) -> c_int {
    let _l = locked(f);
    seek(f, off, whence)
}
#[no_mangle]
pub unsafe extern "C" fn fseeko64(f: *mut FILE, off: off_t, whence: c_int) -> c_int {
    fseeko(f, off, whence)
}
#[no_mangle]
pub unsafe extern "C" fn fseek(f: *mut FILE, off: i64, whence: c_int) -> c_int {
    fseeko(f, off, whence)
}

#[no_mangle]
pub unsafe extern "C" fn ftello(f: *mut FILE) -> off_t {
    let _l = locked(f);
    let pos = match (*f).kind {
        Kind::MemStream | Kind::FixedMem => (*f).mem_pos as i64,
        _ => raw_seek(f, 0, 1),
    };
    if pos < 0 {
        return -1;
    }
    let mut p = pos;
    if (*f).flags & F_CURRENTLY_PUTTING != 0 {
        p += ((*f).write_ptr as usize - (*f).write_base as usize) as i64;
    } else {
        p -= ((*f).read_end as usize - (*f).read_ptr as usize) as i64;
        if (*f).flags & F_IN_BACKUP != 0 {
            let (_, sp, se) = (*f).saved;
            p -= (se as usize - sp as usize) as i64;
        }
    }
    p
}
#[no_mangle]
pub unsafe extern "C" fn ftello64(f: *mut FILE) -> off_t {
    ftello(f)
}
#[no_mangle]
pub unsafe extern "C" fn ftell(f: *mut FILE) -> i64 {
    ftello(f)
}
#[no_mangle]
pub unsafe extern "C" fn rewind(f: *mut FILE) {
    let _l = locked(f);
    seek(f, 0, 0);
    (*f).flags &= !F_ERR;
}

#[repr(C)]
pub struct FposT {
    pos: i64,
    state: [u64; 1],
}

#[no_mangle]
pub unsafe extern "C" fn fgetpos(f: *mut FILE, p: *mut FposT) -> c_int {
    let o = ftello(f);
    if o < 0 {
        return -1;
    }
    (*p).pos = o;
    0
}
#[no_mangle]
pub unsafe extern "C" fn fgetpos64(f: *mut FILE, p: *mut FposT) -> c_int {
    fgetpos(f, p)
}
#[no_mangle]
pub unsafe extern "C" fn fsetpos(f: *mut FILE, p: *const FposT) -> c_int {
    fseeko(f, (*p).pos, 0)
}
#[no_mangle]
pub unsafe extern "C" fn fsetpos64(f: *mut FILE, p: *const FposT) -> c_int {
    fsetpos(f, p)
}

#[no_mangle]
pub unsafe extern "C" fn setvbuf(f: *mut FILE, buf: *mut c_char, mode: c_int, size: size_t) -> c_int {
    let _l = locked(f);
    (*f).flags &= !(F_LINE_BUF | F_UNBUFFERED);
    match mode {
        0 => {}                             // _IOFBF
        1 => (*f).flags |= F_LINE_BUF,      // _IOLBF
        2 => (*f).flags |= F_UNBUFFERED,    // _IONBF
        _ => return -1,
    }
    if !buf.is_null() && size > 0 && mode != 2 {
        if (*f).owns_buf {
            fl::malloc::ifree((*f).buf_base);
        }
        (*f).owns_buf = false;
        (*f).buf_base = buf;
        (*f).buf_end = buf.add(size);
        let b = buf;
        (*f).read_base = b;
        (*f).read_ptr = b;
        (*f).read_end = b;
        (*f).write_base = b;
        (*f).write_ptr = b;
        (*f).write_end = b;
        (*f).flags &= !F_CURRENTLY_PUTTING;
    } else if (*f).flags & F_CURRENTLY_PUTTING != 0 {
        (*f).write_end = if mode == 0 { (*f).buf_end } else { (*f).write_ptr };
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn setbuf(f: *mut FILE, buf: *mut c_char) {
    setvbuf(f, buf, if buf.is_null() { 2 } else { 0 }, BUFSIZ);
}
#[no_mangle]
pub unsafe extern "C" fn setbuffer(f: *mut FILE, buf: *mut c_char, size: size_t) {
    setvbuf(f, buf, if buf.is_null() { 2 } else { 0 }, size);
}
#[no_mangle]
pub unsafe extern "C" fn setlinebuf(f: *mut FILE) {
    setvbuf(f, ptr::null_mut(), 1, 0);
}

// stdio_ext.h
#[no_mangle]
pub unsafe extern "C" fn __fpending(f: *mut FILE) -> size_t {
    if (*f).flags & F_CURRENTLY_PUTTING != 0 {
        (*f).write_ptr as usize - (*f).write_base as usize
    } else {
        0
    }
}
#[no_mangle]
pub unsafe extern "C" fn __fpurge(f: *mut FILE) {
    if (*f).flags & F_IN_BACKUP != 0 {
        leave_backup(f);
    }
    (*f).read_ptr = (*f).read_end;
    (*f).write_ptr = (*f).write_base;
}
#[no_mangle]
pub unsafe extern "C" fn fpurge(f: *mut FILE) -> c_int {
    __fpurge(f);
    0
}
#[no_mangle]
pub unsafe extern "C" fn __fsetlocking(_f: *mut FILE, _t: c_int) -> c_int {
    1 // FSETLOCKING_INTERNAL
}
#[no_mangle]
pub unsafe extern "C" fn __freading(f: *mut FILE) -> c_int {
    ((*f).flags & F_NO_WRITES != 0 || ((*f).flags & F_CURRENTLY_PUTTING == 0 && (*f).read_end > (*f).read_base)) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn __fwriting(f: *mut FILE) -> c_int {
    ((*f).flags & F_NO_READS != 0 || (*f).flags & F_CURRENTLY_PUTTING != 0) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn __freadable(f: *mut FILE) -> c_int {
    ((*f).flags & F_NO_READS == 0) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn __fwritable(f: *mut FILE) -> c_int {
    ((*f).flags & F_NO_WRITES == 0) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn __flbf(f: *mut FILE) -> c_int {
    ((*f).flags & F_LINE_BUF != 0) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn __fbufsize(f: *mut FILE) -> size_t {
    (*f).buf_end as usize - (*f).buf_base as usize
}
#[no_mangle]
pub unsafe extern "C" fn __freadahead(f: *mut FILE) -> size_t {
    (*f).read_end as usize - (*f).read_ptr as usize
}
#[no_mangle]
pub unsafe extern "C" fn _flushlbf() {
    fflush(ptr::null_mut());
}

// --- opening and closing ------------------------------------------------------------------

/// Parse an fopen mode: (open flags, stream flags).
fn parse_mode(m: &[u8]) -> Option<(i32, i32)> {
    const O_WRONLY: i32 = 1;
    const O_RDWR: i32 = 2;
    const O_CREAT: i32 = 0o100;
    const O_EXCL: i32 = 0o200;
    const O_TRUNC: i32 = 0o1000;
    const O_APPEND: i32 = 0o2000;
    const O_CLOEXEC: i32 = 0o2000000;
    let (mut o, mut sf) = match m.first()? {
        b'r' => (0, F_NO_WRITES),
        b'w' => (O_WRONLY | O_CREAT | O_TRUNC, F_NO_READS),
        b'a' => (O_WRONLY | O_CREAT | O_APPEND, F_NO_READS | F_APPENDING),
        _ => return None,
    };
    for &c in &m[1..] {
        match c {
            b'+' => {
                o = (o & !O_WRONLY) | O_RDWR;
                sf &= !(F_NO_READS | F_NO_WRITES);
            }
            b'x' => o |= O_EXCL,
            b'e' => o |= O_CLOEXEC,
            b',' => break, // ",ccs=" and the like
            _ => {}
        }
    }
    Some((o, sf))
}

unsafe fn new_file(fd: i32, sf: i32) -> *mut FILE {
    let f = fl::malloc::imalloc(core::mem::size_of::<FILE>()) as *mut FILE;
    if f.is_null() {
        return f;
    }
    ptr::write(f, blank(fd, sf));
    (*f).owns_file = true;
    let _g = LIST_LOCK.guard();
    (*ptr::addr_of_mut!(LIST)).push(f);
    f
}

#[no_mangle]
pub unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut FILE {
    let Some((o, sf)) = parse_mode(fl::mem::cstr(mode)) else {
        fl::errno::set(fl::errno::EINVAL);
        return ptr::null_mut();
    };
    let fd = fl::file::open(path as *const _, o | 0o100000 /* O_LARGEFILE */, 0o666);
    if fd < 0 {
        return ptr::null_mut();
    }
    let f = new_file(fd, sf);
    if f.is_null() {
        fl::sys::sc1(fl::sys::CLOSE, fd as usize);
    }
    f
}
#[no_mangle]
pub unsafe extern "C" fn fopen64(path: *const c_char, mode: *const c_char) -> *mut FILE {
    fopen(path, mode)
}

#[no_mangle]
pub unsafe extern "C" fn fdopen(fd: c_int, mode: *const c_char) -> *mut FILE {
    let Some((o, sf)) = parse_mode(fl::mem::cstr(mode)) else {
        fl::errno::set(fl::errno::EINVAL);
        return ptr::null_mut();
    };
    if o & 0o2000 != 0 {
        let fl_ = fl::sys::sc2(fl::sys::FCNTL, fd as usize, 3 /* F_GETFL */);
        if fl_ >= 0 && fl_ & 0o2000 == 0 {
            fl::sys::sc3(fl::sys::FCNTL, fd as usize, 4 /* F_SETFL */, (fl_ | 0o2000) as usize);
        }
    }
    new_file(fd, sf)
}

#[no_mangle]
pub unsafe extern "C" fn freopen(path: *const c_char, mode: *const c_char, f: *mut FILE) -> *mut FILE {
    let Some((o, sf)) = parse_mode(fl::mem::cstr(mode)) else {
        fl::errno::set(fl::errno::EINVAL);
        return ptr::null_mut();
    };
    let _l = locked(f);
    if (*f).flags & F_CURRENTLY_PUTTING != 0 {
        flush_out(f);
    }
    let old_fd = (*f).fileno;
    if path.is_null() {
        // same file, new mode
        fl::sys::sc3(fl::sys::FCNTL, old_fd as usize, 4, (o & 0o2000) as usize);
    } else {
        let fd = fl::file::open(path as *const _, o | 0o100000, 0o666);
        if fd < 0 {
            return ptr::null_mut();
        }
        if fd != old_fd {
            fl::sys::sc3(fl::sys::DUP3, fd as usize, old_fd as usize, (o & 0o2000000) as usize);
            fl::sys::sc1(fl::sys::CLOSE, fd as usize);
        }
    }
    let keep = (*f).flags & (F_LINE_BUF | F_UNBUFFERED);
    (*f).flags = MAGIC | sf | keep;
    let b = (*f).buf_base;
    (*f).read_base = b;
    (*f).read_ptr = b;
    (*f).read_end = b;
    (*f).write_base = b;
    (*f).write_ptr = b;
    (*f).write_end = b;
    f
}
#[no_mangle]
pub unsafe extern "C" fn freopen64(path: *const c_char, mode: *const c_char, f: *mut FILE) -> *mut FILE {
    freopen(path, mode, f)
}

#[no_mangle]
pub unsafe extern "C" fn fclose(f: *mut FILE) -> c_int {
    let mut r = 0;
    {
        let _l = locked(f);
        if (*f).flags & F_CURRENTLY_PUTTING != 0 && flush_out(f) != 0 {
            r = EOF;
        }
    }
    {
        let _g = LIST_LOCK.guard();
        let l = &mut *ptr::addr_of_mut!(LIST);
        if let Some(i) = l.iter().position(|&x| x == f) {
            l.remove(i);
        }
    }
    match (*f).kind {
        Kind::Fd => {
            if !(*f).no_close && fl::sys::sc1(fl::sys::CLOSE, (*f).fileno as usize) < 0 {
                r = EOF;
            }
        }
        Kind::Cookie => {
            if let Some(c) = (*f).fns.close {
                if c((*f).cookie) < 0 {
                    r = EOF;
                }
            }
        }
        Kind::MemStream => memstream_publish(f),
        Kind::FixedMem => {
            if (*f).owns_buf && !(*f).mem_user.is_null() && (*f).mem_user != (*f).buf_base {
                // fmemopen(NULL, ...) made its own buffer
            }
        }
    }
    if (*f).owns_buf {
        fl::malloc::ifree((*f).buf_base);
        (*f).owns_buf = false;
    }
    if (*f).owns_file {
        ptr::drop_in_place(f);
        fl::malloc::ifree(f as *mut u8);
    } else {
        (*f).flags |= F_ERR;
    }
    r
}

#[no_mangle]
pub unsafe extern "C" fn fcloseall() -> c_int {
    flush_all();
    0
}

#[no_mangle]
pub unsafe extern "C" fn fopencookie(cookie: *mut c_void, mode: *const c_char, fns: CookieFns) -> *mut FILE {
    let Some((_, sf)) = parse_mode(fl::mem::cstr(mode)) else {
        fl::errno::set(fl::errno::EINVAL);
        return ptr::null_mut();
    };
    let f = new_file(-1, sf);
    if !f.is_null() {
        (*f).kind = Kind::Cookie;
        (*f).cookie = cookie;
        (*f).fns = fns;
    }
    f
}

#[no_mangle]
pub unsafe extern "C" fn open_memstream(p: *mut *mut c_char, size: *mut size_t) -> *mut FILE {
    let f = new_file(-1, F_NO_READS);
    if !f.is_null() {
        (*f).kind = Kind::MemStream;
        (*f).ms_ptr = p;
        (*f).ms_size = size;
        *p = ptr::null_mut();
        memstream_publish(f);
    }
    f
}

#[no_mangle]
pub unsafe extern "C" fn fmemopen(buf: *mut c_void, size: size_t, mode: *const c_char) -> *mut FILE {
    let m = fl::mem::cstr(mode);
    let Some((_, sf)) = parse_mode(m) else {
        fl::errno::set(fl::errno::EINVAL);
        return ptr::null_mut();
    };
    let f = new_file(-1, sf);
    if f.is_null() {
        return f;
    }
    (*f).kind = Kind::FixedMem;
    let b = if buf.is_null() {
        let p = fl::malloc::icalloc(1, size.max(1));
        p
    } else {
        buf as *mut u8
    };
    (*f).mem_user = b;
    (*f).mem_cap = size;
    (*f).mem_len = match m.first() {
        Some(b'w') => {
            if size > 0 {
                *b = 0;
            }
            0
        }
        Some(b'a') => fl::mem::cstrlen(b).min(size),
        _ => size,
    };
    if m.first() == Some(&b'a') {
        (*f).mem_pos = (*f).mem_len;
    }
    f
}

#[no_mangle]
pub unsafe extern "C" fn tmpfile() -> *mut FILE {
    // an unnamed file in /tmp
    let fd = fl::file::open(b"/tmp\0".as_ptr() as *const _, 0o20200000 | 2 /* O_TMPFILE|O_RDWR */, 0o600);
    let fd = if fd >= 0 {
        fd
    } else {
        let mut name = *b"/tmp/tmpfileXXXXXX\0";
        let fd = super::misc::mkstemp(name.as_mut_ptr());
        if fd < 0 {
            return ptr::null_mut();
        }
        fl::file::unlink(name.as_ptr() as *const _);
        fd
    };
    new_file(fd, 0)
}
#[no_mangle]
pub unsafe extern "C" fn tmpfile64() -> *mut FILE {
    tmpfile()
}

#[no_mangle]
pub unsafe extern "C" fn popen(cmd: *const c_char, mode: *const c_char) -> *mut FILE {
    let m = fl::mem::cstr(mode);
    let reading = m.first() == Some(&b'r');
    let mut fds = [0i32; 2];
    if fl::sys::sc2(fl::sys::PIPE2, fds.as_mut_ptr() as usize, 0o2000000) < 0 {
        return ptr::null_mut();
    }
    let (ours, theirs, target) = if reading { (fds[0], fds[1], 1) } else { (fds[1], fds[0], 0) };
    let pid = fl::sys::sc0(57 /* fork */);
    if pid < 0 {
        fl::sys::sc1(fl::sys::CLOSE, fds[0] as usize);
        fl::sys::sc1(fl::sys::CLOSE, fds[1] as usize);
        return ptr::null_mut();
    }
    if pid == 0 {
        fl::sys::sc3(fl::sys::DUP3, theirs as usize, target, 0);
        if theirs as usize == target {
            fl::sys::sc3(fl::sys::FCNTL, target, 2 /* F_SETFD */, 0);
        }
        let argv = [b"sh\0".as_ptr(), b"-c\0".as_ptr(), cmd, ptr::null()];
        fl::sys::sc3(fl::sys::EXECVE, b"/bin/sh\0".as_ptr() as usize, argv.as_ptr() as usize, *fl::env::CELL as usize);
        fl::sys::sc1(fl::sys::EXIT_GROUP, 127);
    }
    fl::sys::sc1(fl::sys::CLOSE, theirs as usize);
    let f = new_file(ours, if reading { F_NO_WRITES } else { F_NO_READS });
    if !f.is_null() {
        (*f).pid = pid as i32;
    }
    f
}

#[no_mangle]
pub unsafe extern "C" fn pclose(f: *mut FILE) -> c_int {
    let pid = (*f).pid;
    fclose(f);
    let mut status = 0i32;
    loop {
        let r = fl::sys::sc4(fl::sys::WAIT4, pid as usize, &mut status as *mut i32 as usize, 0, 0);
        if r == -4 {
            continue;
        }
        if r < 0 {
            fl::errno::set(-r as i32);
            return -1;
        }
        return status;
    }
}

#[no_mangle]
pub unsafe extern "C" fn remove(path: *const c_char) -> c_int {
    let r = fl::sys::sc3(fl::sys::UNLINKAT, (-100isize) as usize, path as usize, 0);
    if r == -21 /* EISDIR */ || r == -1 {
        return fl::sys::ret_i(fl::sys::sc3(fl::sys::UNLINKAT, (-100isize) as usize, path as usize, 0x200 /* AT_REMOVEDIR */));
    }
    fl::sys::ret_i(r)
}

#[no_mangle]
pub unsafe extern "C" fn perror(s: *const c_char) {
    let e = fl::errno::get();
    let f = stderr_file();
    let _l = locked(f);
    if !s.is_null() && *s != 0 {
        put_bytes(f, fl::mem::cstr(s));
        put_bytes(f, b": ");
    }
    put_bytes(f, fl::mem::cstr(fl::strerror::strerror(e) as *const u8));
    put_bytes(f, b"\n");
}

// --- wide-character I/O (UTF-8 on the byte stream) ----------------------------------------

const WEOF: u32 = 0xffff_ffff;

#[no_mangle]
pub unsafe extern "C" fn fgetwc(f: *mut FILE) -> u32 {
    let _l = locked(f);
    let c0 = getc_unl(f);
    if c0 == EOF {
        return WEOF;
    }
    let c0 = c0 as u32;
    let (need, mut v) = if c0 < 0x80 {
        return c0;
    } else if c0 & 0xe0 == 0xc0 {
        (1, c0 & 0x1f)
    } else if c0 & 0xf0 == 0xe0 {
        (2, c0 & 0x0f)
    } else if c0 & 0xf8 == 0xf0 {
        (3, c0 & 0x07)
    } else {
        fl::errno::set(84);
        return WEOF;
    };
    for _ in 0..need {
        let c = getc_unl(f);
        if c == EOF || (c as u32) & 0xc0 != 0x80 {
            fl::errno::set(84);
            (*f).flags |= F_ERR;
            return WEOF;
        }
        v = (v << 6) | (c as u32 & 0x3f);
    }
    v
}
#[no_mangle]
pub unsafe extern "C" fn getwc(f: *mut FILE) -> u32 {
    fgetwc(f)
}
#[no_mangle]
pub unsafe extern "C" fn getwchar() -> u32 {
    fgetwc(stdin_file())
}
#[no_mangle]
pub unsafe extern "C" fn fputwc(c: u32, f: *mut FILE) -> u32 {
    let mut b = [0u8; 4];
    let n = super::locale::utf8_encode(c, &mut b);
    let _l = locked(f);
    if put_bytes(f, &b[..n]) < n {
        WEOF
    } else {
        c
    }
}
#[no_mangle]
pub unsafe extern "C" fn putwc(c: u32, f: *mut FILE) -> u32 {
    fputwc(c, f)
}
#[no_mangle]
pub unsafe extern "C" fn putwchar(c: u32) -> u32 {
    fputwc(c, stdout_file())
}
#[no_mangle]
pub unsafe extern "C" fn ungetwc(c: u32, f: *mut FILE) -> u32 {
    let mut b = [0u8; 4];
    let n = super::locale::utf8_encode(c, &mut b);
    for i in (0..n).rev() {
        if ungetc(b[i] as c_int, f) == EOF {
            return WEOF;
        }
    }
    c
}
#[no_mangle]
pub unsafe extern "C" fn fputws(s: *const i32, f: *mut FILE) -> c_int {
    let mut i = 0;
    while *s.add(i) != 0 {
        if fputwc(*s.add(i) as u32, f) == WEOF {
            return -1;
        }
        i += 1;
    }
    1
}
#[no_mangle]
pub unsafe extern "C" fn fgetws(s: *mut i32, n: c_int, f: *mut FILE) -> *mut i32 {
    let mut i = 0usize;
    while i + 1 < n as usize {
        let c = fgetwc(f);
        if c == WEOF {
            break;
        }
        *s.add(i) = c as i32;
        i += 1;
        if c == b'\n' as u32 {
            break;
        }
    }
    if i == 0 {
        return ptr::null_mut();
    }
    *s.add(i) = 0;
    s
}
#[no_mangle]
pub unsafe extern "C" fn fwide(f: *mut FILE, mode: c_int) -> c_int {
    if (*f).wide == 0 && mode != 0 {
        (*f).wide = mode.signum();
    }
    (*f).wide
}
