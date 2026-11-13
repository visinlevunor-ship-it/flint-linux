//! libm: the functions glibc programs call beyond flint-libc's sin/cos/exp/log/pow/atan/atan2.
//!
//! Exact operations (rounding, fmod, remainder, frexp, scalbn, sqrt, fma) are computed exactly.
//! The transcendental ones not in flint-libc use the x87 unit, which works with a 64-bit
//! significand, so after rounding to double they are within about one ulp. The float (f)
//! versions compute in double and round once. The fenv functions keep the x87 control word
//! and MXCSR in step, as glibc's do.

use core::arch::{asm, global_asm};
use core::ffi::c_void;

type c_int = i32;
type c_long = i64;

// --- bits --------------------------------------------------------------------------------------

#[inline]
fn fabs_(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1 << 63))
}
#[inline]
fn copysign_(x: f64, s: f64) -> f64 {
    f64::from_bits((x.to_bits() & !(1 << 63)) | (s.to_bits() & (1 << 63)))
}
#[inline]
fn pow2(k: i32) -> f64 {
    f64::from_bits(((0x3ff + k) as u64) << 52)
}

fn trunc_(x: f64) -> f64 {
    let e = ((x.to_bits() >> 52) & 0x7ff) as i32 - 0x3ff;
    if e >= 52 {
        return x;
    }
    if e < 0 {
        return copysign_(0.0, x);
    }
    f64::from_bits(x.to_bits() & !((1u64 << (52 - e)) - 1))
}

fn scalbn_(mut x: f64, mut k: i32) -> f64 {
    if k > 1023 {
        x *= pow2(1023);
        k -= 1023;
        if k > 1023 {
            x *= pow2(1023);
            k -= 1023;
            k = k.min(1023);
        }
    } else if k < -1022 {
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

// --- x87 helpers ---------------------------------------------------------------------------------

/// Run x87 code on one double in memory at [p] (loaded first is up to the code).
macro_rules! x87 {
    ($v:ident, $($code:literal),*) => {
        // SAFETY: the code uses only the x87 stack and ax, and leaves the stack empty.
        unsafe {
            asm!($($code,)* p = in(reg) &mut $v as *mut f64,
                out("ax") _, out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack));
        }
    };
    ($v:ident, $w:ident, $($code:literal),*) => {
        // SAFETY: as above.
        unsafe {
            asm!($($code,)* p = in(reg) &mut $v as *mut f64, q = in(reg) &$w as *const f64,
                out("ax") _, out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack));
        }
    };
}

fn log2_(x: f64) -> f64 {
    log2_raw(x)
}
fn log2_raw(x: f64) -> f64 {
    let mut v = x;
    x87!(v, "fld1", "fld qword ptr [{p}]", "fyl2x", "fstp qword ptr [{p}]");
    v
}
fn log10_(x: f64) -> f64 {
    pos_nan(log10_raw(x), x)
}
fn log10_raw(x: f64) -> f64 {
    let mut v = x;
    x87!(v, "fldlg2", "fld qword ptr [{p}]", "fyl2x", "fstp qword ptr [{p}]");
    v
}
fn log1p_(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x < -1.0 {
        return NNAN;
    }
    let mut v = x;
    if fabs_(x) < 0.29 {
        x87!(v, "fldln2", "fld qword ptr [{p}]", "fyl2xp1", "fstp qword ptr [{p}]");
    } else {
        x87!(v, "fldln2", "fld qword ptr [{p}]", "fld1", "faddp st(1), st", "fyl2x", "fstp qword ptr [{p}]");
    }
    v
}
/// 2^x for finite x in a range that cannot overflow the x87 format.
fn exp2_x87(x: f64) -> f64 {
    let mut v = x;
    x87!(
        v,
        "fld qword ptr [{p}]",
        "fld st(0)",
        "frndint",
        "fsub st(1), st",
        "fxch st(1)",
        "f2xm1",
        "fld1",
        "faddp st(1), st",
        "fscale",
        "fstp st(1)",
        "fstp qword ptr [{p}]"
    );
    v
}
fn exp2_(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 1100.0 {
        return f64::INFINITY;
    }
    if x < -1200.0 {
        return 0.0;
    }
    exp2_x87(x)
}
fn expm1_(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 710.0 {
        return f64::INFINITY;
    }
    if x < -60.0 {
        return -1.0;
    }
    let mut v = x;
    if fabs_(x) < 0.69 {
        // |x log2 e| < 1: f2xm1 directly, which keeps small results exact
        x87!(v, "fld qword ptr [{p}]", "fldl2e", "fmulp st(1), st", "f2xm1", "fstp qword ptr [{p}]");
    } else {
        x87!(
            v,
            "fld qword ptr [{p}]",
            "fldl2e",
            "fmulp st(1), st",
            "fld st(0)",
            "frndint",
            "fsub st(1), st",
            "fxch st(1)",
            "f2xm1",
            "fld1",
            "faddp st(1), st",
            "fscale",
            "fstp st(1)",
            "fld1",
            "fsubp st(1), st",
            "fstp qword ptr [{p}]"
        );
    }
    v
}
fn asin_(x: f64) -> f64 {
    pos_nan(asin_raw(x), x)
}
fn asin_raw(x: f64) -> f64 {
    let mut v = x;
    x87!(
        v,
        "fld qword ptr [{p}]",
        "fld1",
        "fsub st, st(1)",
        "fld1",
        "fadd st, st(2)",
        "fmulp st(1), st",
        "fsqrt",
        "fpatan",
        "fstp qword ptr [{p}]"
    );
    v
}
fn acos_(x: f64) -> f64 {
    pos_nan(acos_raw(x), x)
}
fn acos_raw(x: f64) -> f64 {
    let mut v = x;
    x87!(
        v,
        "fld qword ptr [{p}]",
        "fld1",
        "fsub st, st(1)",
        "fld1",
        "fadd st, st(2)",
        "fmulp st(1), st",
        "fsqrt",
        "fxch st(1)",
        "fpatan",
        "fstp qword ptr [{p}]"
    );
    v
}
fn hypot_(x: f64, y: f64) -> f64 {
    if x.is_infinite() || y.is_infinite() {
        return f64::INFINITY;
    }
    let mut v = x;
    x87!(
        v,
        y,
        "fld qword ptr [{p}]",
        "fmul st, st(0)",
        "fld qword ptr [{q}]",
        "fmul st, st(0)",
        "faddp st(1), st",
        "fsqrt",
        "fstp qword ptr [{p}]"
    );
    v
}
fn fmod_(x: f64, y: f64) -> f64 {
    let mut v = x;
    x87!(
        v,
        y,
        "fld qword ptr [{q}]",
        "fld qword ptr [{p}]",
        "2:",
        "fprem",
        "fnstsw ax",
        "test ah, 4",
        "jnz 2b",
        "fstp st(1)",
        "fstp qword ptr [{p}]"
    );
    v
}
fn remainder_(x: f64, y: f64) -> f64 {
    let mut v = x;
    x87!(
        v,
        y,
        "fld qword ptr [{q}]",
        "fld qword ptr [{p}]",
        "2:",
        "fprem1",
        "fnstsw ax",
        "test ah, 4",
        "jnz 2b",
        "fstp st(1)",
        "fstp qword ptr [{p}]"
    );
    v
}

