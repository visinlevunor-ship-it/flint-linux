//! Shared pieces for the Flint Linux userland.
//!
//! With the default `std` feature this is the full toolbox. Without it (for
//! programs built on `flint-rt`) only [`version`] is available.

#![cfg_attr(not(feature = "std"), no_std)]

// Links flint-libc, Flint's C library, into every program built for Linux (no musl).
#[cfg(all(feature = "std", target_os = "linux", target_arch = "x86_64", target_env = "musl"))]
extern crate flint_libc as _;

#[cfg(feature = "std")]
pub mod log;
#[cfg(feature = "std")]
pub mod power;
#[cfg(feature = "std")]
pub mod sys;
#[cfg(feature = "std")]
pub mod term;
#[cfg(feature = "std")]
pub mod tty;
pub mod version;

#[cfg(feature = "std")]
pub use power::PowerAction;
