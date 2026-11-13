//! Starting and ending a program: __libc_start_main, exit and the atexit family
//! (__cxa_atexit, __cxa_finalize, __cxa_thread_atexit_impl, at_quick_exit, on_exit), and
//! the variables a program may keep its own copy of.

use crate::fl;
use crate::loader::{self, Dso};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;

// --- variables (each has a cell: the program's copy if it made one, else ours) ----------

#[no_mangle]
pub static mut __libc_single_threaded: u8 = 1;
static mut SINGLE_THREADED: *mut u8 = ptr::addr_of_mut!(__libc_single_threaded);

#[no_mangle]
pub static mut program_invocation_name: *mut c_char = ptr::null_mut();
#[no_mangle]
pub static mut program_invocation_short_name: *mut c_char = ptr::null_mut();
pub static mut PROGNAME_FULL: *mut *mut c_char = ptr::addr_of_mut!(program_invocation_name);
pub static mut PROGNAME: *mut *mut c_char = ptr::addr_of_mut!(program_invocation_short_name);

static mut ARGC: c_int = 0;
static mut ARGV: *mut *mut c_char = ptr::null_mut();

/// The program's copy of a variable, if it has one (a copy relocation put it there).
pub unsafe fn program_copy(scope: &[*mut Dso], names: &[&[u8]]) -> Option<usize> {
    for n in names {
        if let Some(f) = loader::lookup(n, None, scope, ptr::null_mut()) {
            if !(*f.dso).is_libc {
                return Some((*f.dso).base + (*f.sym).st_value as usize);
            }
        }
    }
    None
}

pub unsafe fn bind_cells(scope: &[*mut Dso]) {
    if let Some(a) = program_copy(scope, &[b"__libc_single_threaded"]) {
        SINGLE_THREADED = a as *mut u8;
    }
    if let Some(a) = program_copy(scope, &[b"program_invocation_name", b"__progname_full"]) {
        PROGNAME_FULL = a as *mut *mut c_char;
    }
    if let Some(a) = program_copy(scope, &[b"program_invocation_short_name", b"__progname"]) {
        PROGNAME = a as *mut *mut c_char;
    }
    super::cells::bind(scope);
}

pub unsafe fn init(argc: c_int, argv: *mut *mut c_char, _envp: *mut *mut c_char) {
    ARGC = argc;
    ARGV = argv;
    let name = if argc > 0 { *argv } else { b"\0".as_ptr() as *mut c_char };
    *PROGNAME_FULL = name;
    let s = fl::mem::cstr(name);
    let short = match s.iter().rposition(|&c| c == b'/') {
        Some(i) => name.add(i + 1),
        None => name,
    };
    *PROGNAME = short;
    *SINGLE_THREADED = 1;
}

pub fn args() -> (c_int, *mut *mut c_char) {
    // SAFETY: written once by init, before the program runs.
    unsafe { (ARGC, ARGV) }
}

pub fn progname_full() -> *const c_char {
    // SAFETY: as above.
    unsafe { *PROGNAME_FULL }
}

pub unsafe fn set_multithreaded() {
    *SINGLE_THREADED = 0;
}

// --- __libc_start_main -------------------------------------------------------------------

type MainFn = unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char) -> c_int;
type InitFn = unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char);

#[no_mangle]
pub unsafe extern "C" fn __libc_start_main(
    main: MainFn,
    argc: c_int,
    argv: *mut *mut c_char,
    init: Option<InitFn>,
    fini: Option<unsafe extern "C" fn()>,
    rtld_fini: Option<unsafe extern "C" fn()>,
    _stack_end: *mut c_void,
) -> c_int {
    let env = *fl::env::CELL;
    if let Some(f) = rtld_fini {
        __cxa_atexit(core::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(*mut c_void)>(f), ptr::null_mut(), ptr::null_mut());
    }
    let prog = loader::st().main;
    match init {
        // programs built for glibc before 2.34 bring __libc_csu_init, which runs their
        // constructors; their destructors come through `fini`
        Some(f) => {
            if let Some(fi) = fini {
                __cxa_atexit(core::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(*mut c_void)>(fi), ptr::null_mut(), ptr::null_mut());
            }
            f(argc, argv, env);
        }
        None => {
            let p = &*prog;
            __cxa_atexit(run_main_fini, ptr::null_mut(), ptr::null_mut());
            loader::run_array(p.preinit_array, p.preinit_arraysz, argc, argv, env);
            if p.init != 0 {
                let f: InitFn = core::mem::transmute(p.init);
                f(argc, argv, env);
            }
            loader::run_array(p.init_array, p.init_arraysz, argc, argv, env);
        }
    }
    exit(main(argc, argv, *fl::env::CELL))
}