fn sqrt_(x: f64) -> f64 {
    let r: f64;
    // SAFETY: plain SSE2 instruction.
    unsafe { asm!("sqrtsd {r}, {x}", r = lateout(xmm_reg) r, x = in(xmm_reg) x, options(pure, nomem, nostack)) };
    r
}
fn sqrtf_(x: f32) -> f32 {
    let r: f32;
    // SAFETY: plain SSE instruction.
    unsafe { asm!("sqrtss {r}, {x}", r = lateout(xmm_reg) r, x = in(xmm_reg) x, options(pure, nomem, nostack)) };
    r
}

use flint_libc::math as fm;

/// The NaN x86 produces for invalid operations (0/0, sqrt(-1)): sign bit set. glibc returns
/// it from most functions (log, pow, sin(inf)), but a positive one from asin and acos.
const NNAN: f64 = f64::from_bits(0xfff8_0000_0000_0000);

/// x87 results for invalid operations are the negative default NaN; asin/acos return +NaN.
#[inline]
fn pos_nan(r: f64, x: f64) -> f64 {
    if r.is_nan() && !x.is_nan() { f64::NAN } else { r }
}

fn log_(x: f64) -> f64 {
    fm::log(x)
}

// --- rounding --------------------------------------------------------------------------------

/// Round to an integer in the current rounding mode (adding 2^52 rounds by MXCSR).
fn rint_(x: f64) -> f64 {
    if fabs_(x) >= 4503599627370496.0 || x.is_nan() {
        return x;
    }
    let t = copysign_(4503599627370496.0, x);
    let r = core::hint::black_box(x + t) - t;
    copysign_(r, x)
}
fn floor_(x: f64) -> f64 {
    let t = trunc_(x);
    if t > x { t - 1.0 } else { t }
}
fn ceil_(x: f64) -> f64 {
    let t = trunc_(x);
    if t < x { t + 1.0 } else { t }
}
fn round_(x: f64) -> f64 {
    let t = trunc_(x);
    if fabs_(x - t) >= 0.5 { t + copysign_(1.0, x) } else { t }
}

#[no_mangle]
pub extern "C" fn trunc(x: f64) -> f64 {
    trunc_(x)
}
#[no_mangle]
pub extern "C" fn truncf(x: f32) -> f32 {
    trunc_(x as f64) as f32
}
#[no_mangle]
pub extern "C" fn floor(x: f64) -> f64 {
    floor_(x)
}
#[no_mangle]
pub extern "C" fn floorf(x: f32) -> f32 {
    floor_(x as f64) as f32
}
#[no_mangle]
pub extern "C" fn ceil(x: f64) -> f64 {
    ceil_(x)
}
#[no_mangle]
pub extern "C" fn ceilf(x: f32) -> f32 {
    ceil_(x as f64) as f32
}
#[no_mangle]
pub extern "C" fn round(x: f64) -> f64 {
    round_(x)
}
#[no_mangle]
pub extern "C" fn roundf(x: f32) -> f32 {
    round_(x as f64) as f32
}
#[no_mangle]
pub extern "C" fn rint(x: f64) -> f64 {
    rint_(x)
}
#[no_mangle]
pub extern "C" fn rintf(x: f32) -> f32 {
    // every float is exact in double, and rounding to an integer is exact again
    rint_(x as f64) as f32
}
#[no_mangle]
pub extern "C" fn nearbyint(x: f64) -> f64 {
    rint_(x)
}
#[no_mangle]
pub extern "C" fn nearbyintf(x: f32) -> f32 {
    rint_(x as f64) as f32
}
#[no_mangle]
pub extern "C" fn lrint(x: f64) -> c_long {
    rint_(x) as c_long
}
#[no_mangle]
pub extern "C" fn lrintf(x: f32) -> c_long {
    rint_(x as f64) as c_long
}
#[no_mangle]
pub extern "C" fn llrint(x: f64) -> i64 {
    rint_(x) as i64
}
#[no_mangle]
pub extern "C" fn llrintf(x: f32) -> i64 {
    rint_(x as f64) as i64
}
#[no_mangle]
pub extern "C" fn lround(x: f64) -> c_long {
    round_(x) as c_long
}
#[no_mangle]
pub extern "C" fn lroundf(x: f32) -> c_long {
    round_(x as f64) as c_long
}
#[no_mangle]
pub extern "C" fn llround(x: f64) -> i64 {
    round_(x) as i64
}
#[no_mangle]
pub extern "C" fn llroundf(x: f32) -> i64 {
    round_(x as f64) as i64
}

