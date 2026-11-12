//! sin, cos, exp, log, pow, atan2 for f64, after Sun's fdlibm (the reference algorithms
//! most C libraries' versions descend from): argument reduction, then minimax polynomials.
//!
//! sin and cos reduce their argument with a three-part pi/2 (Cody-Waite), which is exact
//! for |x| up to about 2^20 * pi/2; beyond that the result loses accuracy (no Payne-Hanek
//! reduction, which needs a table of 2/pi's bits). pow with an integer exponent up to 2^31
//! multiplies by squaring (exact for small powers like 2^10); otherwise it is
//! exp(y * log(x)), within a few ulp.

#![allow(clippy::excessive_precision, clippy::approx_constant)]

fn hi(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}

// What core lacks for f64 on Rust 1.75 (they would otherwise call libm, which this is).
fn fabs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1 << 63))
}

fn copysign(x: f64, s: f64) -> f64 {
    f64::from_bits((x.to_bits() & !(1 << 63)) | (s.to_bits() & (1 << 63)))
}

/// Round toward zero.
fn trunc(x: f64) -> f64 {
    let e = ((x.to_bits() >> 52) & 0x7ff) as i32 - 0x3ff;
    if e >= 52 {
        return x; // already an integer (or inf/nan)
    }
    if e < 0 {
        return copysign(0.0, x);
    }
    f64::from_bits(x.to_bits() & !((1u64 << (52 - e)) - 1))
}

/// Round half away from zero.
fn round(x: f64) -> f64 {
    let t = trunc(x);
    if fabs(x - t) >= 0.5 {
        t + copysign(1.0, x)
    } else {
        t
    }
}

const LN2_HI: f64 = 6.93147180369123816490e-01;
const LN2_LO: f64 = 1.90821492927058770002e-10;
const INV_LN2: f64 = 1.44269504088896338700e+00;

/// 2^k as an f64 (k in the normal range).
fn pow2(k: i32) -> f64 {
    f64::from_bits(((0x3ff + k) as u64) << 52)
}

/// x * 2^k, through subnormal results.
fn scalbn(mut x: f64, mut k: i32) -> f64 {
    if k > 1023 {
        x *= pow2(1023);
        k -= 1023;
        if k > 1023 {
            x *= pow2(1023);
            k -= 1023;
            k = k.min(1023);
        }
    } else if k < -1022 {
        // multiply in steps that keep 53 bits until the last one
        x *= pow2(-1022) * pow2(53);
        k += 1022 - 53;
        if k < -1022 {
            x *= pow2(-1022) * pow2(53);
            k += 1022 - 53;
            k = k.max(-1022);
        }
    }
    x * pow2(k)
}

pub fn exp(x: f64) -> f64 {
    const P1: f64 = 1.66666666666666019037e-01;
    const P2: f64 = -2.77777777770155933842e-03;
    const P3: f64 = 6.61375632143793436117e-05;
    const P4: f64 = -1.65339022054652515390e-06;
    const P5: f64 = 4.13813679705723846039e-08;
    if x.is_nan() {
        return x;
    }
    if x > 709.782712893383973096 {
        return f64::INFINITY;
    }
    if x < -745.13321910194110842 {
        return 0.0;
    }
    if fabs(x) < 3.7252902984e-09 {
        return 1.0 + x;
    }
    // x = k*ln2 + r, |r| <= ln2/2
    let k = (x * INV_LN2 + if x < 0.0 { -0.5 } else { 0.5 }) as i32;
    let kf = k as f64;
    let hi_ = x - kf * LN2_HI;
    let lo = kf * LN2_LO;
    let r = hi_ - lo;
    let z = r * r;
    let c = r - z * (P1 + z * (P2 + z * (P3 + z * (P4 + z * P5))));
    let y = 1.0 - ((lo - (r * c) / (2.0 - c)) - hi_);
    if k == 0 {
        y
    } else {
        scalbn(y, k)
    }
}