unsafe extern "C" fn run_main_fini(_: *mut c_void) {
    let p = &*loader::st().main;
    let mut f = p.fini_array + p.fini_arraysz;
    while f > p.fini_array {
        f -= 8;
        let x = *(f as *const usize);
        if x != 0 && x != usize::MAX {
            let func: extern "C" fn() = core::mem::transmute(x);
            func();
        }
    }
    if p.fini != 0 {
        let func: extern "C" fn() = core::mem::transmute(p.fini);
        func();
    }
}

// --- atexit and friends ------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Handler {
    Cxa(unsafe extern "C" fn(*mut c_void), *mut c_void, *mut c_void), // fn, arg, dso
    OnExit(unsafe extern "C" fn(c_int, *mut c_void), *mut c_void),
}

static LOCK: fl::lock::Lock = fl::lock::Lock::new();
static mut HANDLERS: Vec<Option<Handler>> = Vec::new();
static mut QUICK: Vec<unsafe extern "C" fn()> = Vec::new();
static mut EXITING: bool = false;

#[no_mangle]
pub unsafe extern "C" fn __cxa_atexit(f: unsafe extern "C" fn(*mut c_void), arg: *mut c_void, dso: *mut c_void) -> c_int {
    let _g = LOCK.guard();
    (*ptr::addr_of_mut!(HANDLERS)).push(Some(Handler::Cxa(f, arg, dso)));
    0
}

unsafe extern "C" fn call_plain(arg: *mut c_void) {
    let f: unsafe extern "C" fn() = core::mem::transmute(arg);
    f();
}

