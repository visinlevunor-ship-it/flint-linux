//! Cryptography for Flint, in pure Rust with no dependencies beyond std:
//! SHA-512, Ed25519 signatures (package signing), X25519 key exchange, AES-128-GCM and
//! HKDF-SHA256 (TLS 1.3). SHA-256 and HMAC-SHA256 come from flint-auth.
//!
//! The curve arithmetic follows TweetNaCl (public domain): small, well studied, and easy
//! to check against the reference.

pub mod aes;
pub mod curve25519;
pub mod gcm;
pub mod hkdf;
pub mod sha512;

pub use flint_auth::hmac::hmac_sha256;
pub use flint_auth::sha256;

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Comparison that takes the same time whether or not (and where) the inputs differ.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Random bytes from the kernel.
pub fn random(buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}
