//! The first instructions: the kernel jumps to `_dl_start` (this object is the program's
//! interpreter) with the program's stack. Nothing in this object works until its own
//! relocations are applied, so `self_relocate` does that using only registers, the stack
//! and plain loads and stores: no calls, no statics.

use crate::elf::*;

core::arch::global_asm!(
    ".globl _dl_start",
    ".type _dl_start,@function",
    "_dl_start:",
    "xor ebp, ebp",
    "mov rbx, rsp",          // the kernel's stack pointer: argc, argv, envp, auxv
    "and rsp, -16",
    "lea rdi, [rip + __ehdr_start]",
    "lea rsi, [rip + _DYNAMIC]",
    "call {reloc}",
    "mov rdi, rbx",
    "call {start}",          // returns the program's entry in rax, its stack pointer in rdx
    "mov rsp, rdx",
    "lea rdx, [rip + {fini}]", // rtld_fini: __libc_start_main registers it with atexit
    "jmp rax",
    reloc = sym self_relocate,
    start = sym crate::loader::start,
    fini = sym crate::loader::dl_fini,
);

/// Apply this object's own relocations. Called before anything else; may not call
/// functions or use statics (they are what is being fixed up).
#[inline(never)]
unsafe extern "C" fn self_relocate(base: usize, dynamic: *const Dyn) {
    let (mut rela, mut relasz, mut jmprel, mut pltrelsz) = (0usize, 0usize, 0usize, 0usize);
    let (mut relr, mut relrsz, mut symtab) = (0usize, 0usize, 0usize);
    let mut d = dynamic;
    loop {
        let tag = core::ptr::read_volatile(&(*d).d_tag);
        let val = core::ptr::read_volatile(&(*d).d_val) as usize;
        match tag {
            DT_NULL => break,
            DT_RELA => rela = base + val,
            DT_RELASZ => relasz = val,
            DT_JMPREL => jmprel = base + val,
            DT_PLTRELSZ => pltrelsz = val,
            DT_RELR => relr = base + val,
            DT_RELRSZ => relrsz = val,
            DT_SYMTAB => symtab = base + val,
            _ => {}
        }
        d = d.add(1);
    }
    let mut pass = 0;
    while pass < 2 {
        let (start, size) = if pass == 0 { (rela, relasz) } else { (jmprel, pltrelsz) };
        pass += 1;
        let mut r = start;
        while r < start + size {
            let off = core::ptr::read_volatile(r as *const u64) as usize;
            let info = core::ptr::read_volatile((r + 8) as *const u64);
            let addend = core::ptr::read_volatile((r + 16) as *const i64);
            let ty = (info & 0xffff_ffff) as u32;
            let symi = (info >> 32) as usize;
            let where_ = (base + off) as *mut usize;
            let symval = if symi != 0 {
                // st_value is at offset 8 in a 24-byte Sym
                base + core::ptr::read_volatile((symtab + symi * 24 + 8) as *const u64) as usize
            } else {
                0
            };
            match ty {
                R_X86_64_RELATIVE => core::ptr::write_volatile(where_, base.wrapping_add(addend as usize)),
                R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => core::ptr::write_volatile(where_, symval),
                R_X86_64_64 => core::ptr::write_volatile(where_, symval.wrapping_add(addend as usize)),
                _ => {}
            }
            r += 24;
        }
    }
    // RELR: packed relative relocations
    let mut r = relr;
    let mut where_: usize = 0;
    while r < relr + relrsz {
        let e = core::ptr::read_volatile(r as *const usize);
        if e & 1 == 0 {
            where_ = base + e;
            let p = where_ as *mut usize;
            core::ptr::write_volatile(p, core::ptr::read_volatile(p).wrapping_add(base));
            where_ += 8;
        } else {
            let mut bits = e >> 1;
            let mut i = 0;
            while bits != 0 {
                if bits & 1 != 0 {
                    let p = (where_ + i * 8) as *mut usize;
                    core::ptr::write_volatile(p, core::ptr::read_volatile(p).wrapping_add(base));
                }
                bits >>= 1;
                i += 1;
            }
            where_ += 63 * 8;
        }
        r += 8;
    }
}
