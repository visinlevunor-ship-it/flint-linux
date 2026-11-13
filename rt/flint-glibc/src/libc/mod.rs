//! glibc's C library functions that flint-libc does not provide (or provides with musl's
//! behaviour where glibc's differs).

pub mod start;
pub mod cells;
pub mod string;
pub mod locale;
pub mod umem;
pub mod stdio;
pub mod printf;
pub mod scanf;
pub mod misc;
pub mod pthread;
pub mod chk;
pub mod sysfns;
pub mod time;
pub mod math;
pub mod users;
pub mod netdb;
pub mod util;
pub mod jmp;
pub mod pattern;
pub mod regex;
pub mod fts;

use crate::fl;
use crate::loader::{self, Dso};
use core::ptr;

pub use start::{args, progname_full, run_thread_dtors, set_multithreaded};

/// After every object is relocated: point this object's own references at whatever the
/// program chose (a program that defines malloc gets it used here too; a program with its
/// own copy of `environ` gets that one updated).
pub unsafe fn bind_interposed(scope: &[*mut Dso]) {
    let me = loader::st().libc;
    (*me).relocated = false;
    (*me).relr = 0; // applied once at start-up; not idempotent
    let _ = loader::relocate(me, scope);
    let find = |name: &[u8]| -> Option<usize> {
        loader::lookup(name, None, scope, ptr::null_mut()).map(|f| loader::sym_addr(&f))
    };
    // environ: the program's copy if it made one, under any of its three names
    for n in [&b"__environ"[..], b"environ", b"_environ"] {
        if let Some(f) = loader::lookup(n, None, scope, ptr::null_mut()) {
            if !(*f.dso).is_libc {
                fl::env::CELL = ((*f.dso).base + (*f.sym).st_value as usize) as *mut *mut *mut u8;
                break;
            }
        }
    }
    if let Some(m) = find(b"malloc") {
        fl::malloc::USER_MALLOC = core::mem::transmute::<usize, unsafe extern "C" fn(usize) -> *mut u8>(m);
    }
    umem::bind(scope);
    start::bind_cells(scope);
}

pub unsafe fn init(argc: i32, argv: *mut *mut u8, envp: *mut *mut u8) {
    // environ points at the kernel's array unless the program's copy already says otherwise
    if (*fl::env::CELL).is_null() {
        *fl::env::CELL = envp;
    }
    string::ctype_init();
    locale::locale_init();
    stdio::init();
    start::init(argc, argv, envp);
}

/// Flush every stdio stream (at exit).
pub unsafe fn stdio_flush_all() {
    stdio::flush_all();
}
