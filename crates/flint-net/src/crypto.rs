//! The cryptography WPA2-Personal needs: SHA-1, HMAC-SHA1, PBKDF2 (passphrase to key),
//! the 802.11 PRF (key derivation), and AES-128 key unwrap (RFC 3394) for the group key.
//! Frame encryption itself (CCMP) is done by the kernel or the card.

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

// ---------------------------------------------------------------------------------------
// AES-128

const SBOX: [u8; 256] = {
    // Generated from the multiplicative inverse in GF(2^8) and the affine map, at compile time.
    let mut s = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        // p * 3
        p = p ^ (p << 1) ^ (if p & 0x80 != 0 { 0x1B } else { 0 });
        // q / 3
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        s[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    s[0] = 0x63;
    s
};

const INV_SBOX: [u8; 256] = {
    let mut inv = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inv[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    inv
};

fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1B } else { 0 }
}

fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut r = 0;
    while b != 0 {
        if b & 1 != 0 {
            r ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    r
}

pub struct Aes128 {
    rk: [[u8; 16]; 11],
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Aes128 {
        let mut w = [[0u8; 4]; 44];
        for i in 0..4 {
            w[i].copy_from_slice(&key[i * 4..i * 4 + 4]);
        }
        let mut rcon = 1u8;
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xtime(rcon);
            }
            for j in 0..4 {
                w[i][j] = w[i - 4][j] ^ t[j];
            }
        }
        let mut rk = [[0u8; 16]; 11];
        for r in 0..11 {
            for c in 0..4 {
                rk[r][c * 4..c * 4 + 4].copy_from_slice(&w[r * 4 + c]);
            }
        }
        Aes128 { rk }
    }

    fn add(s: &mut [u8; 16], k: &[u8; 16]) {
        for (a, b) in s.iter_mut().zip(k) {
            *a ^= b;
        }
    }

    pub fn encrypt(&self, b: &mut [u8; 16]) {
        Self::add(b, &self.rk[0]);
        for r in 1..11 {
            for x in b.iter_mut() {
                *x = SBOX[*x as usize];
            }
            // ShiftRows (column-major state: byte i is row i%4, column i/4)
            let t = *b;
            for c in 0..4 {
                for row in 0..4 {
                    b[c * 4 + row] = t[((c + row) % 4) * 4 + row];
                }
            }
            if r != 10 {
                for c in 0..4 {
                    let col = [b[c * 4], b[c * 4 + 1], b[c * 4 + 2], b[c * 4 + 3]];
                    for row in 0..4 {
                        b[c * 4 + row] = mul(col[row], 2) ^ mul(col[(row + 1) % 4], 3) ^ col[(row + 2) % 4] ^ col[(row + 3) % 4];
                    }
                }
            }
            Self::add(b, &self.rk[r]);
        }
    }

    pub fn decrypt(&self, b: &mut [u8; 16]) {
        Self::add(b, &self.rk[10]);
        for r in (0..10).rev() {
            let t = *b;
            for c in 0..4 {
                for row in 0..4 {
                    b[((c + row) % 4) * 4 + row] = t[c * 4 + row];
                }
            }
            for x in b.iter_mut() {
                *x = INV_SBOX[*x as usize];
            }
            Self::add(b, &self.rk[r]);
            if r != 0 {
                for c in 0..4 {
                    let col = [b[c * 4], b[c * 4 + 1], b[c * 4 + 2], b[c * 4 + 3]];
                    for row in 0..4 {
                        b[c * 4 + row] = mul(col[row], 14) ^ mul(col[(row + 1) % 4], 11) ^ mul(col[(row + 2) % 4], 13) ^ mul(col[(row + 3) % 4], 9);
                    }
                }
            }
        }
    }
}

/// AES key unwrap (RFC 3394). None if the integrity check fails (wrong key or tampered).
pub fn aes_unwrap(kek: &[u8; 16], wrapped: &[u8]) -> Option<Vec<u8>> {
    if wrapped.len() < 24 || wrapped.len() % 8 != 0 {
        return None;
    }
    let n = wrapped.len() / 8 - 1;
    let aes = Aes128::new(kek);
    let mut a: [u8; 8] = wrapped[..8].try_into().unwrap();
    let mut r: Vec<[u8; 8]> = wrapped[8..].chunks_exact(8).map(|c| c.try_into().unwrap()).collect();
    for j in (0..6).rev() {
        for i in (0..n).rev() {
            let t = (n * j + i + 1) as u64;
            let mut b = [0u8; 16];
            for (k, x) in t.to_be_bytes().iter().enumerate() {
                b[k] = a[k] ^ x;
            }
            b[8..].copy_from_slice(&r[i]);
            aes.decrypt(&mut b);
            a.copy_from_slice(&b[..8]);
            r[i].copy_from_slice(&b[8..]);
        }
    }
    (a == [0xA6; 8]).then(|| r.concat())
}

/// AES key wrap (RFC 3394), used by the tests and the access point side of them.
pub fn aes_wrap(kek: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    let n = plain.len() / 8;
    let aes = Aes128::new(kek);
    let mut a = [0xA6u8; 8];
    let mut r: Vec<[u8; 8]> = plain.chunks_exact(8).map(|c| c.try_into().unwrap()).collect();
    for j in 0..6 {
        for i in 0..n {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i]);
            aes.encrypt(&mut b);
            let t = (n * j + i + 1) as u64;
            for (k, x) in t.to_be_bytes().iter().enumerate() {
                a[k] = b[k] ^ x;
            }
            r[i].copy_from_slice(&b[8..]);
        }
    }
    let mut out = a.to_vec();
    out.extend(r.concat());
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

    #[test]
    fn aes_and_key_wrap() {
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let mut b: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
        Aes128::new(&key).encrypt(&mut b);
        assert_eq!(hex(&b), "69c4e0d86a7b0430d8cdb78070b4c55a");
        Aes128::new(&key).decrypt(&mut b);
        assert_eq!(hex(&b), "00112233445566778899aabbccddeeff");
        let wrapped = unhex("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5").unwrap();
        let plain = unhex("00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(aes_unwrap(&key, &wrapped).unwrap(), plain);
        assert_eq!(aes_wrap(&key, &plain), wrapped);
        let mut bad = wrapped.clone();
        bad[5] ^= 1;
        assert!(aes_unwrap(&key, &bad).is_none());
    }
}
