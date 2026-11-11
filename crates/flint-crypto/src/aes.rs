//! AES-128 (FIPS 197) and AES key wrap (RFC 3394).


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
}