// --- exact operations ----------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn fabs(x: f64) -> f64 {
    fabs_(x)
}
#[no_mangle]
pub extern "C" fn fabsf(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & 0x7fff_ffff)
}
#[no_mangle]
pub extern "C" fn copysign(x: f64, y: f64) -> f64 {
    copysign_(x, y)
}
#[no_mangle]
pub extern "C" fn copysignf(x: f32, y: f32) -> f32 {
    f32::from_bits((x.to_bits() & 0x7fff_ffff) | (y.to_bits() & 0x8000_0000))
}
#[no_mangle]
pub extern "C" fn fmin(x: f64, y: f64) -> f64 {
    if x.is_nan() {
        y
    } else if y.is_nan() {
        x
    } else if x < y {
        x
    } else {
        y
    }
}
#[no_mangle]
pub extern "C" fn fmax(x: f64, y: f64) -> f64 {
    if x.is_nan() {
        y
    } else if y.is_nan() {
        x
    } else if x > y {
        x
    } else {
        y
    }
}
#[no_mangle]
pub extern "C" fn fminf(x: f32, y: f32) -> f32 {
    fmin(x as f64, y as f64) as f32
}
#[no_mangle]
pub extern "C" fn fmaxf(x: f32, y: f32) -> f32 {
    fmax(x as f64, y as f64) as f32
}
#[no_mangle]
pub extern "C" fn fdim(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        x + y
    } else if x > y {
        x - y
    } else {
        0.0
    }
}
#[no_mangle]
pub extern "C" fn sqrt(x: f64) -> f64 {
    sqrt_(x)
}
#[no_mangle]
pub extern "C" fn sqrtf(x: f32) -> f32 {
    sqrtf_(x)
}
#[no_mangle]
pub extern "C" fn fmod(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    fmod_(x, y)
}
#[no_mangle]
pub extern "C" fn fmodf(x: f32, y: f32) -> f32 {
    fmod_(x as f64, y as f64) as f32
}
#[no_mangle]
pub extern "C" fn remainder(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    remainder_(x, y)
}
#[no_mangle]
pub extern "C" fn remainderf(x: f32, y: f32) -> f32 {
    remainder_(x as f64, y as f64) as f32
}
#[no_mangle]
pub extern "C" fn drem(x: f64, y: f64) -> f64 {
    remainder_(x, y)
}
#[no_mangle]
pub unsafe extern "C" fn modf(x: f64, ip: *mut f64) -> f64 {
    let i = trunc_(x);
    *ip = i;
    if x.is_infinite() {
        return copysign_(0.0, x);
    }
    copysign_(x - i, x)
}
#[no_mangle]
pub unsafe extern "C" fn modff(x: f32, ip: *mut f32) -> f32 {
    let mut i = 0.0;
    let r = modf(x as f64, &mut i);
    *ip = i as f32;
    r as f32
}
#[no_mangle]
pub unsafe extern "C" fn frexp(x: f64, e: *mut c_int) -> f64 {
    let b = x.to_bits();
    let ex = ((b >> 52) & 0x7ff) as i32;
    if ex == 0 {
        if x == 0.0 {
            *e = 0;
            return x;
        }
        // subnormal: scale into the normal range first
        let r = frexp(x * pow2(64), e);
        *e -= 64;
        return r;
    }
    if ex == 0x7ff {
        *e = 0;
        return x;
    }
    *e = ex - 0x3fe;
    f64::from_bits((b & !(0x7ffu64 << 52)) | (0x3feu64 << 52))
}
#[no_mangle]
pub unsafe extern "C" fn frexpf(x: f32, e: *mut c_int) -> f32 {
    frexp(x as f64, e) as f32
}
#[no_mangle]
pub extern "C" fn scalbn(x: f64, k: c_int) -> f64 {
    scalbn_(x, k)
}
#[no_mangle]
pub extern "C" fn scalbnf(x: f32, k: c_int) -> f32 {
    // exact in double over float's whole range, then one rounding
    scalbn_(x as f64, k.clamp(-400, 400)) as f32
}
#[no_mangle]
pub extern "C" fn scalbln(x: f64, k: c_long) -> f64 {
    scalbn_(x, k.clamp(-100000, 100000) as i32)
}
#[no_mangle]
pub extern "C" fn ldexp(x: f64, k: c_int) -> f64 {
    scalbn_(x, k)
}
#[no_mangle]
pub extern "C" fn ldexpf(x: f32, k: c_int) -> f32 {
    scalbnf(x, k)
}
#[no_mangle]
pub extern "C" fn ilogb(x: f64) -> c_int {
    if x == 0.0 {
        return i32::MIN;
    }
    if x.is_nan() {
        return i32::MIN;
    }
    if x.is_infinite() {
        return i32::MAX;
    }
    let mut e = 0;
    // SAFETY: e is a local.
    unsafe { frexp(x, &mut e) };
    e - 1
}
#[no_mangle]
pub extern "C" fn logb(x: f64) -> f64 {
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if !x.is_finite() {
        return x * x;
    }
    ilogb(x) as f64
}

fn nextafter_(x: f64, dir: i32) -> f64 {
    // dir: -1 toward smaller, 1 toward larger
    if x.is_nan() || dir == 0 {
        return x;
    }
    if x == 0.0 {
        return copysign_(f64::from_bits(1), dir as f64);
    }
    let b = x.to_bits();
    let away = (x > 0.0) == (dir > 0);
    f64::from_bits(if away { b + 1 } else { b - 1 })
}
#[no_mangle]
pub extern "C" fn nextafter(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == y {
        return y;
    }
    nextafter_(x, if y > x { 1 } else { -1 })
}
#[no_mangle]
pub extern "C" fn nextafterf(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == y {
        return y;
    }
    if x == 0.0 {
        return f32::from_bits(1 | (y.to_bits() & 0x8000_0000));
    }
    let b = x.to_bits();
    let away = (x > 0.0) == (y > x);
    f32::from_bits(if away { b + 1 } else { b - 1 })
}

/// nexttoward's second argument is a long double, passed on the stack: compare it with x on
/// the x87 unit and hand the direction to Rust.
#[no_mangle]
unsafe extern "C" fn __flint_nexttoward(x: f64, dir: c_int) -> f64 {
    nextafter_(x, dir)
}
#[no_mangle]
unsafe extern "C" fn __flint_nexttowardf(x: f32, dir: c_int) -> f32 {
    if x.is_nan() || dir == 0 {
        return x;
    }
    nextafterf(x, if dir > 0 { f32::INFINITY } else { f32::NEG_INFINITY })
}
global_asm!(
    ".globl nexttoward",
    ".type nexttoward,@function",
    "nexttoward:",
    "movsd qword ptr [rsp - 8], xmm0",
    "fld tbyte ptr [rsp + 8]",
    "fld qword ptr [rsp - 8]",
    // st0 = x, st1 = y
    "fucomip st, st(1)",
    "fstp st(0)",
    "mov edi, 0",
    "jp 1f",          // unordered: dir 0 (x or y NaN; x NaN returns x)
    "je 3f",
    "mov edi, 1",     // x < y
    "jb 2f",
    "mov edi, -1",
    "2:",
    "jmp {f}",
    "1:",
    // y is NaN when x is not: return y as a double
    "fld tbyte ptr [rsp + 8]",
    "fstp qword ptr [rsp - 8]",
    "movsd xmm0, qword ptr [rsp - 8]",
    "ret",
    "3:",
    // equal: return y converted
    "ret",
    ".size nexttoward, . - nexttoward",
    ".globl nexttowardf",
    ".type nexttowardf,@function",
    "nexttowardf:",
    "movss dword ptr [rsp - 8], xmm0",
    "fld tbyte ptr [rsp + 8]",
    "fld dword ptr [rsp - 8]",
    "fucomip st, st(1)",
    "fstp st(0)",
    "mov edi, 0",
    "jp 5f",
    "je 6f",
    "mov edi, 1",
    "jb 4f",
    "mov edi, -1",
    "4:",
    "jmp {g}",
    "5:",
    "fld tbyte ptr [rsp + 8]",
    "fstp dword ptr [rsp - 8]",
    "movss xmm0, dword ptr [rsp - 8]",
    "6:",
    "ret",
    ".size nexttowardf, . - nexttowardf",
    f = sym __flint_nexttoward,
    g = sym __flint_nexttowardf,
);