pub fn log(x: f64) -> f64 {
    const LG1: f64 = 6.666666666666735130e-01;
    const LG2: f64 = 3.999999999940941908e-01;
    const LG3: f64 = 2.857142874366239149e-01;
    const LG4: f64 = 2.222219843214978396e-01;
    const LG5: f64 = 1.818357216161805012e-01;
    const LG6: f64 = 1.531383769920937332e-01;
    const LG7: f64 = 1.479819860511658591e-01;
    if x.is_nan() {
        return x;
    }
    if x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x.is_infinite() {
        return x;
    }
    let mut x = x;
    let mut k: i32 = 0;
    if x < f64::MIN_POSITIVE {
        // subnormal: scale up
        x *= pow2(54);
        k -= 54;
    }
    let bits = x.to_bits();
    let mut hx = (bits >> 32) as u32;
    // reduce x into [sqrt(2)/2, sqrt(2)]
    hx = hx.wrapping_add(0x3ff0_0000 - 0x3fe6_a09e);
    k += (hx >> 20) as i32 - 0x3ff;
    hx = (hx & 0x000f_ffff) + 0x3fe6_a09e;
    let x = f64::from_bits(((hx as u64) << 32) | (bits & 0xffff_ffff));
    let f = x - 1.0;
    let hfsq = 0.5 * f * f;
    let s = f / (2.0 + f);
    let z = s * s;
    let w = z * z;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    let r = t2 + t1;
    let dk = k as f64;
    s * (hfsq + r) + dk * LN2_LO - hfsq + f + dk * LN2_HI
}

pub fn pow(x: f64, y: f64) -> f64 {
    // the special cases C99 Annex F lists, in its order of precedence
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    let y_int = trunc(y) == y;
    let y_odd = y_int && fabs(y) < 9007199254740992.0 && (fabs(y) as u64) & 1 == 1;
    if x == -1.0 && y.is_infinite() {
        return 1.0;
    }
    if y.is_infinite() {
        let ax = fabs(x);
        return if (ax < 1.0) == (y < 0.0) { f64::INFINITY } else { 0.0 };
    }
    if x == 0.0 {
        return if y < 0.0 {
            if y_odd { copysign(f64::INFINITY, x) } else { f64::INFINITY }
        } else if y_odd {
            x
        } else {
            0.0
        };
    }
    if x.is_infinite() {
        if x > 0.0 {
            return if y < 0.0 { 0.0 } else { f64::INFINITY };
        }
        let r = if y < 0.0 { 0.0 } else { f64::INFINITY };
        return if y_odd { -r } else { r };
    }
    if x < 0.0 && !y_int {
        return f64::NAN;
    }
    // integer exponents: exact for small powers
    if y_int && fabs(y) <= 2147483648.0 {
        let mut n = fabs(y) as u64;
        let mut base = x;
        let mut r = 1.0;
        while n > 0 {
            if n & 1 == 1 {
                r *= base;
            }
            base *= base;
            n >>= 1;
        }
        if y < 0.0 {
            // 1/x^n can overflow x^n first though the result is fine: go through exp/log then
            if r.is_infinite() || r == 0.0 {
                let m = exp(y * log(fabs(x)));
                return if y_odd && x < 0.0 { -m } else { m };
            }
            return 1.0 / r;
        }
        return r;
    }
    // y * log(x) in double-double precision, so exp's argument carries ~100 good bits
    let (lh, ll) = log_dd(fabs(x));
    let (ph, pe) = two_prod(y, lh);
    let pl = pe + y * ll;
    let (ph, pl) = fast_two_sum(ph, pl);
    let m = if ph > 709.782712893383973096 {
        f64::INFINITY
    } else if ph < -745.13321910194110842 {
        0.0
    } else {
        let e = exp(ph);
        e + e * pl
    };
    if x < 0.0 && y_odd {
        -m
    } else {
        m
    }
}

// --- double-double helpers (Dekker), for pow ---

fn fast_two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    (s, b - (s - a))
}

fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bb = s - a;
    (s, (a - (s - bb)) + (b - bb))
}

fn split(a: f64) -> (f64, f64) {
    let c = 134217729.0 * a; // 2^27 + 1
    let h = c - (c - a);
    (h, a - h)
}

/// a * b exactly, as hi + lo.
fn two_prod(a: f64, b: f64) -> (f64, f64) {
    let p = a * b;
    let (ah, al) = split(a);
    let (bh, bl) = split(b);
    (p, ((ah * bh - p) + ah * bl + al * bh) + al * bl)
}

