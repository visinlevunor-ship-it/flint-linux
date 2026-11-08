//! The memory functions the compiler emits calls to. x86_64 `rep movsb` /
//! `rep stosb` are fast on modern CPUs (ERMS/FSRM) and cannot recurse.

use core::arch::asm;

#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    asm!("rep movsb", inout("rcx") n => _, inout("rdi") dst => _, inout("rsi") src => _, options(nostack, preserves_flags));
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
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
            inout("rdi") dst.add(n - 1) => _,
            inout("rsi") src.add(n - 1) => _,
            options(nostack),
        );
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memset(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    asm!("rep stosb", inout("rcx") n => _, inout("rdi") s => _, in("al") c as u8, options(nostack, preserves_flags));
    s
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
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
pub unsafe extern "C" fn bcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    memcmp(a, b, n)
}
