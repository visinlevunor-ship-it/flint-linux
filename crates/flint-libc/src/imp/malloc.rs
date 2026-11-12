//! malloc, free, calloc, realloc, posix_memalign.
//!
//! Small blocks (up to 16 KiB) come from 64 KiB spans, one size class per span, with no
//! per-block header: `free` finds a block's span by rounding its address down to 64 KiB.
//! A span that empties is returned to the kernel (one per class is kept for reuse), so a
//! program's heap shrinks when it frees memory. Larger blocks are their own mapping, with a
//! header in the same 64 KiB-aligned position. One lock guards everything.

use crate::imp::{errno, lock::Lock, sys};
use core::ptr;

const SPAN: usize = 64 * 1024;
const HDR: usize = 64; // span header size (keeps blocks 16- and 64-byte aligned)
const MAX_SMALL: usize = 16 * 1024;
const NCLASS: usize = 8 + 4 * 7;
const KIND_SMALL: u32 = 0x5a11_0001;
const KIND_LARGE: u32 = 0x5a11_0002;

#[repr(C)]
struct Span {
    kind: u32,
    class: u32,
    used: u32,
    capacity: u32,
    free: *mut Free,
    bump: usize, // next never-used block, as an offset from the span
    next: *mut Span,
    prev: *mut Span,
    large_map: usize, // for large blocks: the whole mapping
}

struct Free {
    next: *mut Free,
}

struct Class {
    partial: *mut Span, // spans with room
    spare: *mut Span,   // one empty span kept for reuse
}

static LOCK: Lock = Lock::new();
const EMPTY: Class = Class { partial: ptr::null_mut(), spare: ptr::null_mut() };
static mut CLASSES: [Class; NCLASS] = [EMPTY; NCLASS];

/// Size classes: 16..128 in steps of 16, then four steps per doubling up to 16 KiB.
fn class_size(c: usize) -> usize {
    if c < 8 {
        (c + 1) * 16
    } else {
        let k = c - 8; // 0.. : doubling d = k / 4, step s = k % 4
        let base = 128usize << (k / 4);
        base + (base / 4) * (k % 4 + 1)
    }
}

fn class_of(n: usize) -> usize {
    if n <= 128 {
        return if n == 0 { 0 } else { (n - 1) / 16 };
    }
    // find the doubling: 128 < n <= 16384
    let mut base = 128usize;
    let mut d = 0;
    while n > base * 2 {
        base *= 2;
        d += 1;
    }
    let step = base / 4;
    let s = (n - base).div_ceil(step) - 1; // 0..=3
    8 + d * 4 + s
}

unsafe fn map_aligned(size: usize) -> *mut u8 {
    let total = size + SPAN;
    let p = sys::sc6(sys::MMAP, 0, total, 3, 0x22, usize::MAX, 0);
    if p < 0 {
        return ptr::null_mut();
    }
    let p = p as usize;
    let a = (p + SPAN - 1) & !(SPAN - 1);
    if a > p {
        sys::sc2(sys::MUNMAP, p, a - p);
    }
    let end = a + size;
    if p + total > end {
        sys::sc2(sys::MUNMAP, end, p + total - end);
    }
    a as *mut u8
}

unsafe fn unlink(head: *mut *mut Span, s: *mut Span) {
    if !(*s).prev.is_null() {
        (*(*s).prev).next = (*s).next;
    } else if *head == s {
        *head = (*s).next;
    }
    if !(*s).next.is_null() {
        (*(*s).next).prev = (*s).prev;
    }
    (*s).next = ptr::null_mut();
    (*s).prev = ptr::null_mut();
}

unsafe fn push(head: *mut *mut Span, s: *mut Span) {
    (*s).prev = ptr::null_mut();
    (*s).next = *head;
    if !(*head).is_null() {
        (**head).prev = s;
    }
    *head = s;
}

unsafe fn alloc_small(c: usize) -> *mut u8 {
    let cl = &mut (*ptr::addr_of_mut!(CLASSES))[c];
    let mut s = cl.partial;
    if s.is_null() {
        if !cl.spare.is_null() {
            s = cl.spare;
            cl.spare = ptr::null_mut();
        } else {
            let m = map_aligned(SPAN);
            if m.is_null() {
                return ptr::null_mut();
            }
            s = m as *mut Span;
            let size = class_size(c);
            *s = Span {
                kind: KIND_SMALL,
                class: c as u32,
                used: 0,
                capacity: ((SPAN - HDR) / size) as u32,
                free: ptr::null_mut(),
                bump: HDR,
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
                large_map: 0,
            };
        }
        push(&mut cl.partial, s);
    }
    let b: *mut u8 = if !(*s).free.is_null() {
        let f = (*s).free;
        (*s).free = (*f).next;
        f as *mut u8
    } else {
        let b = (s as usize + (*s).bump) as *mut u8;
        (*s).bump += class_size(c);
        b
    };
    (*s).used += 1;
    if (*s).used == (*s).capacity {
        unlink(&mut cl.partial, s);
    }
    b
}