// --- fma -------------------------------------------------------------------------------------------

fn has_fma() -> bool {
    static mut STATE: u8 = 0;
    // SAFETY: a benign race; every thread computes the same value.
    unsafe {
        if STATE == 0 {
            let r = core::arch::x86_64::__cpuid(1);
            let osxsave = r.ecx & (1 << 27) != 0;
            let avx = r.ecx & (1 << 28) != 0;
            let fma = r.ecx & (1 << 12) != 0;
            let mut ok = fma && avx && osxsave;
            if ok {
                let xcr0 = core::arch::x86_64::_xgetbv(0);
                ok = xcr0 & 6 == 6;
            }
            STATE = if ok { 2 } else { 1 };
        }
        STATE == 2
    }
}

fn decompose(x: f64) -> (bool, u64, i32) {
    let b = x.to_bits();
    let e = ((b >> 52) & 0x7ff) as i32;
    let f = b & ((1 << 52) - 1);
    if e == 0 { (b >> 63 != 0, f, -1074) } else { (b >> 63 != 0, f | (1 << 52), e - 1075) }
}

/// Exactly rounded a*b + c with integer arithmetic.
fn fma_soft(a: f64, b: f64, c: f64) -> f64 {
    if !a.is_finite() || !b.is_finite() || !c.is_finite() || a == 0.0 || b == 0.0 {
        return a * b + c;
    }
    if c == 0.0 {
        return a * b + c; // one rounding
    }
    let (sa, ma, ea) = decompose(a);
    let (sb, mb, eb) = decompose(b);
    let (sc, mc, ec) = decompose(c);
    let bits = |v: u128| 128 - v.leading_zeros() as i32;
    let mut pm = ma as u128 * mb as u128;
    let mut pe = ea + eb;
    let ps = sa != sb;
    let sh = 125 - bits(pm);
    pm <<= sh;
    pe -= sh;
    let mut cm = mc as u128;
    let mut ce = ec;
    let sh = 125 - bits(cm);
    cm <<= sh;
    ce -= sh;
    // X has the larger magnitude
    let (mut xm, xe, xs, mut ym, ye, ys) =
        if pe > ce || (pe == ce && pm >= cm) { (pm, pe, ps, cm, ce, sc) } else { (cm, ce, sc, pm, pe, ps) };
    let d = xe - ye;
    if d > 0 {
        if d >= 127 {
            ym = 1; // only the sticky bit is left
        } else {
            let lost = ym & ((1u128 << d) - 1) != 0;
            ym = (ym >> d) | lost as u128;
        }
    }
    let r = if xs == ys { xm + ym } else { xm - ym };
    xm = r;
    if xm == 0 {
        return 0.0;
    }
    let n = bits(xm);
    let msb_exp = xe + n - 1;
    let keep = if msb_exp < -1022 { 53 - (-1022 - msb_exp) } else { 53 };
    let shift = n - keep.max(0);
    let mut mant: u128;
    let mut exp = xe;
    if shift > 0 {
        if shift >= 128 {
            mant = 0;
            // all bits below the half: round to zero (the value is below the smallest subnormal/2)
            exp += shift;
        } else {
            mant = xm >> shift;
            let rem = xm & ((1u128 << shift) - 1);
            let half = 1u128 << (shift - 1);
            if rem > half || (rem == half && mant & 1 == 1) {
                mant += 1;
            }
            exp += shift;
        }
    } else {
        mant = xm;
    }
    let v = scalbn_(mant as u64 as f64, exp);
    if xs { -v } else { v }
}

#[no_mangle]
pub extern "C" fn fma(a: f64, b: f64, c: f64) -> f64 {
    if has_fma() {
        let mut r = a;
        // SAFETY: the CPU and OS support FMA3 (checked above).
        unsafe { asm!("vfmadd213sd {r}, {b}, {c}", r = inout(xmm_reg) r, b = in(xmm_reg) b, c = in(xmm_reg) c, options(pure, nomem, nostack)) };
        r
    } else {
        fma_soft(a, b, c)
    }
}
#[no_mangle]
pub extern "C" fn fmaf(a: f32, b: f32, c: f32) -> f32 {
    // the product of two floats is exact in double; the sum is rounded twice, which differs
    // from one rounding only in rare halfway cases
    fma(a as f64, b as f64, c as f64) as f32
}

// --- transcendental --------------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn log2(x: f64) -> f64 {
    log2_(x)
}
#[no_mangle]
pub extern "C" fn log10(x: f64) -> f64 {
    log10_(x)
}
#[no_mangle]
pub extern "C" fn log1p(x: f64) -> f64 {
    log1p_(x)
}
#[no_mangle]
pub extern "C" fn exp2(x: f64) -> f64 {
    exp2_(x)
}
#[no_mangle]
pub extern "C" fn exp10(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 330.0 {
        return f64::INFINITY;
    }
    if x < -360.0 {
        return 0.0;
    }
    let mut v = x;
    x87!(
        v,
        "fld qword ptr [{p}]",
        "fldl2t",
        "fmulp st(1), st",
        "fld st(0)",
        "frndint",
        "fsub st(1), st",
        "fxch st(1)",
        "f2xm1",
        "fld1",
        "faddp st(1), st",
        "fscale",
        "fstp st(1)",
        "fstp qword ptr [{p}]"
    );
    v
}
#[no_mangle]
pub extern "C" fn expm1(x: f64) -> f64 {
    expm1_(x)
}
#[no_mangle]
pub extern "C" fn asin(x: f64) -> f64 {
    asin_(x)
}
#[no_mangle]
pub extern "C" fn acos(x: f64) -> f64 {
    acos_(x)
}
#[no_mangle]
pub extern "C" fn tan(x: f64) -> f64 {
    tan_(x)
}
#[no_mangle]
pub extern "C" fn hypot(x: f64, y: f64) -> f64 {
    hypot_(x, y)
}
#[no_mangle]
pub unsafe extern "C" fn sincos(x: f64, s: *mut f64, c: *mut f64) {
    *s = sin(x);
    *c = cos(x);
}

