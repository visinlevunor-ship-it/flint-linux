//! Curve25519: X25519 key exchange (RFC 7748) and Ed25519 signatures (RFC 8032).
//! A port of TweetNaCl's field and group arithmetic (16 limbs of 16 bits in i64).

use crate::sha512::sha512;

type Gf = [i64; 16];

const GF0: Gf = [0; 16];
const GF1: Gf = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const C121665: Gf = [0xDB41, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const D: Gf = [0x78a3, 0x1359, 0x4dca, 0x75eb, 0xd8ab, 0x4141, 0x0a4d, 0x0070, 0xe898, 0x7779, 0x4079, 0x8cc7, 0xfe73, 0x2b6f, 0x6cee, 0x5203];
const D2: Gf = [0xf159, 0x26b2, 0x9b94, 0xebd6, 0xb156, 0x8283, 0x149a, 0x00e0, 0xd130, 0xeef3, 0x80f2, 0x198e, 0xfce7, 0x56df, 0xd9dc, 0x2406];
const X: Gf = [0xd51a, 0x8f25, 0x2d60, 0xc956, 0xa7b2, 0x9525, 0xc760, 0x692c, 0xdc5c, 0xfdd6, 0xe231, 0xc0a4, 0x53fe, 0xcd6e, 0x36d3, 0x2169];
const Y: Gf = [0x6658, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666];
const I: Gf = [0xa0b0, 0x4a0e, 0x1b27, 0xc4ee, 0xe478, 0xad2f, 0x1806, 0x2f43, 0xd7a7, 0x3dfb, 0x0099, 0x2b4d, 0xdf0b, 0x4fc1, 0x2480, 0x2b83];

fn car(o: &mut Gf) {
    for i in 0..16 {
        o[i] += 1 << 16;
        let c = o[i] >> 16;
        if i < 15 {
            o[i + 1] += c - 1;
        } else {
            o[0] += 38 * (c - 1);
        }
        o[i] -= c << 16;
    }
}

fn sel(p: &mut Gf, q: &mut Gf, b: i64) {
    let c = !(b - 1);
    for i in 0..16 {
        let t = c & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

fn pack(n: &Gf) -> [u8; 32] {
    let mut t = *n;
    car(&mut t);
    car(&mut t);
    car(&mut t);
    let mut m = GF0;
    for _ in 0..2 {
        m[0] = t[0] - 0xffed;
        for i in 1..15 {
            m[i] = t[i] - 0xffff - ((m[i - 1] >> 16) & 1);
            m[i - 1] &= 0xffff;
        }
        m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
        let b = (m[15] >> 16) & 1;
        m[14] &= 0xffff;
        sel(&mut t, &mut m, 1 - b);
    }
    let mut o = [0u8; 32];
    for i in 0..16 {
        o[2 * i] = (t[i] & 0xff) as u8;
        o[2 * i + 1] = (t[i] >> 8) as u8;
    }
    o
}

fn unpack(n: &[u8; 32]) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = i64::from(n[2 * i]) + (i64::from(n[2 * i + 1]) << 8);
    }
    o[15] &= 0x7fff;
    o
}

fn neq(a: &Gf, b: &Gf) -> bool {
    pack(a) != pack(b)
}

fn par(a: &Gf) -> u8 {
    pack(a)[0] & 1
}

fn add(a: &Gf, b: &Gf) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

fn sub(a: &Gf, b: &Gf) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

fn mul(a: &Gf, b: &Gf) -> Gf {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += a[i] * b[j];
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    let mut o = GF0;
    o.copy_from_slice(&t[..16]);
    car(&mut o);
    car(&mut o);
    o
}

fn sq(a: &Gf) -> Gf {
    mul(a, a)
}

fn inv(i: &Gf) -> Gf {
    let mut c = *i;
    for a in (0..=253).rev() {
        c = sq(&c);
        if a != 2 && a != 4 {
            c = mul(&c, i);
        }
    }
    c
}

fn pow2523(i: &Gf) -> Gf {
    let mut c = *i;
    for a in (0..=250).rev() {
        c = sq(&c);
        if a != 1 {
            c = mul(&c, i);
        }
    }
    c
}

// ---------------------------------------------------------------------------------------
// X25519

pub fn x25519(scalar: &[u8; 32], point: &[u8; 32]) -> [u8; 32] {
    let mut z = *scalar;
    z[31] = (z[31] & 127) | 64;
    z[0] &= 248;
    let x = unpack(point);
    let mut a = GF1;
    let mut b = x;
    let mut c = GF0;
    let mut d = GF1;
    for i in (0..=254).rev() {
        let r = i64::from((z[i >> 3] >> (i & 7)) & 1);
        sel(&mut a, &mut b, r);
        sel(&mut c, &mut d, r);
        let mut e = add(&a, &c);
        a = sub(&a, &c);
        c = add(&b, &d);
        b = sub(&b, &d);
        d = sq(&e);
        let f = sq(&a);
        a = mul(&c, &a);
        c = mul(&b, &e);
        e = add(&a, &c);
        a = sub(&a, &c);
        b = sq(&a);
        c = sub(&d, &f);
        a = mul(&c, &C121665);
        a = add(&a, &d);
        c = mul(&c, &a);
        a = mul(&d, &f);
        d = mul(&b, &x);
        b = sq(&e);
        sel(&mut a, &mut b, r);
        sel(&mut c, &mut d, r);
    }
    let ci = inv(&c);
    pack(&mul(&a, &ci))
}

pub const X25519_BASE: [u8; 32] = {
    let mut b = [0u8; 32];
    b[0] = 9;
    b
};

pub fn x25519_public(secret: &[u8; 32]) -> [u8; 32] {
    x25519(secret, &X25519_BASE)
}

// ---------------------------------------------------------------------------------------
// Ed25519

type Point = [Gf; 4];

fn padd(p: &mut Point, q: &Point) {
    let a = mul(&sub(&p[1], &p[0]), &sub(&q[1], &q[0]));
    let b = mul(&add(&p[0], &p[1]), &add(&q[0], &q[1]));
    let c = mul(&mul(&p[3], &q[3]), &D2);
    let d0 = mul(&p[2], &q[2]);
    let d = add(&d0, &d0);
    let e = sub(&b, &a);
    let f = sub(&d, &c);
    let g = add(&d, &c);
    let h = add(&b, &a);
    p[0] = mul(&e, &f);
    p[1] = mul(&h, &g);
    p[2] = mul(&g, &f);
    p[3] = mul(&e, &h);
}

fn cswap(p: &mut Point, q: &mut Point, b: i64) {
    for i in 0..4 {
        sel(&mut p[i], &mut q[i], b);
    }
}

fn ppack(p: &Point) -> [u8; 32] {
    let zi = inv(&p[2]);
    let tx = mul(&p[0], &zi);
    let ty = mul(&p[1], &zi);
    let mut r = pack(&ty);
    r[31] ^= par(&tx) << 7;
    r
}

fn scalarmult(q: &Point, s: &[u8; 32]) -> Point {
    let mut p: Point = [GF0, GF1, GF1, GF0];
    let mut q = *q;
    for i in (0..=255).rev() {
        let b = i64::from((s[i / 8] >> (i & 7)) & 1);
        cswap(&mut p, &mut q, b);
        let pc = p;
        padd(&mut q, &pc);
        let pc = p;
        padd(&mut p, &pc);
        cswap(&mut p, &mut q, b);
    }
    p
}

fn scalarbase(s: &[u8; 32]) -> Point {
    let base: Point = [X, Y, GF1, mul(&X, &Y)];
    scalarmult(&base, s)
}

const L: [i64; 32] = [0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10];

fn mod_l(x: &mut [i64; 64]) -> [u8; 32] {
    for i in (32..64).rev() {
        let mut carry = 0;
        let mut j = i - 32;
        while j < i - 12 {
            x[j] += carry - 16 * x[i] * L[j - (i - 32)];
            carry = (x[j] + 128) >> 8;
            x[j] -= carry << 8;
            j += 1;
        }
        x[j] += carry;
        x[i] = 0;
    }
    let mut carry = 0;
    for j in 0..32 {
        x[j] += carry - (x[31] >> 4) * L[j];
        carry = x[j] >> 8;
        x[j] &= 255;
    }
    for j in 0..32 {
        x[j] -= carry * L[j];
    }
    let mut r = [0u8; 32];
    for i in 0..32 {
        x[i + 1] += x[i] >> 8;
        r[i] = (x[i] & 255) as u8;
    }
    r
}

fn reduce(h: &[u8; 64]) -> [u8; 32] {
    let mut x = [0i64; 64];
    for i in 0..64 {
        x[i] = i64::from(h[i]);
    }
    mod_l(&mut x)
}

fn clamp(seed: &[u8; 32]) -> [u8; 64] {
    let mut d = sha512(&[seed]);
    d[0] &= 248;
    d[31] &= 127;
    d[31] |= 64;
    d
}

/// The public key for a 32-byte secret seed.
pub fn ed25519_public(seed: &[u8; 32]) -> [u8; 32] {
    let d = clamp(seed);
    ppack(&scalarbase(d[..32].try_into().unwrap()))
}

pub fn ed25519_sign(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let d = clamp(seed);
    let pk = ppack(&scalarbase(d[..32].try_into().unwrap()));
    let r = reduce(&sha512(&[&d[32..], msg]));
    let rr = ppack(&scalarbase(&r));
    let h = reduce(&sha512(&[&rr, &pk, msg]));
    let mut x = [0i64; 64];
    for i in 0..32 {
        x[i] = i64::from(r[i]);
    }
    for i in 0..32 {
        for j in 0..32 {
            x[i + j] += i64::from(h[i]) * i64::from(d[j]);
        }
    }
    let s = mod_l(&mut x);
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&rr);
    sig[32..].copy_from_slice(&s);
    sig
}

