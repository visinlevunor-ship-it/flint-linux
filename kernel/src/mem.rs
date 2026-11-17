//! Memory: physical pages, page tables and the kernel heap.
//!
//! Layout: all physical memory (and the framebuffer) is mapped at PHYS_OFF in the upper half,
//! shared by every address space. The kernel image runs where UEFI loaded it, so its pages
//! are also mapped at their physical address (kernel-only) in every address space. Programs
//! get the rest of the lower half.

use crate::efi::{MemoryDescriptor, MEM_BOOT_CODE, MEM_BOOT_DATA, MEM_CONVENTIONAL};
use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

pub const PAGE: u64 = 4096;
pub const PHYS_OFF: u64 = 0xFFFF_8000_0000_0000;

pub const P_PRESENT: u64 = 1;
pub const P_WRITE: u64 = 1 << 1;
pub const P_USER: u64 = 1 << 2;
pub const P_HUGE: u64 = 1 << 7;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

pub fn phys_to_virt(p: u64) -> *mut u8 {
    (PHYS_OFF + p) as *mut u8
}

// ---------------------------------------------------------------------------------------
// Physical pages: free ranges from the UEFI memory map, plus a list of pages given back.

#[derive(Clone, Copy)]
struct Range {
    next: u64,
    end: u64,
}

const MAX_RANGES: usize = 256;
static mut RANGES: [Range; MAX_RANGES] = [Range { next: 0, end: 0 }; MAX_RANGES];
static mut N_RANGES: usize = 0;
static mut FREE_LIST: u64 = 0; // physical address of a freed page holding the next one's address
static mut BEFORE_PAGING: bool = true;
pub static mut TOTAL_PAGES: u64 = 0;
pub static mut USED_PAGES: u64 = 0;

/// Take the usable memory from the UEFI memory map (after ExitBootServices: boot-services
/// memory is ours too). Pages below 1 MiB are left alone.
pub unsafe fn init_frames(map: &[MemoryDescriptor]) {
    for d in map {
        if !matches!(d.kind, MEM_CONVENTIONAL | MEM_BOOT_CODE | MEM_BOOT_DATA) {
            continue;
        }
        let start = d.physical_start.max(0x10_0000);
        let end = d.physical_start + d.number_of_pages * PAGE;
        if end <= start || N_RANGES == MAX_RANGES {
            continue;
        }
        RANGES[N_RANGES] = Range { next: start, end };
        N_RANGES += 1;
        TOTAL_PAGES += (end - start) / PAGE;
    }
}

/// Before our page tables are on, UEFI's identity mapping reaches physical memory directly.
fn page_ptr(p: u64) -> *mut u8 {
    // SAFETY: read-only check of a flag set once.
    if unsafe { BEFORE_PAGING } { p as *mut u8 } else { phys_to_virt(p) }
}

/// A zeroed physical page.
pub fn alloc_frame() -> u64 {
    // SAFETY: one CPU with interrupts off: no concurrent access to the allocator.
    unsafe {
        let p = if FREE_LIST != 0 {
            let p = FREE_LIST;
            FREE_LIST = *(page_ptr(p) as *const u64);
            p
        } else {
            let mut found = 0;
            for r in RANGES[..N_RANGES].iter_mut() {
                if r.next < r.end {
                    found = r.next;
                    r.next += PAGE;
                    break;
                }
            }
            if found == 0 {
                panic!("out of memory");
            }
            found
        };
        USED_PAGES += 1;
        ptr::write_bytes(page_ptr(p), 0, PAGE as usize);
        p
    }
}

/// `n` physically contiguous zeroed pages (for the heap at start-up).
pub fn alloc_contiguous(n: u64) -> u64 {
    // SAFETY: as alloc_frame.
    unsafe {
        for r in RANGES[..N_RANGES].iter_mut() {
            if r.end - r.next >= n * PAGE {
                let p = r.next;
                r.next += n * PAGE;
                USED_PAGES += n;
                ptr::write_bytes(page_ptr(p), 0, (n * PAGE) as usize);
                return p;
            }
        }
    }
    panic!("no {} contiguous pages", n);
}

