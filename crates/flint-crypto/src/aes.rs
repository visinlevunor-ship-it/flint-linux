//! AES-128 (FIPS 197) and AES key wrap (RFC 3394).
//!
//! Encryption uses the CPU's AES instructions (AES-NI) when it has them, checked once per
//! key; the table-free software rounds are the fallback.

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
    hw: bool,
}

/// Whether this CPU has the AES instructions (always false off x86-64).
pub fn has_aes_ni() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("aes") && std::arch::is_x86_feature_detected!("sse2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

#[cfg(target_arch = "x86_64")]
mod ni {
    use std::arch::x86_64::*;

    /// Encrypt each block with the expanded key, eight at a time so the AES unit stays busy.
    ///
    /// # Safety
    /// The CPU must support AES-NI (checked by the caller).
    #[target_feature(enable = "aes,sse2")]
    pub unsafe fn encrypt(rk: &[[u8; 16]; 11], blocks: &mut [[u8; 16]]) {
        let mut k = [_mm_setzero_si128(); 11];
        for (d, s) in k.iter_mut().zip(rk) {
            *d = _mm_loadu_si128(s.as_ptr().cast());
        }
        let mut chunks = blocks.chunks_exact_mut(8);
        for c in &mut chunks {
            let mut x = [_mm_setzero_si128(); 8];
            for i in 0..8 {
                x[i] = _mm_xor_si128(_mm_loadu_si128(c[i].as_ptr().cast()), k[0]);
            }
            for rk in &k[1..10] {
                for v in x.iter_mut() {
                    *v = _mm_aesenc_si128(*v, *rk);
                }
            }
            for i in 0..8 {
                _mm_storeu_si128(c[i].as_mut_ptr().cast(), _mm_aesenclast_si128(x[i], k[10]));
            }
        }
        for b in chunks.into_remainder() {
            let mut v = _mm_xor_si128(_mm_loadu_si128(b.as_ptr().cast()), k[0]);
            for rk in &k[1..10] {
                v = _mm_aesenc_si128(v, *rk);
            }
            _mm_storeu_si128(b.as_mut_ptr().cast(), _mm_aesenclast_si128(v, k[10]));
        }
    }
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
        Aes128 { rk, hw: has_aes_ni() }
    }

    #[cfg(test)]
    pub(crate) fn disable_hw(&mut self) {
        self.hw = false;
    }

    fn add(s: &mut [u8; 16], k: &[u8; 16]) {
        for (a, b) in s.iter_mut().zip(k) {
            *a ^= b;
        }
    }

    pub fn encrypt(&self, b: &mut [u8; 16]) {
        self.encrypt_blocks(core::slice::from_mut(b));
    }

    /// Encrypt several independent blocks (ECB; CTR mode builds on it).
    pub fn encrypt_blocks(&self, blocks: &mut [[u8; 16]]) {
        #[cfg(target_arch = "x86_64")]
        if self.hw {
            // SAFETY: hw is only set when the CPU has AES-NI.
            unsafe { ni::encrypt(&self.rk, blocks) };
            return;
        }
        for b in blocks {
            self.encrypt_soft(b);
        }
    }

    fn encrypt_soft(&self, b: &mut [u8; 16]) {
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
                        let (a, n) = (col[row], col[(row + 1) % 4]);
                        b[c * 4 + row] = xtime(a) ^ xtime(n) ^ n ^ col[(row + 2) % 4] ^ col[(row + 3) % 4];
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hex, unhex};

    #[test]
    fn fips197_and_rfc3394() {
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
    }

    #[test]
    fn hardware_matches_software() {
        let key: [u8; 16] = core::array::from_fn(|i| (i as u8).wrapping_mul(37) ^ 0x5a);
        let mut hw = Aes128::new(&key);
        let mut blocks: Vec<[u8; 16]> = (0..21u8).map(|n| core::array::from_fn(|i| n.wrapping_mul(13) ^ i as u8)).collect();
        let mut soft = blocks.clone();
        hw.encrypt_blocks(&mut blocks);
        hw.hw = false;
        hw.encrypt_blocks(&mut soft);
        assert_eq!(blocks, soft);
    }
}