#[no_mangle]
pub unsafe extern "C" fn atexit(f: unsafe extern "C" fn()) -> c_int {
    __cxa_atexit(call_plain, f as *mut c_void, ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn on_exit(f: unsafe extern "C" fn(c_int, *mut c_void), arg: *mut c_void) -> c_int {
    let _g = LOCK.guard();
    (*ptr::addr_of_mut!(HANDLERS)).push(Some(Handler::OnExit(f, arg)));
    0
}

/// Run handlers (all of them, or those registered by one shared object), newest first.
unsafe fn run_handlers(dso: Option<*mut c_void>, code: c_int) {
    loop {
        let h = {
            let _g = LOCK.guard();
            let hs = &mut *ptr::addr_of_mut!(HANDLERS);
            let mut found = None;
            for i in (0..hs.len()).rev() {
                if let Some(h) = hs[i] {
                    let matches = match (dso, h) {
                        (None, _) => true,
                        (Some(d), Handler::Cxa(_, _, hd)) => hd == d,
                        (Some(_), Handler::OnExit(..)) => false,
                    };
                    if matches {
                        hs[i] = None;
                        found = Some(h);
                        break;
                    }
                }
            }
            found
        };
        match h {
            Some(Handler::Cxa(f, arg, _)) => f(arg),
            Some(Handler::OnExit(f, arg)) => f(code, arg),
            None => break,
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn __cxa_finalize(dso: *mut c_void) {
    run_handlers(if dso.is_null() { None } else { Some(dso) }, 0);
}

#[no_mangle]
pub unsafe extern "C" fn exit(code: c_int) -> ! {
    EXITING = true;
    run_thread_dtors();
    run_handlers(None, code);
    super::stdio_flush_all();
    fl::start::_exit(code)
}

#[no_mangle]
pub unsafe extern "C" fn at_quick_exit(f: unsafe extern "C" fn()) -> c_int {
    let _g = LOCK.guard();
    (*ptr::addr_of_mut!(QUICK)).push(f);
    0
}

#[no_mangle]
pub unsafe extern "C" fn quick_exit(code: c_int) -> ! {
    let q = core::mem::take(&mut *ptr::addr_of_mut!(QUICK));
    for f in q.iter().rev() {
        f();
    }
    fl::start::_exit(code)
}

#[no_mangle]
pub unsafe extern "C" fn _Exit(code: c_int) -> ! {
    fl::start::_exit(code)
}

// --- C++ thread_local destructors --------------------------------------------------------

/// Per thread: (destructor, object), newest last. Kept in the thread block's dtv slot, which
/// nothing else uses (all TLS is static).
unsafe fn thread_dtors() -> *mut Vec<(unsafe extern "C" fn(*mut c_void), *mut c_void)> {
    let t = fl::tls::current();
    if (*t).dtv == 0 {
        let v: alloc::boxed::Box<Vec<(unsafe extern "C" fn(*mut c_void), *mut c_void)>> = alloc::boxed::Box::new(Vec::new());
        (*t).dtv = alloc::boxed::Box::into_raw(v) as usize;
    }
    (*t).dtv as *mut _
}

#[no_mangle]
pub unsafe extern "C" fn __cxa_thread_atexit_impl(f: unsafe extern "C" fn(*mut c_void), obj: *mut c_void, _dso: *mut c_void) -> c_int {
    (*thread_dtors()).push((f, obj));
    0
}

pub unsafe fn run_thread_dtors() {
    let t = fl::tls::current();
    if (*t).dtv == 0 {
        return;
    }
    let v = &mut *((*t).dtv as *mut Vec<(unsafe extern "C" fn(*mut c_void), *mut c_void)>);
    while let Some((f, obj)) = v.pop() {
        f(obj);
    }
}

// --- small things every program needs -----------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn __stack_chk_fail() -> ! {
    fl::sys::sc3(fl::sys::WRITE, 2, b"*** stack smashing detected ***: terminated\n".as_ptr() as usize, 44);
    fl::start::abort()
}

#[no_mangle]
pub unsafe extern "C" fn __chk_fail() -> ! {
    fl::sys::sc3(fl::sys::WRITE, 2, b"*** buffer overflow detected ***: terminated\n".as_ptr() as usize, 45);
    fl::start::abort()
}

#[no_mangle]
pub unsafe extern "C" fn __assert_fail(expr: *const c_char, file: *const c_char, line: u32, func: *const c_char) -> ! {
    let mut msg: Vec<u8> = Vec::new();
    msg.extend_from_slice(fl::mem::cstr(*PROGNAME_FULL));
    msg.extend_from_slice(b": ");
    msg.extend_from_slice(fl::mem::cstr(file));
    msg.push(b':');
    let mut num = [0u8; 12];
    let mut i = num.len();
    let mut l = line;
    loop {
        i -= 1;
        num[i] = b'0' + (l % 10) as u8;
        l /= 10;
        if l == 0 {
            break;
        }
    }
    msg.extend_from_slice(&num[i..]);
    msg.extend_from_slice(b": ");
    if !func.is_null() {
        msg.extend_from_slice(fl::mem::cstr(func));
        msg.extend_from_slice(b": ");
    }
    msg.extend_from_slice(b"Assertion `");
    msg.extend_from_slice(fl::mem::cstr(expr));
    msg.extend_from_slice(b"' failed.\n");
    fl::sys::sc3(fl::sys::WRITE, 2, msg.as_ptr() as usize, msg.len());
    fl::start::abort()
}

#[no_mangle]
pub unsafe extern "C" fn __assert(expr: *const c_char, file: *const c_char, line: c_int) -> ! {
    __assert_fail(expr, file, line as u32, ptr::null())
}

#[no_mangle]
pub unsafe extern "C" fn gnu_get_libc_version() -> *const c_char {
    b"2.39\0".as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn gnu_get_libc_release() -> *const c_char {
    b"stable\0".as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn __libc_current_sigrtmin() -> c_int {
    34
}

#[no_mangle]
pub unsafe extern "C" fn __libc_current_sigrtmax() -> c_int {
    64
}
