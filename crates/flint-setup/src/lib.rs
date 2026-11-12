//! `flint-install` (put Flint on a disk) and `flint-update` (bring an installed Flint up to
//! date).

mod copy;
pub mod efi;
mod install;

pub use install::main as install;
