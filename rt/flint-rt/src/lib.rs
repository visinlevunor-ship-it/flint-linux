//! flint-rt: the runtime Flint programs use instead of libc.
//!
//! A program is `#![no_std] #![no_main]` and declares its entry with
//! [`entry!`]. This crate provides everything a libc normally would for such a
//! program: the process entry point (`_start`), raw system calls, a heap, the
//! `mem*` functions the compiler needs, a panic handler, and tiny stdio.
//!
//! Scope and limits (deliberate, for now):
//! * Linux on x86_64 only.
//! * Single threaded: the heap has no locking. Add one before using threads.
//! * Not a C library: no C ABI for third-party C code, no stdio buffering
//!   beyond a small per-call buffer, no locale, no networking. Flint's own
//!   Rust programs do not need any of that.
//!
//! Needs `panic = "abort"` in the profile (see rt/Cargo.toml).

#![no_std]
// The mem* functions below must not be recognised by LLVM as memcpy/memset
// idioms, or they would compile into calls to themselves.
#![no_builtins]

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("flint-rt supports Linux on x86_64 only");

extern crate alloc;

mod arch;
mod heap;
pub mod io;
mod mem;
mod panic;
pub mod selftest;
mod start;
pub mod sys;

pub use start::{args, Args};

#[global_allocator]
static HEAP: heap::FlintHeap = heap::FlintHeap::new();

/// Declare the program's entry point: `entry!(main);` where `fn main() -> i32`.
/// The return value is the process exit status.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        pub fn flint_main() -> i32 {
            let f: fn() -> i32 = $main;
            f()
        }
    };
}