unsafe fn free_small(s: *mut Span, p: *mut u8) {
    let c = (*s).class as usize;
    let size = class_size(c);
    // the block's start (p may point inside it, after posix_memalign)
    let b = s as usize + HDR + (p as usize - s as usize - HDR) / size * size;
    let cl = &mut (*ptr::addr_of_mut!(CLASSES))[c];
    let was_full = (*s).used == (*s).capacity;
    let f = b as *mut Free;
    (*f).next = (*s).free;
    (*s).free = f;
    (*s).used -= 1;
    if was_full {
        push(&mut cl.partial, s);
    }
    if (*s).used == 0 {
        unlink(&mut cl.partial, s);
        if cl.spare.is_null() {
            // keep it, reset so it hands out fresh (zero) blocks from the start again
            (*s).free = ptr::null_mut();
            (*s).bump = HDR;
            sys::sc3(sys::MADVISE, s as usize + 4096, SPAN - 4096, 4 /* MADV_DONTNEED */);
            cl.spare = s;
        } else {
            sys::sc2(sys::MUNMAP, s as usize, SPAN);
        }
    }
}

unsafe fn alloc_large(n: usize, align: usize) -> *mut u8 {
    let off = (HDR).max(align);
    let Some(total) = n.checked_add(off).map(|t| (t + 4095) & !4095) else { return ptr::null_mut() };
    let m = map_aligned(total);
    if m.is_null() {
        return ptr::null_mut();
    }
    let s = m as *mut Span;
    (*s).kind = KIND_LARGE;
    (*s).large_map = total;
    m.add(off)
}

unsafe fn span_of(p: *mut u8) -> *mut Span {
    (p as usize & !(SPAN - 1)) as *mut Span
}

/// Usable size of a live block.
unsafe fn usable(p: *mut u8) -> usize {
    let s = span_of(p);
    if (*s).kind == KIND_LARGE {
        s as usize + (*s).large_map - p as usize
    } else {
        let size = class_size((*s).class as usize);
        let b = s as usize + HDR + (p as usize - s as usize - HDR) / size * size;
        b + size - p as usize
    }
}

#[no_mangle]
pub unsafe extern "C" fn malloc(n: usize) -> *mut u8 {
    let p = if n <= MAX_SMALL {
        let _g = LOCK.guard();
        alloc_small(class_of(n))
    } else {
        alloc_large(n, 16)
    };
    if p.is_null() {
        errno::set(errno::ENOMEM);
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let s = span_of(p);
    match (*s).kind {
        KIND_SMALL => {
            let _g = LOCK.guard();
            free_small(s, p)
        }
        KIND_LARGE => {
            sys::sc2(sys::MUNMAP, s as usize, (*s).large_map);
        }
        _ => crate::imp::start::abort(), // not ours: heap corruption or a bad pointer
    }
}

#[no_mangle]
pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut u8 {
    let Some(total) = n.checked_mul(size) else {
        errno::set(errno::ENOMEM);
        return ptr::null_mut();
    };
    let p = malloc(total);
    if !p.is_null() && total <= MAX_SMALL {
        // large blocks are fresh mappings, already zero
        ptr::write_bytes(p, 0, total);
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn realloc(p: *mut u8, n: usize) -> *mut u8 {
    if p.is_null() {
        return malloc(n);
    }
    if n == 0 {
        free(p);
        return ptr::null_mut();
    }
    let have = usable(p);
    let s = span_of(p);
    // stays in place when it still fits and does not waste more than half the block
    if n <= have && ((*s).kind == KIND_LARGE && n > MAX_SMALL || n * 2 > have || have <= 16) {
        return p;
    }
    let q = malloc(n);
    if q.is_null() {
        return q;
    }
    ptr::copy_nonoverlapping(p, q, have.min(n));
    free(p);
    q
}

#[no_mangle]
pub unsafe extern "C" fn posix_memalign(out: *mut *mut u8, align: usize, n: usize) -> i32 {
    if !align.is_power_of_two() || align < 8 {
        return errno::EINVAL;
    }
    let p = if align <= 16 {
        malloc(n)
    } else if align > SPAN / 2 {
        ptr::null_mut()
    } else if n + align <= MAX_SMALL {
        // a block big enough to hold an aligned run of n bytes; free() finds its start
        let b = malloc(n + align);
        if b.is_null() {
            b
        } else {
            ((b as usize + align - 1) & !(align - 1)) as *mut u8
        }
    } else {
        alloc_large(n, align)
    };
    if p.is_null() {
        return errno::ENOMEM;
    }
    *out = p;
    0
}

#[no_mangle]
pub unsafe extern "C" fn aligned_alloc(align: usize, n: usize) -> *mut u8 {
    let mut p = ptr::null_mut();
    if posix_memalign(&mut p, align, n) != 0 {
        return ptr::null_mut();
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn malloc_usable_size(p: *mut u8) -> usize {
    if p.is_null() {
        0
    } else {
        usable(p)
    }
}

/// fork takes the heap lock first, so no other thread is half-way through changing the
/// heap when the child's copy is made.
pub fn lock_for_fork() {
    LOCK.lock();
}

pub fn unlock_after_fork(child: bool) {
    if child {
        LOCK.reset();
    } else {
        LOCK.unlock();
    }
}
