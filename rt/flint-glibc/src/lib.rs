//! flint-glibc: glibc's ABI on top of flint-libc.
//!
//! Programs built for glibc (Firefox, Java, anything from another distribution) expect a
//! dynamic loader at /lib64/ld-linux-x86-64.so.2 and a C library called libc.so.6 with
//! glibc's functions, structure layouts and symbol versions. This crate is both, in one
//! shared object, linked by `build.sh`:
//!
//!  - `boot` and `loader`: the dynamic loader. It maps the program's libraries, relocates
//!    them, sets up thread-local storage and runs their constructors. Every name glibc's
//!    own pieces go by (libc.so.6, libm.so.6, libpthread.so.0, libdl.so.2, ...) is answered
//!    by this one object.
//!  - `dl`: dlopen, dlsym and friends, dl_iterate_phdr, __tls_get_addr.
//!  - `libc`: the glibc functions that flint-libc (which has musl's ABI, for Rust programs)
//!    does not have, and glibc's versions of the ones whose behaviour differs.
//!
//! Only Flint's code runs underneath: no glibc and no musl.

#![no_std]
#![feature(c_variadic)]
#![allow(clippy::missing_safety_doc)]
#![allow(non_camel_case_types)]
#![allow(static_mut_refs)]

extern crate alloc;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use flint_libc::imp as fl;

pub mod sys;
pub mod elf;
pub mod boot;
pub mod loader;
pub mod dl;
pub mod libc;

/// The loader's own memory: flint-libc's allocator, never a program's interposed malloc.
struct InternalAlloc;

unsafe impl core::alloc::GlobalAlloc for InternalAlloc {
    unsafe fn alloc(&self, l: core::alloc::Layout) -> *mut u8 {
        if l.align() <= 16 {
            fl::malloc::imalloc(l.size())
        } else {
            let mut p = core::ptr::null_mut();
            if fl::malloc::posix_memalign(&mut p, l.align(), l.size()) != 0 {
                return core::ptr::null_mut();
            }
            p
        }
    }
    unsafe fn dealloc(&self, p: *mut u8, _l: core::alloc::Layout) {
        fl::malloc::ifree(p)
    }
    unsafe fn realloc(&self, p: *mut u8, l: core::alloc::Layout, n: usize) -> *mut u8 {
        if l.align() <= 16 {
            fl::malloc::irealloc(p, n)
        } else {
            let q = self.alloc(core::alloc::Layout::from_size_align_unchecked(n, l.align()));
            if !q.is_null() {
                core::ptr::copy_nonoverlapping(p, q, l.size().min(n));
                self.dealloc(p, l);
            }
            q
        }
    }
}

#[global_allocator]
static ALLOC: InternalAlloc = InternalAlloc;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    let mut w = sys::Stderr;
    let _ = write!(w, "flint-glibc: internal error: {info}\n");
    sys::exit_group(127)
}