/// (e^a + s e^-a) / 2 for 0 <= a < 712, in extended precision (s = 1 for cosh, -1 for sinh).
fn cosh_sinh(a: f64, s: f64) -> f64 {
    let mut v = a;
    x87!(
        v,
        s,
        "fld qword ptr [{p}]",
        "fldl2e",
        "fmulp st(1), st",
        "fld st(0)",
        "frndint",
        "fsub st(1), st",
        "fxch st(1)",
        "f2xm1",
        "fld1",
        "faddp st(1), st",
        "fscale",
        "fstp st(1)",
        // st0 = E
        "fld1",
        "fdiv st, st(1)",
        "fmul qword ptr [{q}]",
        "faddp st(1), st",
        "fld1",
        "fadd st, st(0)",
        "fdivp st(1), st",
        "fstp qword ptr [{p}]"
    );
    v
}
#[no_mangle]
pub extern "C" fn sinh(x: f64) -> f64 {
    let a = fabs_(x);
    if !a.is_finite() {
        return x;
    }
    if a < 3.7e-9 {
        return x;
    }
    let r = if a < 1.0 {
        // cancellation: through expm1
        let t = expm1_(a);
        0.5 * (2.0 * t - t * t / (t + 1.0))
    } else if a > 712.0 {
        f64::INFINITY
    } else {
        cosh_sinh(a, -1.0)
    };
    copysign_(r, x)
}
#[no_mangle]
pub extern "C" fn cosh(x: f64) -> f64 {
    let a = fabs_(x);
    if a.is_nan() {
        return x;
    }
    if a > 712.0 {
        return f64::INFINITY;
    }
    if a < 0.34657359027997264 {
        let t = expm1_(a);
        let w = 1.0 + t;
        return 1.0 + (t * t) / (w + w);
    }
    cosh_sinh(a, 1.0)
}
#[no_mangle]
pub extern "C" fn tanh(x: f64) -> f64 {
    let a = fabs_(x);
    if a.is_nan() {
        return x;
    }
    let r = if a >= 22.0 {
        1.0
    } else if a < 2.8e-17 {
        a
    } else if a >= 1.0 {
        1.0 - 2.0 / (expm1_(2.0 * a) + 2.0)
    } else {
        let t = expm1_(-2.0 * a);
        -t / (t + 2.0)
    };
    copysign_(r, x)
}
#[no_mangle]
pub extern "C" fn asinh(x: f64) -> f64 {
    let a = fabs_(x);
    if !a.is_finite() {
        return x + x;
    }
    let r = if a < 3.7e-9 {
        a
    } else if a > 2.68e8 {
        log_(a) + core::f64::consts::LN_2
    } else if a > 2.0 {
        log_(2.0 * a + 1.0 / (sqrt_(a * a + 1.0) + a))
    } else {
        let t = a * a;
        log1p_(a + t / (1.0 + sqrt_(1.0 + t)))
    };
    copysign_(r, x)
}
#[no_mangle]
pub extern "C" fn acosh(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x < 1.0 {
        return NNAN;
    }
    if x >= 2.68e8 {
        return if x.is_infinite() { x } else { log_(x) + core::f64::consts::LN_2 };
    }
    if x > 2.0 {
        return log_(2.0 * x - 1.0 / (x + sqrt_(x * x - 1.0)));
    }
    let t = x - 1.0;
    log1p_(t + sqrt_(2.0 * t + t * t))
}
#[no_mangle]
pub extern "C" fn atanh(x: f64) -> f64 {
    let a = fabs_(x);
    if a.is_nan() {
        return x;
    }
    if a > 1.0 {
        return NNAN;
    }
    if a == 1.0 {
        return copysign_(f64::INFINITY, x);
    }
    let r = if a < 3.7e-9 {
        a
    } else if a < 0.5 {
        let t = a + a;
        0.5 * log1p_(t + t * a / (1.0 - a))
    } else {
        0.5 * log1p_((a + a) / (1.0 - a))
    };
    copysign_(r, x)
}
#[no_mangle]
pub extern "C" fn cbrt(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    // x = m 2^(3k) with m in [1/8, 1): cbrt(x) = cbrt(m) 2^k
    let mut e = 0;
    // SAFETY: e is a local.
    let mut m = unsafe { frexp(fabs_(x), &mut e) };
    let r = e.rem_euclid(3);
    let k = (e - r) / 3;
    if r != 0 {
        m = scalbn_(m, r - 3);
    }
    let k = k + if r != 0 { 1 } else { 0 };
    // estimate on the x87 unit, then a Newton step with the residual computed exactly
    let mut y = exp2_x87(log2_raw(m) / 3.0);
    let y2 = y * y;
    let e2 = fma(y, y, -y2);
    let y3 = y2 * y;
    let e3 = fma(y2, y, -y3) + e2 * y;
    let resid = (m - y3) - e3;
    y += resid / (3.0 * y2);
    copysign_(scalbn_(y, k), x)
}

// --- trigonometric argument reduction ------------------------------------------------------------

/// 2/pi in binary, 64 bits a word, most significant first.
const TWO_OVER_PI: [u64; 20] = [
    0xa2f9836e4e441529, 0xfc2757d1f534ddc0, 0xdb6295993c439041, 0xfe5163abdebbc561,
    0xb7246e3a424dd2e0, 0x06492eea09d1921c, 0xfe1deb1cb129a73e, 0xe88235f52ebb4484,
    0xe99c7026b45f7e41, 0x3991d639835339f4, 0x9c845f8bbdf9283b, 0x1ff897ffde05980f,
    0xef2f118b5a0a6d1f, 0x6d367ecf27cb09b7, 0x4f463f669e5fea2d, 0x7527bac7ebe5f17b,
    0x3d0739f78a5292ea, 0x6bfb5fb11f8d5d08, 0x56033046fc7b6bab, 0xf0cfbc209af4361d,
];

/// 64 bits of 2/pi starting at fraction bit `i` (bit 1 is the first after the point).
fn tpi_bits(i: usize) -> u64 {
    let b = i - 1;
    let w = b / 64;
    let s = b % 64;
    let hi = TWO_OVER_PI.get(w).copied().unwrap_or(0);
    if s == 0 {
        return hi;
    }
    let lo = TWO_OVER_PI.get(w + 1).copied().unwrap_or(0);
    (hi << s) | (lo >> (64 - s))
}

