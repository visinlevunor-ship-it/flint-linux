//! The program's malloc, realloc and free (whichever the program uses: its own if it
//! defines them, as Firefox does), for memory the C library hands out or takes back.

use crate::fl;
use crate::loader::{self, Dso};
use core::ffi::c_void;
use core::ptr;

type MallocFn = unsafe extern "C" fn(usize) -> *mut c_void;
type ReallocFn = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
type FreeFn = unsafe extern "C" fn(*mut c_void);
type CallocFn = unsafe extern "C" fn(usize, usize) -> *mut c_void;

unsafe extern "C" fn own_malloc(n: usize) -> *mut c_void {
    fl::malloc::imalloc(n) as *mut c_void
}
unsafe extern "C" fn own_realloc(p: *mut c_void, n: usize) -> *mut c_void {
    fl::malloc::irealloc(p as *mut u8, n) as *mut c_void
}
unsafe extern "C" fn own_free(p: *mut c_void) {
    fl::malloc::ifree(p as *mut u8)
}
unsafe extern "C" fn own_calloc(n: usize, s: usize) -> *mut c_void {
    fl::malloc::icalloc(n, s) as *mut c_void
}

static mut MALLOC: MallocFn = own_malloc;
static mut REALLOC: ReallocFn = own_realloc;
static mut FREE: FreeFn = own_free;
static mut CALLOC: CallocFn = own_calloc;

pub unsafe fn bind(scope: &[*mut Dso]) {
    let find = |n: &[u8]| loader::lookup(n, None, scope, ptr::null_mut()).filter(|f| !(*f.dso).is_libc).map(|f| loader::sym_addr(&f));
    if let Some(a) = find(b"malloc") {
        MALLOC = core::mem::transmute::<usize, MallocFn>(a);
    }
    if let Some(a) = find(b"realloc") {
        REALLOC = core::mem::transmute::<usize, ReallocFn>(a);
    }
    if let Some(a) = find(b"free") {
        FREE = core::mem::transmute::<usize, FreeFn>(a);
    }
    if let Some(a) = find(b"calloc") {
        CALLOC = core::mem::transmute::<usize, CallocFn>(a);
    }
}

pub unsafe fn malloc(n: usize) -> *mut c_void {
    MALLOC(n)
}
pub unsafe fn realloc(p: *mut c_void, n: usize) -> *mut c_void {
    REALLOC(p, n)
}
pub unsafe fn free(p: *mut c_void) {
    FREE(p)
}
pub unsafe fn calloc(n: usize, s: usize) -> *mut c_void {
    CALLOC(n, s)
}
