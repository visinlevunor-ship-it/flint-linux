//! The heap: power-of-two size classes carved from mmap'd chunks, with
//! per-class free lists; blocks above 64 KiB get their own mapping.
//!
//! Single threaded by design (see crate docs). Blocks are aligned to their own
//! size, so any layout whose alignment is <= its rounded size is satisfied.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;

use crate::sys;

const MIN_SHIFT: usize = 4; // 16 bytes
const MAX_SHIFT: usize = 16; // 64 KiB
const CLASSES: usize = MAX_SHIFT - MIN_SHIFT + 1;
const CHUNK: usize = 256 * 1024;
const PAGE: usize = 4096;

struct State {
    free: [*mut u8; CLASSES],
    bump: usize,
    end: usize,
}

pub struct FlintHeap {
    state: UnsafeCell<State>,
}

// SAFETY: Flint programs are single threaded; see the crate docs.
unsafe impl Sync for FlintHeap {}

impl FlintHeap {
    pub const fn new() -> Self {
        FlintHeap { state: UnsafeCell::new(State { free: [ptr::null_mut(); CLASSES], bump: 0, end: 0 }) }
    }
}

/// Size a request is rounded to; alignment is satisfied by size >= align.
#[inline]
fn block_size(layout: Layout) -> usize {
    layout.size().max(layout.align()).max(1 << MIN_SHIFT)
}

#[inline]
fn round_up(n: usize, to: usize) -> usize {
    (n + to - 1) & !(to - 1)
}

unsafe impl GlobalAlloc for FlintHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = block_size(layout);
        if size > (1 << MAX_SHIFT) {
            if layout.align() > PAGE {
                return ptr::null_mut();
            }
            return sys::mmap_anon(round_up(size, PAGE)).unwrap_or(ptr::null_mut());
        }

        let size = size.next_power_of_two();
        let class = size.trailing_zeros() as usize - MIN_SHIFT;
        let st = &mut *self.state.get();

        let head = st.free[class];
        if !head.is_null() {
            st.free[class] = *(head as *mut *mut u8);
            return head;
        }

        let mut p = round_up(st.bump, size);
        if p + size > st.end {
            let base = match sys::mmap_anon(CHUNK) {
                Ok(b) => b as usize,
                Err(_) => return ptr::null_mut(),
            };
            st.end = base + CHUNK;
            p = round_up(base, size);
        }
        st.bump = p + size;
        p as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = block_size(layout);
        if size > (1 << MAX_SHIFT) {
            let _ = sys::munmap(ptr, round_up(size, PAGE));
            return;
        }
        let size = size.next_power_of_two();
        let class = size.trailing_zeros() as usize - MIN_SHIFT;
        let st = &mut *self.state.get();
        *(ptr as *mut *mut u8) = st.free[class];
        st.free[class] = ptr;
    }
}