fn unpackneg(p: &[u8; 32]) -> Option<Point> {
    let mut r: Point = [GF0, GF0, GF1, GF0];
    r[1] = unpack(p);
    let num0 = sq(&r[1]);
    let den0 = mul(&num0, &D);
    let num = sub(&num0, &r[2]);
    let den = add(&r[2], &den0);
    let den2 = sq(&den);
    let den4 = sq(&den2);
    let den6 = mul(&den4, &den2);
    let mut t = mul(&den6, &num);
    t = mul(&t, &den);
    t = pow2523(&t);
    t = mul(&t, &num);
    t = mul(&t, &den);
    t = mul(&t, &den);
    r[0] = mul(&t, &den);
    let mut chk = mul(&sq(&r[0]), &den);
    if neq(&chk, &num) {
        r[0] = mul(&r[0], &I);
    }
    chk = mul(&sq(&r[0]), &den);
    if neq(&chk, &num) {
        return None;
    }
    if par(&r[0]) == (p[31] >> 7) {
        r[0] = sub(&GF0, &r[0]);
    }
    r[3] = mul(&r[0], &r[1]);
    Some(r)
}

/// Is S < L? (A signature with a larger S is malleable and refused.)
fn scalar_canonical(s: &[u8]) -> bool {
    for i in (0..32).rev() {
        let l = L[i] as u8;
        if s[i] < l {
            return true;
        }
        if s[i] > l {
            return false;
        }
    }
    false
}