/// log(x) for finite x > 0 as hi + lo, using log's reduction and log(1+f) = 2s + s*R.
fn log_dd(x: f64) -> (f64, f64) {
    const LG1: f64 = 6.666666666666735130e-01;
    const LG2: f64 = 3.999999999940941908e-01;
    const LG3: f64 = 2.857142874366239149e-01;
    const LG4: f64 = 2.222219843214978396e-01;
    const LG5: f64 = 1.818357216161805012e-01;
    const LG6: f64 = 1.531383769920937332e-01;
    const LG7: f64 = 1.479819860511658591e-01;
    let mut x = x;
    let mut k: i32 = 0;
    if x < f64::MIN_POSITIVE {
        x *= pow2(54);
        k -= 54;
    }
    let bits = x.to_bits();
    let mut hx = (bits >> 32) as u32;
    hx = hx.wrapping_add(0x3ff0_0000 - 0x3fe6_a09e);
    k += (hx >> 20) as i32 - 0x3ff;
    hx = (hx & 0x000f_ffff) + 0x3fe6_a09e;
    let xr = f64::from_bits(((hx as u64) << 32) | (bits & 0xffff_ffff));
    let f = xr - 1.0; // exact
    // s = f / (2 + f), as s_hi + s_lo
    let (u, ue) = two_sum(2.0, f);
    let s_hi = f / u;
    let (p, pe) = two_prod(s_hi, u);
    let s_lo = (((f - p) - pe) - s_hi * ue) / u;
    let z = s_hi * s_hi;
    let w = z * z;
    let r = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7))) + w * (LG2 + w * (LG4 + w * LG6));
    let dk = k as f64;
    let (h, e) = two_sum(dk * LN2_HI, 2.0 * s_hi);
    let lo = e + 2.0 * s_lo + s_hi * r + dk * LN2_LO;
    fast_two_sum(h, lo)
}

// --- sin, cos ---

const S1: f64 = -1.66666666666666324348e-01;
const S2: f64 = 8.33333333332248946124e-03;
const S3: f64 = -1.98412698298579493134e-04;
const S4: f64 = 2.75573137070700676789e-06;
const S5: f64 = -2.50507602534068634195e-08;
const S6: f64 = 1.58969099521155010221e-10;

/// sin(x + y) for |x| <= pi/4, y the tail of x.
fn k_sin(x: f64, y: f64, iy: bool) -> f64 {
    let z = x * x;
    let w = z * z;
    let r = S2 + z * (S3 + z * S4) + z * w * (S5 + z * S6);
    let v = z * x;
    if !iy {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (0.5 * y - v * r) - y) - v * S1)
    }
}

const C1: f64 = 4.16666666666666019037e-02;
const C2: f64 = -1.38888888888741095749e-03;
const C3: f64 = 2.48015872894767294178e-05;
const C4: f64 = -2.75573143513906633035e-07;
const C5: f64 = 2.08757232129817482790e-09;
const C6: f64 = -1.13596475577881948265e-11;

fn k_cos(x: f64, y: f64) -> f64 {
    let z = x * x;
    let w = z * z;
    let r = z * (C1 + z * (C2 + z * C3)) + w * w * (C4 + z * (C5 + z * C6));
    let hz = 0.5 * z;
    let w = 1.0 - hz;
    w + (((1.0 - w) - hz) + (z * r - x * y))
}

/// x = n*pi/2 + (y0 + y1); returns n mod 4.
fn rem_pio2(x: f64) -> (i32, f64, f64) {
    const INV_PIO2: f64 = 6.36619772367581382433e-01;
    const PIO2_1: f64 = 1.57079632673412561417e+00;
    const PIO2_1T: f64 = 6.07710050650619224932e-11;
    const PIO2_2: f64 = 6.07710050630396597660e-11;
    const PIO2_2T: f64 = 2.02226624879595063154e-21;
    const PIO2_3: f64 = 2.02226624871116645580e-21;
    const PIO2_3T: f64 = 8.47842766036889956997e-32;
    let n = round(x * INV_PIO2);
    let mut r = x - n * PIO2_1;
    let mut w = n * PIO2_1T;
    let mut y0 = r - w;
    let ex = (hi(x) >> 20) & 0x7ff;
    if ex as i32 - ((hi(y0) >> 20) & 0x7ff) as i32 > 16 {
        // cancellation: a second, then a third step
        let t = r;
        w = n * PIO2_2;
        r = t - w;
        w = n * PIO2_2T - ((t - r) - w);
        y0 = r - w;
        if ex as i32 - ((hi(y0) >> 20) & 0x7ff) as i32 > 49 {
            let t = r;
            w = n * PIO2_3;
            r = t - w;
            w = n * PIO2_3T - ((t - r) - w);
            y0 = r - w;
        }
    }
    let y1 = (r - y0) - w;
    ((n as i64 & 3) as i32, y0, y1)
}