pub fn free_frame(p: u64) {
    // SAFETY: as alloc_frame; the page is no longer mapped anywhere.
    unsafe {
        *(phys_to_virt(p) as *mut u64) = FREE_LIST;
        FREE_LIST = p;
        USED_PAGES -= 1;
    }
}

// ---------------------------------------------------------------------------------------
// Page tables

fn table(p: u64) -> *mut u64 {
    page_ptr(p) as *mut u64
}

/// The next-level table under `entry`, made if missing.
unsafe fn next_level(entry: *mut u64, user: bool) -> u64 {
    if *entry & P_PRESENT == 0 {
        let t = alloc_frame();
        *entry = t | P_PRESENT | P_WRITE | if user { P_USER } else { 0 };
    } else if user {
        *entry |= P_USER;
    }
    *entry & ADDR_MASK
}

/// Map one 4 KiB page. Returns false when something is already there.
pub fn map_page(pml4: u64, virt: u64, phys: u64, flags: u64) -> bool {
    let user = flags & P_USER != 0;
    // SAFETY: the tables are ours; indexes are 9-bit.
    unsafe {
        let mut t = pml4;
        for shift in [39u64, 30, 21] {
            let e = table(t).add(((virt >> shift) & 511) as usize);
            if *e & P_HUGE != 0 {
                return false;
            }
            t = next_level(e, user);
        }
        let e = table(t).add(((virt >> 12) & 511) as usize);
        if *e & P_PRESENT != 0 {
            return false;
        }
        *e = (phys & ADDR_MASK) | flags | P_PRESENT;
    }
    true
}

/// The page-table entry for `virt`, if a 4 KiB page is mapped there.
pub fn lookup(pml4: u64, virt: u64) -> Option<u64> {
    // SAFETY: walking our own tables.
    unsafe {
        let mut t = pml4;
        for shift in [39u64, 30, 21] {
            let e = *table(t).add(((virt >> shift) & 511) as usize);
            if e & P_PRESENT == 0 || e & P_HUGE != 0 {
                return None;
            }
            t = e & ADDR_MASK;
        }
        let e = *table(t).add(((virt >> 12) & 511) as usize);
        if e & P_PRESENT == 0 {
            None
        } else {
            Some(e)
        }
    }
}

/// Unmap one page; returns its physical address.
pub fn unmap_page(pml4: u64, virt: u64) -> Option<u64> {
    // SAFETY: walking our own tables.
    unsafe {
        let mut t = pml4;
        for shift in [39u64, 30, 21] {
            let e = *table(t).add(((virt >> shift) & 511) as usize);
            if e & P_PRESENT == 0 || e & P_HUGE != 0 {
                return None;
            }
            t = e & ADDR_MASK;
        }
        let e = table(t).add(((virt >> 12) & 511) as usize);
        if *e & P_PRESENT == 0 {
            return None;
        }
        let p = *e & ADDR_MASK;
        *e = 0;
        core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
        Some(p)
    }
}

/// Free a lower-half page-table tree. User pages (USER leaves) are freed; the kernel image's
/// pages (kernel-only leaves) are not ours to free.
pub fn free_lower_half(pml4: u64) {
    // SAFETY: the address space is no longer in use (CR3 points elsewhere).
    unsafe {
        for i in 0..256 {
            let e4 = *table(pml4).add(i);
            if e4 & P_PRESENT == 0 {
                continue;
            }
            let pdpt = e4 & ADDR_MASK;
            for j in 0..512 {
                let e3 = *table(pdpt).add(j);
                if e3 & P_PRESENT == 0 {
                    continue;
                }
                let pd = e3 & ADDR_MASK;
                for k in 0..512 {
                    let e2 = *table(pd).add(k);
                    if e2 & P_PRESENT == 0 {
                        continue;
                    }
                    let pt = e2 & ADDR_MASK;
                    for l in 0..512 {
                        let e1 = *table(pt).add(l);
                        if e1 & P_PRESENT != 0 && e1 & P_USER != 0 {
                            free_frame(e1 & ADDR_MASK);
                        }
                    }
                    free_frame(pt);
                }
                free_frame(pd);
            }
            free_frame(pdpt);
            *table(pml4).add(i) = 0;
        }
    }
}

