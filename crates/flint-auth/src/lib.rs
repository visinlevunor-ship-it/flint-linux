//! Flint account handling. Pure Rust and std-only, so it also builds on non-Linux hosts.
//!
//! * [`sha256`], [`hmac`], [`pbkdf2`]: the primitives behind password hashes.
//! * [`hash`]: Flint's stored password format and verification.
//! * [`files`]: /etc/passwd, /etc/shadow and /etc/group parsing and editing.
//! * [`login`]: the authentication decision `flint-login` makes.

pub mod files;
pub mod hash;
pub mod hmac;
pub mod login;
pub mod pbkdf2;
pub mod sha256;
