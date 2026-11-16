//! dlopen and friends, dl_iterate_phdr, _dl_find_object, and thread-local storage access
//! (__tls_get_addr, TLS descriptors).

use crate::elf::*;
use crate::fl;
use crate::loader::{self, st, Dso, LOCK};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;

// ---------------------------------------------------------------------------------------
// Thread-local storage. Every module lives in static TLS (libraries opened later take room
// reserved for them), so an address is always the thread pointer minus the module's offset.

#[repr(C)]
pub struct TlsIndex {
    module: usize,
    offset: usize,
}

#[no_mangle]
pub unsafe extern "C" fn __tls_get_addr(ti: *const TlsIndex) -> *mut c_void {
    let tp = fl::tls::current() as usize;
    (tp - loader::tls_offset_of((*ti).module) + (*ti).offset) as *mut c_void
}

core::arch::global_asm!(
    // TLS descriptor resolver for static TLS: the descriptor's second word is the offset
    // from the thread pointer.
    ".globl __flint_tlsdesc_static",
    ".hidden __flint_tlsdesc_static",
    ".type __flint_tlsdesc_static,@function",
    "__flint_tlsdesc_static:",
    "mov rax, qword ptr [rax + 8]",
    "ret",
);

extern "C" {
    fn __flint_tlsdesc_static();
}

pub fn tlsdesc_static() -> usize {
    __flint_tlsdesc_static as usize
}

unsafe fn on_create(t: *mut fl::tls::Thread) {
    let _g = LOCK.guard();
    st().threads.push(t as usize);
    crate::libc::set_multithreaded();
}

unsafe fn on_exit(t: *mut fl::tls::Thread) {
    crate::libc::run_thread_dtors();
    let _g = LOCK.guard();
    let th = &mut st().threads;
    if let Some(i) = th.iter().position(|&x| x == t as usize) {
        th.swap_remove(i);
    }
}

pub unsafe fn install_thread_hooks() {
    fl::thread::CREATE_HOOK = Some(on_create);
    fl::thread::EXIT_HOOK = Some(on_exit);
}

// ---------------------------------------------------------------------------------------
// dlopen, dlsym, dlclose, dlerror

const RTLD_LAZY: c_int = 1;
const RTLD_NOW: c_int = 2;
const RTLD_NOLOAD: c_int = 4;
const RTLD_GLOBAL: c_int = 0x100;
const RTLD_NODELETE: c_int = 0x1000;
const RTLD_DEFAULT: usize = 0;
const RTLD_NEXT: usize = usize::MAX;

static mut ERROR: Vec<u8> = Vec::new();
static mut HAVE_ERROR: bool = false;

unsafe fn set_error(msg: &[u8]) {
    let e = &mut *ptr::addr_of_mut!(ERROR);
    e.clear();
    e.extend_from_slice(msg);
    e.push(0);
    HAVE_ERROR = true;
}

#[no_mangle]
pub unsafe extern "C" fn dlerror() -> *mut c_char {
    if !HAVE_ERROR {
        return ptr::null_mut();
    }
    HAVE_ERROR = false;
    (*ptr::addr_of!(ERROR)).as_ptr() as *mut c_char
}

/// The object containing an address.
pub unsafe fn dso_at(addr: usize) -> Option<*mut Dso> {
    for &d in &st().dsos {
        if addr >= (*d).map_start && addr < (*d).map_end {
            return Some(d);
        }
    }
    None
}

