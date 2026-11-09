//! Flint's stored password format:
//!
//! ```text
//! $flint1$<iterations>$<salt, 32 hex digits>$<PBKDF2-HMAC-SHA256, 64 hex digits>
//! ```
//!
//! A stored field that is empty or starts with `!` or `*` means the account has
//! no usable password (locked).

use std::fmt::Write as _;
use std::io::Read;

use crate::pbkdf2::pbkdf2_hmac_sha256;

pub const PREFIX: &str = "$flint1$";
pub const DEFAULT_ITERATIONS: u32 = 100_000;
const MAX_ITERATIONS: u32 = 10_000_000;
pub const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub enum HashError {
    /// Not in Flint's format.
    Malformed,
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

pub fn hash_password(password: &str, salt: &[u8; SALT_LEN], iterations: u32) -> String {
    let mut key = [0u8; KEY_LEN];
    pbkdf2_hmac_sha256(password.as_bytes(), salt, iterations, &mut key);
    format!("{PREFIX}{iterations}${}${}", hex(salt), hex(&key))
}

/// A fresh random salt from the kernel.
pub fn random_salt() -> std::io::Result<[u8; SALT_LEN]> {
    let mut salt = [0u8; SALT_LEN];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut salt)?;
    Ok(salt)
}

/// Hash `password` with a new random salt and the default iteration count.
pub fn hash_new(password: &str) -> std::io::Result<String> {
    Ok(hash_password(password, &random_salt()?, DEFAULT_ITERATIONS))
}

/// No usable password: empty, or starting with `!` or `*`.
pub fn is_locked(stored: &str) -> bool {
    stored.is_empty() || stored.starts_with('!') || stored.starts_with('*')
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Check `password` against a stored hash in constant time (per hash length).
pub fn verify(password: &str, stored: &str) -> Result<bool, HashError> {
    let rest = stored.strip_prefix(PREFIX).ok_or(HashError::Malformed)?;
    let mut parts = rest.split('$');
    let (iters, salt, want) = (
        parts.next().ok_or(HashError::Malformed)?,
        parts.next().ok_or(HashError::Malformed)?,
        parts.next().ok_or(HashError::Malformed)?,
    );
    if parts.next().is_some() {
        return Err(HashError::Malformed);
    }
    let iterations: u32 = iters.parse().map_err(|_| HashError::Malformed)?;
    if !(1..=MAX_ITERATIONS).contains(&iterations) {
        return Err(HashError::Malformed);
    }
    let salt = unhex(salt).ok_or(HashError::Malformed)?;
    let want = unhex(want).ok_or(HashError::Malformed)?;
    if want.len() != KEY_LEN || salt.is_empty() {
        return Err(HashError::Malformed);
    }
    let mut key = [0u8; KEY_LEN];
    pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations, &mut key);
    Ok(ct_eq(&key, &want))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn salt() -> [u8; SALT_LEN] {
        let mut s = [0u8; SALT_LEN];
        for (i, b) in s.iter_mut().enumerate() {
            *b = i as u8;
        }
        s
    }

    #[test]
    fn known_hash_matches_python() {
        let h = hash_password("flint", &salt(), 1000);
        assert_eq!(
            h,
            "$flint1$1000$000102030405060708090a0b0c0d0e0f$f39ca00c059f2a31fef288d3fedfc58cae11dba414cfb99e9f50a527c64d47ce"
        );
    }

    #[test]
    fn verify_accepts_right_and_rejects_wrong() {
        let h = hash_password("correct horse", &salt(), 50);
        assert_eq!(verify("correct horse", &h), Ok(true));
        assert_eq!(verify("correct horsf", &h), Ok(false));
        assert_eq!(verify("", &h), Ok(false));
    }

    #[test]
    fn verify_rejects_malformed() {
        for bad in [
            "",
            "x",
            "$flint1$",
            "$flint1$10$00$",
            "$flint1$0$00$00",
            "$flint1$abc$00$00",
            "$flint1$10$zz$00",
            "$flint1$10$00$00$extra",
            "$6$salt$hash",
            "$flint1$99999999$000102030405060708090a0b0c0d0e0f$f39ca00c059f2a31fef288d3fedfc58cae11dba414cfb99e9f50a527c64d47ce",
        ] {
            assert_eq!(verify("x", bad), Err(HashError::Malformed), "{bad:?}");
        }
    }

    #[test]
    fn locked_detection() {
        assert!(is_locked(""));
        assert!(is_locked("!"));
        assert!(is_locked("*"));
        assert!(is_locked("!$flint1$..."));
        assert!(!is_locked("$flint1$1$00$00"));
    }

    #[test]
    fn random_salts_differ() {
        let a = random_salt().unwrap();
        let b = random_salt().unwrap();
        assert_ne!(a, b);
    }
}