/// fork: a new address space with a copy of every program page of `parent`.
pub fn clone_user(parent: u64) -> u64 {
    let child = new_address_space();
    // SAFETY: walking our own tables; pages are copied through the physical mapping.
    unsafe {
        for i in 0..256usize {
            let e4 = *table(parent).add(i);
            if e4 & P_PRESENT == 0 {
                continue;
            }
            for j in 0..512usize {
                let e3 = *table(e4 & ADDR_MASK).add(j);
                if e3 & P_PRESENT == 0 || e3 & P_HUGE != 0 {
                    continue;
                }
                for k in 0..512usize {
                    let e2 = *table(e3 & ADDR_MASK).add(k);
                    if e2 & P_PRESENT == 0 || e2 & P_HUGE != 0 {
                        continue;
                    }
                    for l in 0..512usize {
                        let e1 = *table(e2 & ADDR_MASK).add(l);
                        if e1 & P_PRESENT == 0 || e1 & P_USER == 0 {
                            continue;
                        }
                        let virt = ((i as u64) << 39) | ((j as u64) << 30) | ((k as u64) << 21) | ((l as u64) << 12);
                        let f = alloc_frame();
                        ptr::copy_nonoverlapping(phys_to_virt(e1 & ADDR_MASK), phys_to_virt(f), PAGE as usize);
                        map_page(child, virt, f, e1 & 0xFFF & !P_PRESENT);
                    }
                }
            }
        }
    }
    child
}

static mut KERNEL_PML4: u64 = 0;
static mut IMAGE: (u64, u64) = (0, 0);

/// Build the kernel's page tables: physical memory up to `limit` at PHYS_OFF (2 MiB pages),
/// the kernel image at its own address. Then switch to them.
pub unsafe fn init_paging(limit: u64, image_base: u64, image_size: u64) {
    let pml4 = alloc_frame();
    let limit = (limit + (1 << 30) - 1) & !((1 << 30) - 1);
    let mut p = 0u64;
    while p < limit {
        let v = PHYS_OFF + p;
        let e4 = table(pml4).add(((v >> 39) & 511) as usize);
        let pdpt = next_level(e4, false);
        let e3 = table(pdpt).add(((v >> 30) & 511) as usize);
        let pd = next_level(e3, false);
        *table(pd).add(((v >> 21) & 511) as usize) = p | P_PRESENT | P_WRITE | P_HUGE;
        p += 2 << 20;
    }
    IMAGE = (image_base & !(PAGE - 1), (image_base + image_size + PAGE - 1) & !(PAGE - 1));
    map_image(pml4);
    KERNEL_PML4 = pml4;
    BEFORE_PAGING = false;
    core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack));
}

unsafe fn map_image(pml4: u64) {
    let (start, end) = IMAGE;
    let mut a = start;
    while a < end {
        map_page(pml4, a, a, P_WRITE);
        a += PAGE;
    }
}

/// Device registers at `phys`: mapped uncached in the physical mapping (2 MiB pages) if they
/// are not there yet. Returns their address in the kernel's view.
pub fn map_mmio(phys: u64, size: u64) -> *mut u8 {
    const P_PWT: u64 = 1 << 3;
    const P_PCD: u64 = 1 << 4;
    let mut p = phys & !((2 << 20) - 1);
    // SAFETY: the kernel's own tables; the upper half is shared by every address space.
    unsafe {
        while p < phys + size {
            let v = PHYS_OFF + p;
            let pml4 = KERNEL_PML4;
            let e4 = table(pml4).add(((v >> 39) & 511) as usize);
            let pdpt = next_level(e4, false);
            let e3 = table(pdpt).add(((v >> 30) & 511) as usize);
            let pd = next_level(e3, false);
            let e2 = table(pd).add(((v >> 21) & 511) as usize);
            if *e2 & P_PRESENT == 0 {
                *e2 = p | P_PRESENT | P_WRITE | P_HUGE | P_PCD | P_PWT;
            }
            p += 2 << 20;
        }
    }
    phys_to_virt(phys)
}