pub fn sin(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return f64::NAN;
    }
    if fabs(x) <= 0.7853981633974483 {
        return if fabs(x) < 7.450580596923828e-09 { x } else { k_sin(x, 0.0, false) };
    }
    let (n, y0, y1) = rem_pio2(x);
    match n {
        0 => k_sin(y0, y1, true),
        1 => k_cos(y0, y1),
        2 => -k_sin(y0, y1, true),
        _ => -k_cos(y0, y1),
    }
}

pub fn cos(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return f64::NAN;
    }
    if fabs(x) <= 0.7853981633974483 {
        return if fabs(x) < 7.450580596923828e-09 { 1.0 } else { k_cos(x, 0.0) };
    }
    let (n, y0, y1) = rem_pio2(x);
    match n {
        0 => k_cos(y0, y1),
        1 => -k_sin(y0, y1, true),
        2 => -k_cos(y0, y1),
        _ => k_sin(y0, y1, true),
    }
}

// --- atan, atan2 ---

const ATANHI: [f64; 4] = [4.63647609000806093515e-01, 7.85398163397448278999e-01, 9.82793723247329054082e-01, 1.57079632679489655800e+00];
const ATANLO: [f64; 4] = [2.26987774529616870924e-17, 3.06161699786838301793e-17, 1.39033110312309984516e-17, 6.12323399573676603587e-17];
const AT: [f64; 11] = [
    3.33333333333329318027e-01, -1.99999999998764832476e-01, 1.42857142725034663711e-01,
    -1.11111104054623557880e-01, 9.09088713343650656196e-02, -7.69187620504482999495e-02,
    6.66107313738753120669e-02, -5.83357013379057348645e-02, 4.97687799461593236017e-02,
    -3.65315727442169155270e-02, 1.62858201153657823623e-02,
];

pub fn atan(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let neg = x.is_sign_negative();
    let ix = hi(x) & 0x7fff_ffff;
    if ix >= 0x4410_0000 {
        // |x| >= 2^66
        let z = ATANHI[3] + ATANLO[3];
        return if neg { -z } else { z };
    }
    let (id, x) = if ix < 0x3fdc_0000 {
        // |x| < 0.4375
        if ix < 0x3e40_0000 {
            return x;
        }
        (-1, x)
    } else {
        let a = fabs(x);
        if ix < 0x3ff3_0000 {
            if ix < 0x3fe6_0000 {
                (0, (2.0 * a - 1.0) / (2.0 + a))
            } else {
                (1, (a - 1.0) / (a + 1.0))
            }
        } else if ix < 0x4003_8000 {
            (2, (a - 1.5) / (1.0 + 1.5 * a))
        } else {
            (3, -1.0 / a)
        }
    };
    let z = x * x;
    let w = z * z;
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    if id < 0 {
        return x - x * (s1 + s2);
    }
    let id = id as usize;
    let z = ATANHI[id] - ((x * (s1 + s2) - ATANLO[id]) - x);
    if neg {
        -z
    } else {
        z
    }
}

