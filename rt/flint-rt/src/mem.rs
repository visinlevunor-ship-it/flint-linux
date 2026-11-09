//! The memory functions the compiler emits calls to. x86_64 `rep movsb` /
//! `rep stosb` are fast on modern CPUs (ERMS/FSRM) and cannot recurse.
//! Signatures use `c_void` like the C prototypes, which newer rustc checks for.

use core::arch::asm;
use core::ffi::c_void;

#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    asm!("rep movsb", inout("rcx") n => _, inout("rdi") dst => _, inout("rsi") src => _, options(nostack, preserves_flags));
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    if (dst as usize).wrapping_sub(src as usize) >= n {
        // dst is below src, or the ranges do not overlap: copying forward is safe.
        asm!("rep movsb", inout("rcx") n => _, inout("rdi") dst => _, inout("rsi") src => _, options(nostack, preserves_flags));
    } else if n != 0 {
        // Overlapping with dst above src: copy backward.
        asm!(
            "std",
            "rep movsb",
            "cld",
            inout("rcx") n => _,
            inout("rdi") (dst as *mut u8).add(n - 1) => _,
            inout("rsi") (src as *const u8).add(n - 1) => _,
            options(nostack),
        );
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memset(s: *mut c_void, c: i32, n: usize) -> *mut c_void {
    asm!("rep stosb", inout("rcx") n => _, inout("rdi") s => _, in("al") c as u8, options(nostack, preserves_flags));
    s
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    let (a, b) = (a as *const u8, b as *const u8);
    let mut i = 0;
    while i < n {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y {
            return i32::from(x) - i32::from(y);
        }
        i += 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    memcmp(a, b, n)
}
