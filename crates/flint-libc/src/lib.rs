//! flint-libc: Flint's own C library, written in Rust.
//!
//! It provides the C functions that Rust's standard library calls on Linux, with the same
//! ABI (calling conventions and struct layouts) as musl, so Rust's
//! `x86_64-unknown-linux-musl` std links against it unchanged. Flint's programs then contain
//! no musl code at all: everything between them and the kernel is in this crate (and the
//! unwinder in flint-unwind).
//!
//! The thread control block follows glibc's layout on x86-64 (self pointer at %fs:0, stack
//! guard at %fs:0x28), so the same code can later stand in for glibc too.
//!
//! On any target other than x86-64 Linux with `target_env = "musl"` the crate is empty: the
//! host's own libc is used there (tests on a glibc host, the FreeBSD build checks).

#![cfg_attr(not(test), no_std)]
#![allow(clippy::missing_safety_doc)]
#![allow(non_camel_case_types)]
// built for flint-glibc, some of the static-program machinery goes unused
#![cfg_attr(feature = "glibc", allow(unused))]

/// The math functions, also tested on the build host against its libm.
#[allow(dead_code)]
#[doc(hidden)]
pub mod math;

// public for flint-glibc (rt/flint-glibc), which builds the glibc ABI on these pieces
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "musl"))]
#[doc(hidden)]
pub mod imp {
    pub mod sys;
    pub mod types;
    pub mod errno;
    pub mod lock;
    pub mod start;
    pub mod tls;
    pub mod mem;
    pub mod malloc;
    pub mod env;
    pub mod file;
    pub mod dir;
    pub mod thread;
    pub mod signal;
    pub mod process;
    pub mod net;
    pub mod tty;
    pub mod time;
    pub mod misc;
    pub mod strerror;
    // flint-glibc exports its own libm
    #[cfg(not(feature = "glibc"))]
    pub mod math;
    // glibc programs bring libgcc_s, which has the unwinder
    #[cfg(not(feature = "glibc"))]
    pub mod unwind;
    pub mod resolve;
    pub mod pwd;
}

/// Make sure the linker keeps this crate: referenced from flint-core.
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "musl"))]
pub fn linked() -> bool {
    true
}
