//! The thread control block and static thread-local storage (x86-64 TLS variant II).
//!
//! Each thread has one block: its TLS area (the program's PT_TLS image, copied, followed by
//! zeros) ends exactly where its `Thread` struct starts, and %fs points at that struct. The
//! first fields follow glibc's `tcbhead_t`, so code built for glibc finds what it expects at
//! %fs:0 (self), %fs:0x28 (stack guard) and %fs:0x30 (pointer guard).

use crate::imp::sys;
use core::arch::asm;
use core::ptr;
use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

#[cfg(not(feature = "glibc"))]
pub const KEYS_MAX: usize = 128;
#[cfg(feature = "glibc")]
const _: () = assert!(core::mem::offset_of!(Thread, tid) == 720);

/// glibc's PTHREAD_KEYS_MAX: GTK and friends use keys freely
#[cfg(feature = "glibc")]
pub const KEYS_MAX: usize = 1024;

#[repr(C)]
pub struct Thread {
    // --- glibc tcbhead_t ---
    pub tcb: *mut Thread, // %fs:0
    pub dtv: usize,
    pub self_: *mut Thread, // %fs:16
    pub multiple_threads: i32,
    pub gscope: i32,
    pub sysinfo: usize,
    pub stack_guard: usize,   // %fs:0x28
    pub pointer_guard: usize, // %fs:0x30
    pub _tcbhead_rest: [usize; 7],
    /// The rest of glibc's tcbhead_t (704 bytes) and struct pthread's list head, so that
    /// `tid` sits where glibc keeps it: programs that clone() by hand (Firefox's sandbox)
    /// find and fix glibc's cached thread id there.
    #[cfg(feature = "glibc")]
    pub _glibc_pthread: [usize; 76],
    /// The kernel clears this and wakes it when the thread exits (CLONE_CHILD_CLEARTID).
    pub tid: AtomicI32,
    // --- flint ---
    pub errno: i32,
    /// glibc's h_errno (netdb lookups)
    pub h_errno: i32,
    /// 0 joinable, 1 detached, 2 finished (and not detached)
    pub state: AtomicI32,
    pub result: *mut core::ffi::c_void,
    /// The whole mapping (guard + stack + TLS + this struct); 0 for the main thread's TLS.
    pub map_base: usize,
    pub map_size: usize,
    pub stack_lo: usize,
    pub stack_size: usize,
    pub guard_size: usize,
    pub start: Option<unsafe extern "C" fn(*mut core::ffi::c_void) -> *mut core::ffi::c_void>,
    pub arg: *mut core::ffi::c_void,
    pub keys: [*mut core::ffi::c_void; KEYS_MAX],
    pub key_gen: [u32; KEYS_MAX],
}

/// One object's TLS template and where its block sits: `offset` bytes below the thread
/// pointer (x86-64 variant II). A static program has one; flint-glibc's loader adds one per
/// loaded object with a PT_TLS segment.
#[derive(Clone, Copy)]
pub struct TlsModule {
    pub image: usize,
    pub filesz: usize,
    pub memsz: usize,
    pub align: usize,
    pub offset: usize,
}

pub const MAX_MODULES: usize = 64;
const NO_MODULE: TlsModule = TlsModule { image: 0, filesz: 0, memsz: 0, align: 1, offset: 0 };
/// Static TLS modules; module id N (as in dl_iterate_phdr and __tls_get_addr) is MODULES[N-1].
pub static mut MODULES: [TlsModule; MAX_MODULES] = [NO_MODULE; MAX_MODULES];
pub static mut NMODULES: usize = 0;
/// Bytes below the thread pointer for all static TLS (the loader may reserve extra, for
/// objects opened later that use the initial-exec model), and their alignment.
pub static mut STATIC_SIZE: usize = 0;
pub static mut STATIC_ALIGN: usize = 16;
/// Room set aside below the thread pointer in every thread (flint-glibc reserves some for
/// libraries opened later); the TLS area is at least this big.
pub static mut RESERVE: usize = 0;
static MAIN_TLS: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
pub fn current() -> *mut Thread {
    let t: *mut Thread;
    // SAFETY: %fs:0 holds the thread block's own address once init_main has run.
    unsafe { asm!("mov {}, qword ptr fs:0", out(reg) t, options(nostack, readonly, preserves_flags)) };
    t
}