/// Payne-Hanek: x = (4n + q) pi/2 + r with |r| <= pi/4, r as a double-double (hi, lo).
fn rem_pio2(x: f64) -> (u32, f64, f64) {
    let (neg, m, e) = decompose(x);
    // x * 2/pi = m 2^e * sum b_i 2^-i; terms with i <= e - 2 are multiples of 4
    let i0 = if e - 1 >= 1 { (e - 1) as usize } else { 1 };
    let w = [tpi_bits(i0), tpi_bits(i0 + 64), tpi_bits(i0 + 128)];
    // P = m * W, 256 bits, as four limbs (least significant first)
    let mut p = [0u64; 4];
    let mut carry: u128 = 0;
    for k in 0..3 {
        let t = m as u128 * w[2 - k] as u128 + carry;
        p[k] = t as u64;
        carry = t >> 64;
    }
    p[3] = carry as u64;
    // the binary point sits s bits above the bottom of P
    let s = (i0 as i32 + 191 - e) as usize;
    let bit = |n: usize| -> u64 { if n >= 256 { 0 } else { (p[n / 64] >> (n % 64)) & 1 } };
    let mut q = (bit(s) | bit(s + 1) << 1) as u32;
    // 128 fraction bits below the point
    let mut f: u128 = 0;
    for k in 0..128 {
        let n = s as isize - 1 - k as isize;
        f = (f << 1) | if n >= 0 { bit(n as usize) as u128 } else { 0 };
    }
    // fold into [-1/2, 1/2)
    let mut fneg = false;
    if f >> 127 == 1 {
        f = f.wrapping_neg();
        fneg = true;
        q = (q + 1) & 3;
    }
    if f == 0 {
        return (q, 0.0, 0.0);
    }
    // r = f pi/2 with f = f / 2^128
    let lz = f.leading_zeros();
    let fn_ = f << lz;
    let fhi = (fn_ >> 75) as u64 as f64; // top 53 bits
    let flo = ((fn_ >> 11) & ((1 << 64) - 1)) as u64 as f64 * pow2(-64);
    let sc = -128 - lz as i32 + 75;
    let fh = scalbn_(fhi, sc);
    let fl = scalbn_(flo, sc);
    const PIO2_HI: f64 = 1.5707963267948966;
    const PIO2_LO: f64 = 6.123233995736766e-17;
    let rh = fh * PIO2_HI;
    let rl = fma(fh, PIO2_HI, -rh) + fh * PIO2_LO + fl * PIO2_HI;
    let (mut h, mut l) = (rh + rl, rl - ((rh + rl) - rh));
    if fneg {
        h = -h;
        l = -l;
    }
    if neg {
        h = -h;
        l = -l;
        q = (4 - q) & 3;
    }
    (q, h, l)
}

fn sin_dd(h: f64, l: f64) -> f64 {
    fm::sin(h) + l * fm::cos(h)
}
fn cos_dd(h: f64, l: f64) -> f64 {
    fm::cos(h) - l * fm::sin(h)
}
/// tan of a reduced argument (|h| <= pi/4) on the x87 unit.
fn tan_small(x: f64) -> f64 {
    let mut v = x;
    x87!(v, "fld qword ptr [{p}]", "fptan", "fstp st(0)", "fstp qword ptr [{p}]");
    v
}

const BIG: f64 = 1.0e6;

#[no_mangle]
pub extern "C" fn sin(x: f64) -> f64 {
    if !x.is_finite() {
        return if x.is_nan() { x } else { NNAN };
    }
    if fabs_(x) < BIG {
        return fm::sin(x);
    }
    let (q, h, l) = rem_pio2(x);
    match q {
        0 => sin_dd(h, l),
        1 => cos_dd(h, l),
        2 => -sin_dd(h, l),
        _ => -cos_dd(h, l),
    }
}
#[no_mangle]
pub extern "C" fn cos(x: f64) -> f64 {
    if !x.is_finite() {
        return if x.is_nan() { x } else { NNAN };
    }
    if fabs_(x) < BIG {
        return fm::cos(x);
    }
    let (q, h, l) = rem_pio2(x);
    match q {
        0 => cos_dd(h, l),
        1 => -sin_dd(h, l),
        2 => -cos_dd(h, l),
        _ => sin_dd(h, l),
    }
}
fn tan_(x: f64) -> f64 {
    if !x.is_finite() {
        return if x.is_nan() { x } else { NNAN };
    }
    if fabs_(x) <= core::f64::consts::FRAC_PI_4 {
        return tan_small(x);
    }
    let (q, h, l) = rem_pio2(x);
    if q & 1 == 0 {
        let t = tan_small(h);
        // d tan = (1 + tan^2) dh
        t + l * (1.0 + t * t)
    } else {
        // -cot h, divided in extended precision; d(-cot) = (1 + cot^2) dh
        let mut c = h;
        x87!(c, "fld qword ptr [{p}]", "fptan", "fdivrp st(1), st", "fchs", "fstp qword ptr [{p}]");
        c + l * (1.0 + c * c)
    }
}
#[no_mangle]
pub extern "C" fn exp(x: f64) -> f64 {
    fm::exp(x)
}
#[no_mangle]
pub extern "C" fn log(x: f64) -> f64 {
    if x < 0.0 {
        return NNAN;
    }
    fm::log(x)
}
#[no_mangle]
pub extern "C" fn atan(x: f64) -> f64 {
    fm::atan(x)
}
#[no_mangle]
pub extern "C" fn atan2(y: f64, x: f64) -> f64 {
    fm::atan2(y, x)
}

/// x^y for x > 0 finite and y finite: 2^(y log2 x) with the product in extended precision.
fn pow_pos(x: f64, y: f64) -> f64 {
    let mut v = x;
    x87!(
        v,
        y,
        "fld qword ptr [{q}]",
        "fld qword ptr [{p}]",
        "fyl2x",
        // |y log2 x| beyond the double range: let fscale saturate, clamped first
        "fld st(0)",
        "frndint",
        "fsub st(1), st",
        "fxch st(1)",
        "f2xm1",
        "fld1",
        "faddp st(1), st",
        "fscale",
        "fstp st(1)",
        "fstp qword ptr [{p}]"
    );
    v
}

fn is_int(y: f64) -> bool {
    trunc_(y) == y
}
fn is_odd_int(y: f64) -> bool {
    if fabs_(y) >= 9007199254740992.0 || !is_int(y) {
        return false;
    }
    (y as i64) & 1 == 1
}

