//! PBKDF2-HMAC-SHA256 (RFC 8018).

use crate::hmac::hmac_sha256;

/// Fill `out` with the derived key. `iterations` must be at least 1.
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    assert!(iterations >= 1, "iterations must be >= 1");
    for (block_index, chunk) in out.chunks_mut(32).enumerate() {
        let mut msg = salt.to_vec();
        msg.extend_from_slice(&(block_index as u32 + 1).to_be_bytes());
        let mut u = hmac_sha256(password, &msg);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for (a, b) in t.iter_mut().zip(u) {
                *a ^= b;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hex;

    #[test]
    fn rfc7914_vector() {
        let mut out = [0u8; 64];
        pbkdf2_hmac_sha256(b"passwd", b"salt", 1, &mut out);
        assert_eq!(
            hex(&out),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783"
        );
    }

    #[test]
    fn more_iterations() {
        let mut out = [0u8; 32];
        pbkdf2_hmac_sha256(b"password", b"salt", 2, &mut out);
        assert_eq!(hex(&out), "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43");
        pbkdf2_hmac_sha256(b"password", b"salt", 4096, &mut out);
        assert_eq!(hex(&out), "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a");
    }
}