/// The kernel image's range: programs must not be loaded over it.
pub fn image_range() -> (u64, u64) {
    // SAFETY: set once during start-up.
    unsafe { IMAGE }
}

/// A new address space: the shared upper half and the kernel image.
pub fn new_address_space() -> u64 {
    let pml4 = alloc_frame();
    // SAFETY: both tables are valid pages.
    unsafe {
        for i in 256..512 {
            *table(pml4).add(i) = *table(KERNEL_PML4).add(i);
        }
        map_image(pml4);
    }
    pml4
}

pub fn kernel_pml4() -> u64 {
    // SAFETY: set once during start-up.
    unsafe { KERNEL_PML4 }
}

pub fn switch_to(pml4: u64) {
    // SAFETY: pml4 maps the kernel image and the upper half like every address space.
    unsafe { core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack)) };
}

// ---------------------------------------------------------------------------------------
// Kernel heap: a first-fit free list over one contiguous block.

pub const HEAP_PAGES: u64 = 8192; // 32 MiB

struct Block {
    size: usize,
    next: *mut Block,
}

pub struct Heap;

static mut HEAD: *mut Block = ptr::null_mut();

pub unsafe fn init_heap() {
    let p = alloc_contiguous(HEAP_PAGES);
    let b = phys_to_virt(p) as *mut Block;
    (*b).size = (HEAP_PAGES * PAGE) as usize;
    (*b).next = ptr::null_mut();
    HEAD = b;
}

const MIN: usize = core::mem::size_of::<Block>();

fn round(n: usize) -> usize {
    (n.max(MIN) + 15) & !15
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let align = l.align().max(16);
        let size = round(l.size());
        let mut prev: *mut *mut Block = ptr::addr_of_mut!(HEAD);
        let mut b = HEAD;
        while !b.is_null() {
            let start = b as usize;
            let aligned = (start + align - 1) & !(align - 1);
            let pad = aligned - start;
            // a gap in front must be able to hold a block of its own
            let pad = if pad != 0 && pad < MIN { pad + ((MIN + align - 1) & !(align - 1)) } else { pad };
            let aligned = start + pad;
            if (*b).size >= pad + size {
                let rest = (*b).size - pad - size;
                let next = (*b).next;
                // what is left after the allocation
                let after = if rest >= MIN {
                    let r = (aligned + size) as *mut Block;
                    (*r).size = rest;
                    (*r).next = next;
                    r
                } else {
                    next
                };
                let used = if rest >= MIN { size } else { size + rest };
                if pad != 0 {
                    (*b).size = pad;
                    (*b).next = after;
                } else {
                    *prev = after;
                }
                // remember the size for dealloc: kept in the layout (callers give it back)
                let _ = used;
                return aligned as *mut u8;
            }
            prev = ptr::addr_of_mut!((*b).next);
            b = (*b).next;
        }
        ptr::null_mut()
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let size = round(l.size());
        // insert in address order, merging with neighbours
        let nb = p as *mut Block;
        (*nb).size = size;
        let mut prev: *mut Block = ptr::null_mut();
        let mut b = HEAD;
        while !b.is_null() && (b as usize) < (nb as usize) {
            prev = b;
            b = (*b).next;
        }
        (*nb).next = b;
        if !b.is_null() && nb as usize + (*nb).size == b as usize {
            (*nb).size += (*b).size;
            (*nb).next = (*b).next;
        }
        if prev.is_null() {
            HEAD = nb;
        } else {
            (*prev).next = nb;
            if prev as usize + (*prev).size == nb as usize {
                (*prev).size += (*nb).size;
                (*prev).next = (*nb).next;
            }
        }
    }
}