pub fn atan2(y: f64, x: f64) -> f64 {
    const PI: f64 = 3.1415926535897931160E+00;
    const PI_LO: f64 = 1.2246467991473531772E-16;
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == 1.0 {
        return atan(y);
    }
    let m = (y.is_sign_negative() as u32) | ((x.is_sign_negative() as u32) << 1);
    if y == 0.0 {
        return match m {
            0 | 1 => y,
            2 => PI,
            _ => -PI,
        };
    }
    if x == 0.0 {
        return if m & 1 == 1 { -PI / 2.0 } else { PI / 2.0 };
    }
    if x.is_infinite() {
        if y.is_infinite() {
            return match m {
                0 => PI / 4.0,
                1 => -PI / 4.0,
                2 => 3.0 * PI / 4.0,
                _ => -3.0 * PI / 4.0,
            };
        }
        return match m {
            0 => 0.0,
            1 => -0.0,
            2 => PI,
            _ => -PI,
        };
    }
    if y.is_infinite() {
        return if m & 1 == 1 { -PI / 2.0 } else { PI / 2.0 };
    }
    let ex = ((hi(x) >> 20) & 0x7ff) as i32;
    let ey = ((hi(y) >> 20) & 0x7ff) as i32;
    let z = if ey - ex > 60 {
        // |y/x| > 2^60
        PI / 2.0 + 0.5 * PI_LO
    } else if m & 2 != 0 && ey - ex < -60 {
        0.0
    } else {
        atan(fabs(y / x))
    };
    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distance in units of the last place.
    fn ulps(a: f64, b: f64) -> u64 {
        if a == b || (a.is_nan() && b.is_nan()) {
            return 0;
        }
        let (ia, ib) = (a.to_bits() as i64, b.to_bits() as i64);
        if (ia < 0) != (ib < 0) {
            return u64::MAX;
        }
        ia.abs_diff(ib)
    }

    /// Deterministic inputs spread over many magnitudes.
    fn inputs(lo: f64, hi_: f64, n: usize) -> impl Iterator<Item = f64> {
        let mut s: u64 = 0x9e3779b97f4a7c15;
        (0..n).map(move |_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            lo + (hi_ - lo) * ((s >> 11) as f64 / (1u64 << 53) as f64)
        })
    }

    fn check(name: &str, f: fn(f64) -> f64, g: fn(f64) -> f64, xs: impl Iterator<Item = f64>, max: u64) {
        let mut worst = (0, 0.0);
        for x in xs {
            let d = ulps(f(x), g(x));
            if d > worst.0 {
                worst = (d, x);
            }
        }
        assert!(worst.0 <= max, "{name}: {} ulps at {:e}: got {:e}, want {:e}", worst.0, worst.1, f(worst.1), g(worst.1));
    }

    #[test]
    fn against_the_host_libm() {
        check("exp", exp, f64::exp, inputs(-745.0, 709.0, 200_000), 1);
        check("exp small", exp, f64::exp, inputs(-1.0, 1.0, 100_000), 1);
        check("log", log, f64::ln, inputs(1e-300, 1e300, 200_000), 1);
        check("log near 1", log, f64::ln, inputs(0.5, 2.0, 100_000), 1);
        check("sin", sin, f64::sin, inputs(-1e5, 1e5, 200_000), 1);
        check("sin small", sin, f64::sin, inputs(-4.0, 4.0, 100_000), 1);
        check("cos", cos, f64::cos, inputs(-1e5, 1e5, 200_000), 1);
        check("cos small", cos, f64::cos, inputs(-4.0, 4.0, 100_000), 1);
        check("atan", atan, f64::atan, inputs(-1e3, 1e3, 200_000), 1);
        for (a, b) in inputs(-50.0, 50.0, 20_000).zip(inputs(-50.0, 50.0, 20_000).skip(7)) {
            assert!(ulps(atan2(a, b), a.atan2(b)) <= 2, "atan2({a}, {b})");
        }
        for (a, b) in inputs(0.0, 30.0, 50_000).zip(inputs(-20.0, 20.0, 50_000).skip(3)) {
            let (got, want) = (pow(a, b), a.powf(b));
            assert!(ulps(got, want) <= 2, "pow({a}, {b}) = {got:e}, want {want:e}");
        }
    }

    #[test]
    fn exact_and_special_values() {
        assert_eq!(pow(2.0, 10.0), 1024.0);
        assert_eq!(pow(10.0, 15.0), 1e15);
        assert_eq!(pow(-2.0, 3.0), -8.0);
        assert_eq!(pow(2.0, -2.0), 0.25);
        assert!(pow(-2.0, 0.5).is_nan());
        assert_eq!(pow(0.0, -1.0), f64::INFINITY);
        assert_eq!(pow(f64::NAN, 0.0), 1.0);
        assert_eq!(exp(0.0), 1.0);
        assert_eq!(log(1.0), 0.0);
        assert_eq!(log(0.0), f64::NEG_INFINITY);
        assert!(log(-1.0).is_nan());
        assert_eq!(sin(0.0), 0.0);
        assert_eq!(cos(0.0), 1.0);
        assert_eq!(atan2(0.0, -1.0), core::f64::consts::PI);
        assert_eq!(atan2(1.0, 0.0), core::f64::consts::FRAC_PI_2);
        assert_eq!(exp(1e4), f64::INFINITY);
        assert_eq!(exp(-1e4), 0.0);
        assert!(exp(-740.0) > 0.0, "subnormal results");
    }
}
