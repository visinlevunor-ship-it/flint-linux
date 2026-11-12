//! AES-128-GCM (NIST SP 800-38D), the cipher TLS 1.3 servers must support.
//!
//! With AES-NI and PCLMULQDQ (every x86-64 CPU of the last decade) the counter blocks are
//! encrypted eight at a time and GHASH is a carry-less multiply; otherwise both run in
//! software.

use crate::aes::Aes128;

pub struct Gcm {
    aes: Aes128,
    h: u128,
    clmul: bool,
}

#[cfg(target_arch = "x86_64")]
mod clmul {
    use std::arch::x86_64::*;

    /// GF(2^128) multiply in GCM's bit order (Gueron & Kounavis, Intel white paper
    /// "Carry-Less Multiplication and Its Usage for Computing the GCM Mode", algorithm 5).
    /// Inputs and output are byte-reversed blocks, which is what a u128 read big-endian
    /// looks like in a little-endian register.
    ///
    /// # Safety
    /// The CPU must support PCLMULQDQ (checked by the caller).
    #[target_feature(enable = "pclmulqdq,sse2")]
    pub unsafe fn mul(a: u128, b: u128) -> u128 {
        let a: __m128i = core::mem::transmute(a);
        let b: __m128i = core::mem::transmute(b);
        let mut t3 = _mm_clmulepi64_si128(a, b, 0x00);
        let mut t4 = _mm_clmulepi64_si128(a, b, 0x10);
        let mut t5 = _mm_clmulepi64_si128(a, b, 0x01);
        let mut t6 = _mm_clmulepi64_si128(a, b, 0x11);
        t4 = _mm_xor_si128(t4, t5);
        t5 = _mm_slli_si128(t4, 8);
        t4 = _mm_srli_si128(t4, 8);
        t3 = _mm_xor_si128(t3, t5);
        t6 = _mm_xor_si128(t6, t4);
        // shift the 256-bit product left by one (the bit order is reflected)
        let mut t7 = _mm_srli_epi32(t3, 31);
        let mut t8 = _mm_srli_epi32(t6, 31);
        t3 = _mm_slli_epi32(t3, 1);
        t6 = _mm_slli_epi32(t6, 1);
        let mut t9 = _mm_srli_si128(t7, 12);
        t8 = _mm_slli_si128(t8, 4);
        t7 = _mm_slli_si128(t7, 4);
        t3 = _mm_or_si128(t3, t7);
        t6 = _mm_or_si128(t6, t8);
        t6 = _mm_or_si128(t6, t9);
        // reduce modulo x^128 + x^7 + x^2 + x + 1
        t7 = _mm_slli_epi32(t3, 31);
        t8 = _mm_slli_epi32(t3, 30);
        t9 = _mm_slli_epi32(t3, 25);
        t7 = _mm_xor_si128(t7, t8);
        t7 = _mm_xor_si128(t7, t9);
        t8 = _mm_srli_si128(t7, 4);
        t7 = _mm_slli_si128(t7, 12);
        t3 = _mm_xor_si128(t3, t7);
        let mut t2 = _mm_srli_epi32(t3, 1);
        t4 = _mm_srli_epi32(t3, 2);
        t5 = _mm_srli_epi32(t3, 7);
        t2 = _mm_xor_si128(t2, t4);
        t2 = _mm_xor_si128(t2, t5);
        t2 = _mm_xor_si128(t2, t8);
        t3 = _mm_xor_si128(t3, t2);
        t6 = _mm_xor_si128(t6, t3);
        core::mem::transmute(t6)
    }
}

fn has_clmul() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("pclmulqdq") && std::arch::is_x86_feature_detected!("sse2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
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
        Gcm { aes, h: u128::from_be_bytes(h), clmul: has_clmul() }
    }

    fn mul_h(&self, x: u128) -> u128 {
        #[cfg(target_arch = "x86_64")]
        if self.clmul {
            // SAFETY: clmul is only set when the CPU has PCLMULQDQ.
            return unsafe { clmul::mul(x, self.h) };
        }
        gf_mul(x, self.h)
    }

    fn ghash(&self, aad: &[u8], ct: &[u8]) -> u128 {
        let mut y = 0u128;
        for c in aad.chunks(16) {
            y = self.mul_h(y ^ block(c));
        }
        for c in ct.chunks(16) {
            y = self.mul_h(y ^ block(c));
        }
        let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
        self.mul_h(y ^ lens)
    }

    fn ctr(&self, nonce: &[u8; 12], data: &mut [u8]) {
        // 512 bytes of key stream at a time: eight-block batches, no allocation.
        const N: usize = 32;
        let mut counter = 2u32;
        let mut ks = [[0u8; 16]; N];
        for chunk in data.chunks_mut(16 * N) {
            let n = chunk.len().div_ceil(16);
            for b in &mut ks[..n] {
                b[..12].copy_from_slice(nonce);
                b[12..].copy_from_slice(&counter.to_be_bytes());
                counter = counter.wrapping_add(1);
            }
            self.aes.encrypt_blocks(&mut ks[..n]);
            for (d, k) in chunk.iter_mut().zip(ks.iter().flatten()) {
                *d ^= k;
            }
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

    #[test]
    fn clmul_matches_software() {
        if !has_clmul() {
            return;
        }
        let mut x = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128;
        for i in 0..200u32 {
            let y = x.rotate_left(i % 128) ^ (i as u128 * 0x9e37_79b9_7f4a_7c15);
            // SAFETY: checked above.
            assert_eq!(unsafe { clmul::mul(x, y) }, gf_mul(x, y), "round {i}");
            x = gf_mul(x, y) ^ 1;
        }
        // a long message through both paths
        let key = [7u8; 16];
        let mut fast = Gcm::new(&key);
        let mut a: Vec<u8> = (0..5000u32).map(|i| (i * 31) as u8).collect();
        let mut b = a.clone();
        let t1 = fast.seal(&[1; 12], b"hdr", &mut a);
        fast.clmul = false;
        fast.aes = {
            let mut s = Aes128::new(&key);
            s.disable_hw();
            s
        };
        let t2 = fast.seal(&[1; 12], b"hdr", &mut b);
        assert_eq!(a, b);
        assert_eq!(t1, t2);
    }
}