#[no_mangle]
pub unsafe extern "C" fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void {
    let _g = LOCK.guard();
    if file.is_null() {
        return st().main as *mut c_void;
    }
    let name = fl::mem::cstr(file);
    let caller = st().main;
    if mode & RTLD_NOLOAD != 0 {
        let s = st();
        for &d in &s.dsos {
            let dd = &*d;
            if dd.soname == name || (dd.path.len() > 1 && &dd.path[..dd.path.len() - 1] == name) {
                (*d).refcount += 1;
                return d as *mut c_void;
            }
        }
        set_error(b"object not loaded (RTLD_NOLOAD)");
        return ptr::null_mut();
    }
    let root = match loader::load(name, caller) {
        Ok(d) => d,
        Err(e) => {
            set_error(e.as_bytes());
            return ptr::null_mut();
        }
    };
    (*root).refcount += 1;
    let order = match loader::load_deps(root) {
        Ok(o) => o,
        Err(e) => {
            set_error(e.as_bytes());
            return ptr::null_mut();
        }
    };
    let mut fresh_tls = Vec::new();
    for &d in &order {
        let had = (*d).tls_id != 0;
        if let Err(e) = loader::assign_tls(d) {
            let msg = alloc::format!("{}: {}", (*d).display(), e);
            set_error(msg.as_bytes());
            return ptr::null_mut();
        }
        if !had && (*d).tls_id != 0 {
            fresh_tls.push(d);
        }
    }
    let s = st();
    if mode & RTLD_GLOBAL != 0 {
        for &d in &order {
            if !s.global.contains(&d) {
                s.global.push(d);
            }
        }
    }
    let scope = s.global.clone();
    for &d in order.iter().rev() {
        if !(*d).relocated {
            if !scope.contains(&d) {
                (*d).local_scope = order.clone();
            }
            if let Err(e) = loader::relocate(d, &scope) {
                set_error(e.as_bytes());
                return ptr::null_mut();
            }
        }
    }
    // the TLS images are relocated now; running threads get the new blocks
    for &d in &fresh_tls {
        loader::copy_tls_to_threads(d);
    }
    let mut out = Vec::new();
    let mut seen = Vec::new();
    loader::init_order(root, &mut out, &mut seen);
    let env = *fl::env::CELL;
    let (argc, argv) = crate::libc::args();
    for d in out {
        loader::run_init(d, argc, argv, env);
    }
    root as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn dlmopen(_lmid: isize, file: *const c_char, mode: c_int) -> *mut c_void {
    dlopen(file, mode)
}

#[no_mangle]
pub unsafe extern "C" fn dlclose(handle: *mut c_void) -> c_int {
    // objects stay loaded: unloading is rarely worth the risk
    let d = handle as *mut Dso;
    if !d.is_null() && (*d).refcount > 0 {
        (*d).refcount -= 1;
    }
    0
}

/// A handle's search scope: the object and its dependencies, breadth-first.
unsafe fn handle_scope(d: *mut Dso) -> Vec<*mut Dso> {
    let mut order = alloc::vec![d];
    let mut i = 0;
    while i < order.len() {
        let x = order[i];
        i += 1;
        for &dep in &(*x).deps {
            if !order.contains(&dep) {
                order.push(dep);
            }
        }
    }
    order
}

unsafe fn dlsym_impl(handle: *mut c_void, name: *const c_char, ver: Option<&[u8]>, caller: usize) -> *mut c_void {
    let _g = LOCK.guard();
    let n = fl::mem::cstr(name);
    let s = st();
    let scope: Vec<*mut Dso> = match handle as usize {
        RTLD_DEFAULT => s.global.clone(),
        RTLD_NEXT => {
            // the objects after the caller; glibc searches the caller's load scope, which for
            // a dlopened library (Firefox's libmozsandbox wrapping sigaction) still ends with
            // libc, so libc always comes last
            let me = dso_at(caller);
            let mut v = match me.and_then(|m| s.global.iter().position(|&x| x == m)) {
                Some(i) => s.global[i + 1..].to_vec(),
                None => match me {
                    Some(m) => {
                        let sc = handle_scope(m);
                        match sc.iter().position(|&x| x == m) {
                            Some(i) => sc[i + 1..].to_vec(),
                            None => sc,
                        }
                    }
                    None => s.global.clone(),
                },
            };
            if let Some(&libc) = s.global.iter().find(|&&d| (*d).is_libc) {
                if !v.contains(&libc) && me != Some(libc) {
                    v.push(libc);
                }
            }
            v
        }
        _ => {
            let d = handle as *mut Dso;
            if d == s.main {
                s.global.clone()
            } else {
                handle_scope(d)
            }
        }
    };
    match loader::lookup(n, ver, &scope, ptr::null_mut()) {
        Some(f) => {
            if (*f.sym).st_info & 0xf == STT_TLS {
                let tp = fl::tls::current() as usize;
                return (tp - (*f.dso).tls_offset + (*f.sym).st_value as usize) as *mut c_void;
            }
            loader::sym_addr(&f) as *mut c_void
        }
        None => {
            let msg = alloc::format!("undefined symbol: {}", core::str::from_utf8_unchecked(n));
            set_error(msg.as_bytes());
            ptr::null_mut()
        }
    }
}

core::arch::global_asm!(
    // dlsym and dlvsym need their caller's address for RTLD_NEXT
    ".globl dlsym",
    ".type dlsym,@function",
    "dlsym:",
    "mov rdx, qword ptr [rsp]",
    "jmp {dlsym}",
    ".globl dlvsym",
    ".type dlvsym,@function",
    "dlvsym:",
    "mov rcx, qword ptr [rsp]",
    "jmp {dlvsym}",
    dlsym = sym dlsym_caller,
    dlvsym = sym dlvsym_caller,
);

unsafe extern "C" fn dlsym_caller(handle: *mut c_void, name: *const c_char, caller: usize) -> *mut c_void {
    dlsym_impl(handle, name, None, caller)
}

unsafe extern "C" fn dlvsym_caller(handle: *mut c_void, name: *const c_char, ver: *const c_char, caller: usize) -> *mut c_void {
    let v = if ver.is_null() { None } else { Some(fl::mem::cstr(ver)) };
    dlsym_impl(handle, name, v, caller)
}

#[repr(C)]
pub struct DlInfo {
    dli_fname: *const c_char,
    dli_fbase: *mut c_void,
    dli_sname: *const c_char,
    dli_saddr: *mut c_void,
}

#[no_mangle]
pub unsafe extern "C" fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int {
    dladdr1(addr, info, ptr::null_mut(), 0)
}

#[no_mangle]
pub unsafe extern "C" fn dladdr1(addr: *const c_void, info: *mut DlInfo, extra: *mut *mut c_void, flags: c_int) -> c_int {
    let a = addr as usize;
    let d = match dso_at(a) {
        Some(d) => d,
        None => return 0,
    };
    let dd = &*d;
    (*info).dli_fname = if dd.is_main { crate::libc::progname_full() } else { dd.path.as_ptr() };
    (*info).dli_fbase = dd.map_start as *mut c_void;
    (*info).dli_sname = ptr::null();
    (*info).dli_saddr = ptr::null_mut();
    let mut best = 0usize;
    let mut best_i = 0usize;
    for i in 1..dd.nsyms() {
        let s = dd.sym(i);
        if s.st_shndx == SHN_UNDEF || s.st_value == 0 {
            continue;
        }
        let t = s.st_info & 0xf;
        if t == STT_TLS {
            continue;
        }
        let v = dd.base + s.st_value as usize;
        if v <= a && v > best && (a < v + (s.st_size as usize).max(1) || s.st_size == 0 || true) {
            best = v;
            best_i = i;
        }
    }
    if best != 0 {
        (*info).dli_sname = dd.str_at(dd.sym(best_i).st_name as usize).as_ptr();
        (*info).dli_saddr = best as *mut c_void;
    }
    if !extra.is_null() {
        match flags {
            2 /* RTLD_DL_LINKMAP */ => *extra = d as *mut c_void,
            1 /* RTLD_DL_SYMENT */ => *extra = if best != 0 { dd.sym(best_i) as *const Sym as *mut c_void } else { ptr::null_mut() },
            _ => {}
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn dlinfo(handle: *mut c_void, request: c_int, arg: *mut c_void) -> c_int {
    let d = handle as *mut Dso;
    match request {
        2 /* RTLD_DI_LINKMAP */ => {
            *(arg as *mut *mut Dso) = d;
            0
        }
        6 /* RTLD_DI_ORIGIN */ => {
            let p = &(*d).path;
            let p = &p[..p.len().saturating_sub(1)];
            let dir = match p.iter().rposition(|&c| c == b'/') {
                Some(i) => &p[..i],
                None => b".",
            };
            ptr::copy_nonoverlapping(dir.as_ptr(), arg as *mut u8, dir.len());
            *(arg as *mut u8).add(dir.len()) = 0;
            0
        }
        10 /* RTLD_DI_TLS_MODID */ => {
            *(arg as *mut usize) = (*d).tls_id;
            0
        }
        11 /* RTLD_DI_TLS_DATA */ => {
            *(arg as *mut *mut c_void) = if (*d).tls_id != 0 {
                (fl::tls::current() as usize - (*d).tls_offset) as *mut c_void
            } else {
                ptr::null_mut()
            };
            0
        }
        _ => {
            set_error(b"dlinfo: unsupported request");
            -1
        }
    }
}

// ---------------------------------------------------------------------------------------
// dl_iterate_phdr and _dl_find_object: how unwinders (libgcc_s) find .eh_frame

#[repr(C)]
pub struct DlPhdrInfo {
    addr: usize,
    name: *const c_char,
    phdr: *const c_void,
    phnum: u16,
    adds: u64,
    subs: u64,
    tls_modid: usize,
    tls_data: *mut c_void,
}

#[no_mangle]
pub unsafe extern "C" fn dl_iterate_phdr(
    cb: unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> c_int,
    data: *mut c_void,
) -> c_int {
    let list = {
        let _g = LOCK.guard();
        st().dsos.clone()
    };
    let adds = st().adds;
    let tp = fl::tls::current() as usize;
    for d in list {
        let dd = &*d;
        let mut info = DlPhdrInfo {
            addr: dd.base,
            name: dd.path.as_ptr(),
            phdr: dd.phdr as *const c_void,
            phnum: dd.phnum as u16,
            adds,
            subs: 0,
            tls_modid: dd.tls_id,
            tls_data: if dd.tls_id != 0 { (tp - dd.tls_offset) as *mut c_void } else { ptr::null_mut() },
        };
        let r = cb(&mut info, core::mem::size_of::<DlPhdrInfo>(), data);
        if r != 0 {
            return r;
        }
    }
    0
}

#[repr(C)]
pub struct DlFindObject {
    dlfo_flags: u64,
    dlfo_map_start: *mut c_void,
    dlfo_map_end: *mut c_void,
    dlfo_link_map: *mut c_void,
    dlfo_eh_frame: *mut c_void,
    reserved: [u64; 7],
}

#[no_mangle]
pub unsafe extern "C" fn _dl_find_object(pc: *mut c_void, result: *mut DlFindObject) -> c_int {
    match dso_at(pc as usize) {
        Some(d) if (*d).eh_frame_hdr != 0 => {
            (*result).dlfo_flags = 0;
            (*result).dlfo_map_start = (*d).map_start as *mut c_void;
            (*result).dlfo_map_end = (*d).map_end as *mut c_void;
            (*result).dlfo_link_map = d as *mut c_void;
            (*result).dlfo_eh_frame = (*d).eh_frame_hdr as *mut c_void;
            0
        }
        _ => -1,
    }
}

#[allow(dead_code)]
const _UNUSED: (c_int, c_int, c_int) = (RTLD_LAZY, RTLD_NOW, RTLD_NODELETE);

/// Where the kernel's initial stack (argc, argv...) is: ld.so's variable, which programs
/// (Firefox's JS engine) read to find the main thread's stack.
#[no_mangle]
pub static mut __libc_stack_end: *mut core::ffi::c_void = core::ptr::null_mut();