pub fn ed25519_verify(pk: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    if !scalar_canonical(&sig[32..]) {
        return false;
    }
    let Some(q) = unpackneg(pk) else { return false };
    let h = reduce(&sha512(&[&sig[..32], pk, msg]));
    let mut p = scalarmult(&q, &h);
    let s: [u8; 32] = sig[32..].try_into().unwrap();
    let q2 = scalarbase(&s);
    padd(&mut p, &q2);
    crate::ct_eq(&ppack(&p), &sig[..32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hex, unhex};

    fn arr<const N: usize>(h: &str) -> [u8; N] {
        unhex(h).unwrap().try_into().unwrap()
    }

    #[test]
    fn x25519_rfc7748() {
        let k: [u8; 32] = arr("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        let u: [u8; 32] = arr("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        assert_eq!(hex(&x25519(&k, &u)), "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552");
        // Diffie-Hellman between Alice and Bob (RFC 7748 section 6.1)
        let a: [u8; 32] = arr("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let b: [u8; 32] = arr("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        assert_eq!(hex(&x25519_public(&a)), "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        assert_eq!(hex(&x25519(&a, &x25519_public(&b))), "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(x25519(&a, &x25519_public(&b)), x25519(&b, &x25519_public(&a)));
    }

    #[test]
    fn ed25519_rfc8032() {
        // Test 1: empty message
        let seed: [u8; 32] = arr("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pk = ed25519_public(&seed);
        assert_eq!(hex(&pk), "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let sig = ed25519_sign(&seed, b"");
        assert_eq!(hex(&sig), "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b");
        assert!(ed25519_verify(&pk, b"", &sig));
        // Test 2: one byte
        let seed: [u8; 32] = arr("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb");
        let pk = ed25519_public(&seed);
        assert_eq!(hex(&pk), "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c");
        let sig = ed25519_sign(&seed, &[0x72]);
        assert_eq!(hex(&sig), "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00");
        assert!(ed25519_verify(&pk, &[0x72], &sig));
        // Tampering is caught.
        assert!(!ed25519_verify(&pk, &[0x73], &sig));
        let mut bad = sig;
        bad[10] ^= 1;
        assert!(!ed25519_verify(&pk, &[0x72], &bad));
        let mut bad = sig;
        bad[63] |= 0xF0; // S >= L
        assert!(!ed25519_verify(&pk, &[0x72], &bad));
        let other = ed25519_public(&[7; 32]);
        assert!(!ed25519_verify(&other, &[0x72], &sig));
    }
}