pub fn round_up(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

/// Add a module to static TLS (before any thread but the main one exists, or into space
/// reserved for it). Returns its module id.
pub unsafe fn add_module(image: usize, filesz: usize, memsz: usize, align: usize) -> usize {
    let n = &mut *ptr::addr_of_mut!(NMODULES);
    let align = align.max(1);
    let size = &mut *ptr::addr_of_mut!(STATIC_SIZE);
    let offset = round_up(*size + memsz, align);
    *size = offset;
    let a = &mut *ptr::addr_of_mut!(STATIC_ALIGN);
    *a = (*a).max(align);
    (*ptr::addr_of_mut!(MODULES))[*n] = TlsModule { image, filesz, memsz, align, offset };
    *n += 1;
    *n
}

/// Bytes below the thread block taken by the TLS area.
pub fn tls_offset() -> usize {
    // SAFETY: written before any thread but the main one exists.
    unsafe { round_up(STATIC_SIZE.max(RESERVE), STATIC_ALIGN.max(1)) }
}

/// Bytes needed for the TLS area plus the thread block, and the block's alignment.
pub fn block_size() -> (usize, usize) {
    // SAFETY: as above.
    let align = unsafe { STATIC_ALIGN }.max(core::mem::align_of::<Thread>()).max(64);
    (tls_offset() + core::mem::size_of::<Thread>() + align, align)
}

/// Copy every static module's TLS image into the area below thread pointer `tp`.
pub unsafe fn init_static_tls(tp: usize) {
    let mods = &*ptr::addr_of!(MODULES);
    for m in &mods[..NMODULES] {
        let block = tp - m.offset;
        if m.filesz > 0 {
            ptr::copy_nonoverlapping(m.image as *const u8, block as *mut u8, m.filesz);
        }
        ptr::write_bytes((block + m.filesz) as *mut u8, 0, m.memsz - m.filesz);
    }
}

/// Lay out the TLS area and thread block inside `mem` (at least `block_size().0` bytes,
/// zeroed) and return the thread block.
pub unsafe fn setup_block(mem: usize) -> *mut Thread {
    let (_, align) = block_size();
    let tp = round_up(mem + tls_offset(), align);
    init_static_tls(tp);
    let t = tp as *mut Thread;
    (*t).tcb = t;
    (*t).self_ = t;
    if let Some(main) = (MAIN_TLS.load(Ordering::Relaxed) as *mut Thread).as_ref() {
        (*t).stack_guard = main.stack_guard;
        (*t).pointer_guard = main.pointer_guard;
        (*t).multiple_threads = 1;
    }
    t
}

pub unsafe fn set_fs(t: *mut Thread) {
    const ARCH_SET_FS: usize = 0x1002;
    sys::sc2(sys::ARCH_PRCTL, ARCH_SET_FS, t as usize);
}

/// The main thread: find PT_TLS in the program headers, build its block, point %fs at it.
pub unsafe fn init_main(phdr: usize, phnum: usize, random: usize) {
    // ELF64 program header: p_type u32, p_flags u32, p_offset, p_vaddr, p_paddr, p_filesz, p_memsz, p_align
    const PT_PHDR: u32 = 6;
    const PT_TLS: u32 = 7;
    let mut bias = 0usize;
    for i in 0..phnum {
        let p = phdr + i * 56;
        if *(p as *const u32) == PT_PHDR {
            bias = phdr.wrapping_sub(*((p + 16) as *const usize));
        }
    }
    for i in 0..phnum {
        let p = phdr + i * 56;
        if *(p as *const u32) == PT_TLS {
            add_module(
                bias.wrapping_add(*((p + 16) as *const usize)),
                *((p + 32) as *const usize),
                *((p + 40) as *const usize),
                *((p + 48) as *const usize),
            );
        }
    }
    init_main_block(random);
}

/// Build the main thread's block for the static TLS laid out so far and point %fs at it.
pub unsafe fn init_main_block(random: usize) {
    let (size, _) = block_size();
    let mem = sys::sc6(sys::MMAP, 0, size, 3 /* rw */, 0x22 /* private|anon */, usize::MAX, 0);
    if mem < 0 {
        sys::sc1(sys::EXIT_GROUP, 127);
    }
    let t = setup_block(mem as usize);
    if random != 0 {
        // AT_RANDOM: 16 random bytes from the kernel; the low byte of the canary stays zero
        // so string overflows cannot copy it
        (*t).stack_guard = *(random as *const usize) & !0xff;
        (*t).pointer_guard = *((random + 8) as *const usize);
    }
    (*t).tid.store(sys::sc0(sys::GETTID) as i32, Ordering::Relaxed);
    set_fs(t);
    MAIN_TLS.store(t as usize, Ordering::Relaxed);
}

/// Make `t` the main thread's block (flint-glibc builds a new one once it knows every
/// object's TLS).
pub unsafe fn set_main_thread(t: *mut Thread) {
    MAIN_TLS.store(t as usize, Ordering::Relaxed);
}

pub fn main_thread() -> *mut Thread {
    MAIN_TLS.load(Ordering::Relaxed) as *mut Thread
}
