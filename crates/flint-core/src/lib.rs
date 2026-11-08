//! Shared pieces for the Flint Linux userland.
//!
//! With the default `std` feature this is the full toolbox. Without it (for
//! programs built on `flint-rt`) only [`version`] is available.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
pub mod log;
#[cfg(feature = "std")]
pub mod power;
#[cfg(feature = "std")]
pub mod sys;
pub mod version;

#[cfg(feature = "std")]
pub use power::PowerAction;