#[no_mangle]
pub extern "C" fn pow(x: f64, y: f64) -> f64 {
    // C99 Annex F special cases
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let ax = fabs_(x);
    if y.is_infinite() {
        if ax == 1.0 {
            return 1.0;
        }
        return if (ax > 1.0) == (y > 0.0) { f64::INFINITY } else { 0.0 };
    }
    if x == 0.0 {
        let odd = is_odd_int(y);
        return if y < 0.0 {
            if odd { copysign_(f64::INFINITY, x) } else { f64::INFINITY }
        } else if odd {
            x
        } else {
            0.0
        };
    }
    if x.is_infinite() {
        let odd = is_odd_int(y);
        let r = if y < 0.0 { 0.0 } else { f64::INFINITY };
        return if x < 0.0 && odd { -r } else { r };
    }
    let mut neg = false;
    if x < 0.0 {
        if !is_int(y) {
            return NNAN;
        }
        neg = is_odd_int(y);
    }
    // exact small integer powers by squaring when the result stays exact
    let r = if fabs_(y) > 1.0e5 {
        // keep the extended exponent from wrapping: the result is 0 or inf anyway
        let l = log2_raw(ax);
        if l * y > 16000.0 {
            f64::INFINITY
        } else if l * y < -16000.0 {
            0.0
        } else {
            pow_pos(ax, y)
        }
    } else {
        pow_pos(ax, y)
    };
    if neg { -r } else { r }
}

// float versions: compute in double, round once
macro_rules! via_double {
    ($($f:ident => $d:expr),* $(,)?) => {
        $(
            #[no_mangle]
            pub extern "C" fn $f(x: f32) -> f32 {
                ($d)(x as f64) as f32
            }
        )*
    };
}
via_double! {
    sinf => sin,
    cosf => cos,
    tanf => tan_,
    asinf => asin_,
    acosf => acos_,
    atanf => fm::atan,
    expf => exp,
    exp2f => exp2_,
    expm1f => expm1_,
    exp10f => exp10,
    logf => log,
    log2f => log2_,
    log10f => log10_,
    log1pf => log1p_,
    sinhf => sinh,
    coshf => cosh,
    tanhf => tanh,
    asinhf => asinh,
    acoshf => acosh,
    atanhf => atanh,
    cbrtf => cbrt,
}
#[no_mangle]
pub extern "C" fn powf(x: f32, y: f32) -> f32 {
    pow(x as f64, y as f64) as f32
}
#[no_mangle]
pub extern "C" fn atan2f(y: f32, x: f32) -> f32 {
    fm::atan2(y as f64, x as f64) as f32
}
#[no_mangle]
pub extern "C" fn hypotf(x: f32, y: f32) -> f32 {
    hypot_(x as f64, y as f64) as f32
}
#[no_mangle]
pub unsafe extern "C" fn sincosf(x: f32, s: *mut f32, c: *mut f32) {
    *s = sin(x as f64) as f32;
    *c = cos(x as f64) as f32;
}

#[no_mangle]
pub extern "C" fn nan(_tag: *const u8) -> f64 {
    f64::NAN
}
#[no_mangle]
pub extern "C" fn nanf(_tag: *const u8) -> f32 {
    f32::NAN
}

// --- long double (x87 extended, passed on the stack and returned in st0) ----------------------

#[no_mangle]
unsafe extern "C" fn __flint_frexpl(e: *mut c_int, x: *mut u8) {
    let m = (x as *mut u64).read_unaligned();
    let se = (x.add(8) as *mut u16).read_unaligned();
    let ex = (se & 0x7fff) as i32;
    if ex == 0x7fff || m == 0 {
        *e = 0;
        return;
    }
    let (m, ex) = if ex == 0 || m >> 63 == 0 {
        // denormal (or pseudo-denormal): normalise
        let s = m.leading_zeros() as i32;
        (m << s, ex.max(1) - s)
    } else {
        (m, ex)
    };
    *e = ex - 0x3ffe;
    (x as *mut u64).write_unaligned(m);
    (x.add(8) as *mut u16).write_unaligned((se & 0x8000) | 0x3ffe);
}

global_asm!(
    ".globl ceill",
    ".type ceill,@function",
    "ceill:",
    "fld tbyte ptr [rsp + 8]",
    "fnstcw word ptr [rsp - 4]",
    "movzx eax, word ptr [rsp - 4]",
    "and eax, 0xf3ff",
    "or eax, 0x0800",
    "mov word ptr [rsp - 8], ax",
    "fldcw word ptr [rsp - 8]",
    "frndint",
    "fldcw word ptr [rsp - 4]",
    "ret",
    ".size ceill, . - ceill",
    ".globl floorl",
    ".type floorl,@function",
    "floorl:",
    "fld tbyte ptr [rsp + 8]",
    "fnstcw word ptr [rsp - 4]",
    "movzx eax, word ptr [rsp - 4]",
    "and eax, 0xf3ff",
    "or eax, 0x0400",
    "mov word ptr [rsp - 8], ax",
    "fldcw word ptr [rsp - 8]",
    "frndint",
    "fldcw word ptr [rsp - 4]",
    "ret",
    ".size floorl, . - floorl",
    ".globl truncl",
    ".type truncl,@function",
    "truncl:",
    "fld tbyte ptr [rsp + 8]",
    "fnstcw word ptr [rsp - 4]",
    "movzx eax, word ptr [rsp - 4]",
    "or eax, 0x0c00",
    "mov word ptr [rsp - 8], ax",
    "fldcw word ptr [rsp - 8]",
    "frndint",
    "fldcw word ptr [rsp - 4]",
    "ret",
    ".size truncl, . - truncl",
    ".globl rintl",
    ".type rintl,@function",
    "rintl:",
    "fld tbyte ptr [rsp + 8]",
    "frndint",
    "ret",
    ".size rintl, . - rintl",
    ".globl fabsl",
    ".type fabsl,@function",
    "fabsl:",
    "fld tbyte ptr [rsp + 8]",
    "fabs",
    "ret",
    ".size fabsl, . - fabsl",
    ".globl sqrtl",
    ".type sqrtl,@function",
    "sqrtl:",
    "fld tbyte ptr [rsp + 8]",
    "fsqrt",
    "ret",
    ".size sqrtl, . - sqrtl",
    ".globl ldexpl",
    ".type ldexpl,@function",
    "ldexpl:",
    ".globl scalbnl",
    ".type scalbnl,@function",
    "scalbnl:",
    "mov dword ptr [rsp - 4], edi",
    "fild dword ptr [rsp - 4]",
    "fld tbyte ptr [rsp + 8]",
    "fscale",
    "fstp st(1)",
    "ret",
    ".size ldexpl, . - ldexpl",
    ".globl frexpl",
    ".type frexpl,@function",
    "frexpl:",
    "push rbx",
    "lea rbx, [rsp + 16]",
    "mov rsi, rbx",
    "call {frexp}",
    "fld tbyte ptr [rbx]",
    "pop rbx",
    "ret",
    ".size frexpl, . - frexpl",
    ".globl fmodl",
    ".type fmodl,@function",
    "fmodl:",
    "fld tbyte ptr [rsp + 24]",
    "fld tbyte ptr [rsp + 8]",
    "2:",
    "fprem",
    "fnstsw ax",
    "test ah, 4",
    "jnz 2b",
    "fstp st(1)",
    "ret",
    ".size fmodl, . - fmodl",
    frexp = sym __flint_frexpl,
);

