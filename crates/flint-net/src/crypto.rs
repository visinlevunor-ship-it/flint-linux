//! The cryptography WPA2-Personal needs: SHA-1, HMAC-SHA1, PBKDF2 (passphrase to key),
//! the 802.11 PRF (key derivation), and AES-128 key unwrap (RFC 3394) for the group key.
//! Frame encryption itself (CCMP) is done by the kernel or the card.

pub use flint_crypto::aes::{aes_unwrap, aes_wrap, Aes128};

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&sha1(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    for p in parts {
        inner.extend_from_slice(p);
    }
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5C).collect();
    outer.extend_from_slice(&sha1(&inner));
    sha1(&outer)
}

/// The WPA pre-shared key: PBKDF2-HMAC-SHA1(passphrase, SSID, 4096 rounds, 32 bytes).
pub fn wpa_psk(passphrase: &[u8], ssid: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (block, chunk) in out.chunks_mut(20).enumerate() {
        let idx = (block as u32 + 1).to_be_bytes();
        let mut u = hmac_sha1(passphrase, &[ssid, &idx]);
        let mut t = u;
        for _ in 1..4096 {
            u = hmac_sha1(passphrase, &[&u]);
            for (x, y) in t.iter_mut().zip(u.iter()) {
                *x ^= y;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
    out
}

/// IEEE 802.11 PRF-n: HMAC-SHA1(key, label || 0 || data || counter), concatenated.
pub fn prf(key: &[u8], label: &str, data: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut i = 0u8;
    while out.len() < len {
        out.extend_from_slice(&hmac_sha1(key, &[label.as_bytes(), &[0], data, &[i]]));
        i += 1;
    }
    out.truncate(len);
    out
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_and_hmac() {
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(&sha1(&[b'a'; 1000])), "291e9a6c66994949b57ba5e650361e98fc36b1ba");
        assert_eq!(hex(&hmac_sha1(&[0x0b; 20], &[b"Hi There"])), "b617318655057264e28bc0b6fb378c8ef146be00");
        assert_eq!(hex(&hmac_sha1(b"Jefe", &[b"what do ya want ", b"for nothing?"])), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn passphrase_to_psk() {
        // IEEE 802.11 test vectors
        assert_eq!(hex(&wpa_psk(b"password", b"IEEE")), "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e");
        assert_eq!(hex(&wpa_psk(b"ThisIsAPassword", b"ThisIsASSID")), "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af");
    }

    #[test]
    fn prf_vector() {
        let out = prf(&[0x0b; 20], "prefix", b"Hi There", 64);
        assert_eq!(hex(&out), "bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606e17d8da35402ffee75df78c3d31e0f889f012120c0862beb67753e7439ae242edb8373698356cf5a");
    }

}
