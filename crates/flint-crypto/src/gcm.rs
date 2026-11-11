//! AES-128-GCM (NIST SP 800-38D), the cipher TLS 1.3 servers must support.

use crate::aes::Aes128;

pub struct Gcm {
    aes: Aes128,
    h: u128,
}

fn gf_mul(x: u128, y: u128) -> u128 {
    // Bits are numbered from the most significant end, as GCM defines them.
    const R: u128 = 0xE1 << 120;
    let mut z = 0u128;
    let mut v = x;
    for i in 0..128 {
        if (y >> (127 - i)) & 1 == 1 {
            z ^= v;
        }
        v = if v & 1 == 1 { (v >> 1) ^ R } else { v >> 1 };
    }
    z
}

fn block(b: &[u8]) -> u128 {
    let mut a = [0u8; 16];
    a[..b.len()].copy_from_slice(b);
    u128::from_be_bytes(a)
}

impl Gcm {
    pub fn new(key: &[u8; 16]) -> Gcm {
        let aes = Aes128::new(key);
        let mut h = [0u8; 16];
        aes.encrypt(&mut h);
        Gcm { aes, h: u128::from_be_bytes(h) }
    }

    fn ghash(&self, aad: &[u8], ct: &[u8]) -> u128 {
        let mut y = 0u128;
        for c in aad.chunks(16) {
            y = gf_mul(y ^ block(c), self.h);
        }
        for c in ct.chunks(16) {
            y = gf_mul(y ^ block(c), self.h);
        }
        let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
        gf_mul(y ^ lens, self.h)
    }

    fn ctr(&self, nonce: &[u8; 12], data: &mut [u8]) {
        let mut counter = 2u32;
        for chunk in data.chunks_mut(16) {
            let mut ks = [0u8; 16];
            ks[..12].copy_from_slice(nonce);
            ks[12..].copy_from_slice(&counter.to_be_bytes());
            self.aes.encrypt(&mut ks);
            for (d, k) in chunk.iter_mut().zip(ks) {
                *d ^= k;
            }
            counter = counter.wrapping_add(1);
        }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;
        self.aes.encrypt(&mut j0);
        (self.ghash(aad, ct) ^ u128::from_be_bytes(j0)).to_be_bytes()
    }

    /// Encrypt in place and return the tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
        self.ctr(nonce, data);
        self.tag(nonce, aad, data)
    }

    /// Check the tag, then decrypt in place. False (data untouched) if it does not verify.
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8], tag: &[u8]) -> bool {
        if !crate::ct_eq(&self.tag(nonce, aad, data), tag) {
            return false;
        }
        self.ctr(nonce, data);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hex, unhex};

    #[test]
    fn nist_vectors() {
        // GCM spec test case 2: zero key, zero IV, one zero block
        let g = Gcm::new(&[0; 16]);
        let mut d = [0u8; 16];
        let tag = g.seal(&[0; 12], &[], &mut d);
        assert_eq!(hex(&d), "0388dace60b6a392f328c2b971b2fe78");
        assert_eq!(hex(&tag), "ab6e47d42cec13bdf53a67b21257bddf");
        // Test case 4: with AAD and a partial last block
        let key: [u8; 16] = unhex("feffe9928665731c6d6a8f9467308308").unwrap().try_into().unwrap();
        let iv: [u8; 12] = unhex("cafebabefacedbaddecaf888").unwrap().try_into().unwrap();
        let mut pt = unhex("d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39").unwrap();
        let aad = unhex("feedfacedeadbeeffeedfacedeadbeefabaddad2").unwrap();
        let g = Gcm::new(&key);
        let tag = g.seal(&iv, &aad, &mut pt);
        assert_eq!(hex(&pt), "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091");
        assert_eq!(hex(&tag), "5bc94fbc3221a5db94fae95ae7121a47");
        assert!(g.open(&iv, &aad, &mut pt, &tag));
        assert_eq!(hex(&pt[..4]), "d9313225");
        let mut bad = pt.clone();
        g.seal(&iv, &aad, &mut bad);
        assert!(!g.open(&iv, b"other", &mut bad, &tag));
    }
}