// --- floating-point environment ------------------------------------------------------------------

const FE_ALL_EXCEPT: c_int = 0x3f;

fn get_mxcsr() -> u32 {
    let mut m: u32 = 0;
    // SAFETY: stores MXCSR to a local.
    unsafe { asm!("stmxcsr [{p}]", p = in(reg) &mut m, options(nostack)) };
    m
}
fn set_mxcsr(m: u32) {
    // SAFETY: loads MXCSR from a local.
    unsafe { asm!("ldmxcsr [{p}]", p = in(reg) &m, options(nostack)) };
}
fn get_cw() -> u16 {
    let mut c: u16 = 0;
    // SAFETY: stores the x87 control word to a local.
    unsafe { asm!("fnstcw [{p}]", p = in(reg) &mut c, options(nostack)) };
    c
}
fn set_cw(c: u16) {
    // SAFETY: loads the x87 control word from a local.
    unsafe { asm!("fldcw [{p}]", p = in(reg) &c, options(nostack)) };
}
fn get_sw() -> u16 {
    let s: u16;
    // SAFETY: reads the x87 status word.
    unsafe { asm!("fnstsw ax", out("ax") s, options(nomem, nostack)) };
    s
}

#[no_mangle]
pub extern "C" fn fegetround() -> c_int {
    (get_cw() & 0xc00) as c_int
}
#[no_mangle]
pub extern "C" fn fesetround(r: c_int) -> c_int {
    if r & !0xc00 != 0 {
        return 1;
    }
    set_cw((get_cw() & !0xc00) | r as u16);
    set_mxcsr((get_mxcsr() & !0x6000) | ((r as u32) << 3));
    0
}
#[no_mangle]
pub extern "C" fn fetestexcept(e: c_int) -> c_int {
    ((get_sw() as u32 | get_mxcsr()) & (e & FE_ALL_EXCEPT) as u32) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn feclearexcept(e: c_int) -> c_int {
    let e = (e & FE_ALL_EXCEPT) as u32;
    let mut env = [0u8; 28];
    asm!("fnstenv [{p}]", p = in(reg) env.as_mut_ptr(), options(nostack));
    let sw = (env.as_ptr().add(4) as *const u16).read_unaligned() & !(e as u16);
    (env.as_mut_ptr().add(4) as *mut u16).write_unaligned(sw);
    asm!("fldenv [{p}]", p = in(reg) env.as_ptr(), options(nostack));
    set_mxcsr(get_mxcsr() & !e);
    0
}
#[no_mangle]
pub unsafe extern "C" fn feraiseexcept(e: c_int) -> c_int {
    set_mxcsr(get_mxcsr() | (e & FE_ALL_EXCEPT) as u32);
    0
}
#[no_mangle]
pub unsafe extern "C" fn fegetenv(env: *mut c_void) -> c_int {
    // fnstenv masks all exceptions as a side effect: reload it
    asm!("fnstenv [{p}]", "fldenv [{p}]", p = in(reg) env, options(nostack));
    (env as *mut u8).add(28).cast::<u32>().write_unaligned(get_mxcsr());
    0
}
#[no_mangle]
pub unsafe extern "C" fn fesetenv(env: *const c_void) -> c_int {
    let e = env as isize;
    if e == -1 || e == -2 {
        // FE_DFL_ENV / FE_NOMASK_ENV
        let mut cur = [0u8; 28];
        asm!("fnstenv [{p}]", p = in(reg) cur.as_mut_ptr(), options(nostack));
        let cw: u16 = if e == -1 { 0x37f } else { 0x340 };
        (cur.as_mut_ptr() as *mut u16).write_unaligned(cw);
        (cur.as_mut_ptr().add(4) as *mut u16).write_unaligned(0);
        asm!("fldenv [{p}]", p = in(reg) cur.as_ptr(), options(nostack));
        set_mxcsr(if e == -1 { 0x1f80 } else { 0 });
        return 0;
    }
    asm!("fldenv [{p}]", p = in(reg) env, options(nostack));
    set_mxcsr((env as *const u8).add(28).cast::<u32>().read_unaligned());
    0
}
#[no_mangle]
pub unsafe extern "C" fn feholdexcept(env: *mut c_void) -> c_int {
    fegetenv(env);
    feclearexcept(FE_ALL_EXCEPT);
    set_cw(get_cw() | 0x3f);
    set_mxcsr(get_mxcsr() | 0x1f80);
    0
}
#[no_mangle]
pub unsafe extern "C" fn feupdateenv(env: *const c_void) -> c_int {
    let raised = fetestexcept(FE_ALL_EXCEPT);
    fesetenv(env);
    feraiseexcept(raised);
    0
}
#[no_mangle]
pub unsafe extern "C" fn fegetexceptflag(f: *mut u16, e: c_int) -> c_int {
    *f = fetestexcept(e) as u16;
    0
}
#[no_mangle]
pub unsafe extern "C" fn fesetexceptflag(f: *const u16, e: c_int) -> c_int {
    feclearexcept(e);
    feraiseexcept(*f as c_int & e);
    0
}
#[no_mangle]
pub extern "C" fn feenableexcept(e: c_int) -> c_int {
    let old = !(get_cw() as c_int) & FE_ALL_EXCEPT;
    let e = (e & FE_ALL_EXCEPT) as u16;
    set_cw(get_cw() & !e);
    set_mxcsr(get_mxcsr() & !((e as u32) << 7));
    old
}
#[no_mangle]
pub extern "C" fn fedisableexcept(e: c_int) -> c_int {
    let old = !(get_cw() as c_int) & FE_ALL_EXCEPT;
    let e = (e & FE_ALL_EXCEPT) as u16;
    set_cw(get_cw() | e);
    set_mxcsr(get_mxcsr() | ((e as u32) << 7));
    old
}
#[no_mangle]
pub extern "C" fn fegetexcept() -> c_int {
    !(get_cw() as c_int) & FE_ALL_EXCEPT
}
