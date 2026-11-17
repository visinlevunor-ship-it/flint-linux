//! Programs: loading an ELF executable into a fresh address space, its stack (arguments,
//! environment, auxiliary vector as Linux lays them out), memory (brk, mmap) and open files.
//!
//! One program runs at a time for now (no fork yet).

use crate::arch;
use crate::cpio::{self, Node};
use crate::mem::{self, P_USER, P_WRITE, PAGE};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const USER_TOP: u64 = 0x0000_8000_0000_0000;
const STACK_TOP: u64 = 0x7FFF_FFFF_F000;
const STACK_SIZE: u64 = 8 << 20;
const STACK_MAPPED: u64 = 1 << 20; // mapped at start; the rest on demand later
const MMAP_TOP: u64 = 0x7F00_0000_0000;
const PIE_BASE: u64 = 0x5555_5555_4000;

#[derive(Clone)]
pub enum Kind {
    Console,
    File(&'static Node),
    Dir(String),
}

#[derive(Clone)]
pub struct File {
    pub kind: Kind,
    pub pos: usize,
    pub flags: u32,
    pub path: String,
}

pub struct Proc {
    pub name: String,
    pub pml4: u64,
    pub brk_start: u64,
    pub brk: u64,
    pub mmap_top: u64,
    pub fds: Vec<Option<File>>,
    pub cwd: String,
    pub tid_address: u64,
}

static mut CURRENT: Option<Proc> = None;

pub fn current() -> &'static mut Proc {
    // SAFETY: one CPU, interrupts off; set while a program runs.
    unsafe { (*core::ptr::addr_of_mut!(CURRENT)).as_mut().expect("no program running") }
}

pub fn name() -> String {
    // SAFETY: as current().
    unsafe { (*core::ptr::addr_of!(CURRENT)).as_ref().map(|p| p.name.clone()).unwrap_or_default() }
}

pub fn exit_current(code: i64) -> ! {
    // SAFETY: exit_to_kernel returns to run(), which tears the program down.
    unsafe { arch::exit_to_kernel(code) }
}

// ---------------------------------------------------------------------------------------
// User memory

/// Map fresh zeroed pages over [start, end) where nothing is mapped yet.
pub fn map_range(pml4: u64, start: u64, end: u64, flags: u64) -> Result<(), i64> {
    let (ks, ke) = mem::image_range();
    let mut a = start & !(PAGE - 1);
    while a < end {
        if a >= USER_TOP || (a >= ks && a < ke) {
            return Err(12); // ENOMEM: not a place for program memory
        }
        if mem::lookup(pml4, a).is_none() {
            let f = mem::alloc_frame();
            mem::map_page(pml4, a, f, flags | P_USER);
        }
        a += PAGE;
    }
    Ok(())
}

pub fn unmap_range(pml4: u64, start: u64, end: u64) {
    let mut a = start & !(PAGE - 1);
    while a < end {
        if let Some(e) = mem::lookup(pml4, a) {
            if e & P_USER != 0 {
                if let Some(p) = mem::unmap_page(pml4, a) {
                    mem::free_frame(p);
                }
            }
        }
        a += PAGE;
    }
}

/// Is [p, p+len) program memory mapped in the current address space?
pub fn user_ok(p: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = p.checked_add(len) else { return false };
    if end > USER_TOP {
        return false;
    }
    let pml4 = current().pml4;
    let mut a = p & !(PAGE - 1);
    while a < end {
        match mem::lookup(pml4, a) {
            Some(e) if e & P_USER != 0 => {}
            _ => return false,
        }
        a += PAGE;
    }
    true
}

/// A program's buffer, checked. The current address space is the program's, so its memory
/// is reachable at its own addresses.
pub fn user_slice(p: u64, len: u64) -> Result<&'static mut [u8], i64> {
    if !user_ok(p, len) {
        return Err(14); // EFAULT
    }
    if len == 0 {
        return Ok(&mut []);
    }
    // SAFETY: checked mapped and user-owned just above.
    Ok(unsafe { core::slice::from_raw_parts_mut(p as *mut u8, len as usize) })
}

/// A NUL-terminated string from a program.
pub fn user_str(p: u64) -> Result<String, i64> {
    let mut out = Vec::new();
    let mut a = p;
    loop {
        if !user_ok(a, 1) {
            return Err(14);
        }
        // SAFETY: checked.
        let b = unsafe { *(a as *const u8) };
        if b == 0 {
            break;
        }
        out.push(b);
        if out.len() > 4096 {
            return Err(36); // ENAMETOOLONG
        }
        a += 1;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

pub fn user_u64(p: u64) -> Result<u64, i64> {
    let s = user_slice(p, 8)?;
    Ok(u64::from_le_bytes(s.try_into().unwrap()))
}

// ---------------------------------------------------------------------------------------
// ELF

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;

struct Image {
    entry: u64,
    phdr: u64,
    phent: u64,
    phnum: u64,
    end: u64,
}

fn rd16(b: &[u8], o: usize) -> u64 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u64
}
fn rd32(b: &[u8], o: usize) -> u64 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as u64
}
fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

const ENOEXEC: i64 = 8;

fn load_elf(pml4: u64, f: &[u8]) -> Result<Image, i64> {
    if f.len() < 64 || &f[..4] != b"\x7fELF" || f[4] != 2 || f[5] != 1 || rd16(f, 18) != 0x3E {
        return Err(ENOEXEC);
    }
    let etype = rd16(f, 16);
    let base = match etype {
        2 => 0,
        3 => PIE_BASE,
        _ => return Err(ENOEXEC),
    };
    let phoff = rd64(f, 32) as usize;
    let phent = rd16(f, 54) as usize;
    let phnum = rd16(f, 56) as usize;
    if phent < 56 || phoff + phent * phnum > f.len() {
        return Err(ENOEXEC);
    }
    let mut end = 0;
    let mut phdr = 0;
    let mut first_load = None;
    for i in 0..phnum {
        let h = &f[phoff + i * phent..];
        let kind = rd32(h, 0) as u32;
        let off = rd64(h, 8);
        let vaddr = base + rd64(h, 16);
        let filesz = rd64(h, 32);
        let memsz = rd64(h, 40);
        match kind {
            PT_INTERP => {
                crate::kprintln!("[kernel] {}: dynamically linked programs are not supported yet", name());
                return Err(ENOEXEC);
            }
            PT_PHDR => phdr = vaddr,
            PT_LOAD => {
                if first_load.is_none() {
                    first_load = Some(vaddr - off);
                }
                if off as usize + filesz as usize > f.len() || memsz < filesz {
                    return Err(ENOEXEC);
                }
                map_range(pml4, vaddr, vaddr + memsz, P_WRITE)?;
                // copy the file's bytes in through the physical mapping
                let mut done = 0u64;
                while done < filesz {
                    let v = vaddr + done;
                    let in_page = (PAGE - (v & (PAGE - 1))).min(filesz - done);
                    let e = mem::lookup(pml4, v).ok_or(ENOEXEC)?;
                    let dst = mem::phys_to_virt((e & 0x000F_FFFF_FFFF_F000) + (v & (PAGE - 1)));
                    // SAFETY: dst is inside the page just mapped; the source is in bounds.
                    unsafe { core::ptr::copy_nonoverlapping(f.as_ptr().add((off + done) as usize), dst, in_page as usize) };
                    done += in_page;
                }
                end = end.max(vaddr + memsz);
            }
            _ => {}
        }
    }
    if phdr == 0 {
        phdr = first_load.ok_or(ENOEXEC)? + phoff as u64;
    }
    Ok(Image { entry: base + rd64(f, 24), phdr, phent: phent as u64, phnum: phnum as u64, end })
}

/// Lay out the stack Linux gives a new program: argc, argv, envp, auxv, then the strings.
fn build_stack(pml4: u64, img: &Image, path: &str, argv: &[String], envp: &[String]) -> Result<u64, i64> {
    map_range(pml4, STACK_TOP - STACK_MAPPED, STACK_TOP, P_WRITE)?;
    let mut sp = STACK_TOP;
    let push_bytes = |sp: &mut u64, b: &[u8]| -> u64 {
        *sp -= b.len() as u64 + 1;
        // SAFETY: the stack pages are mapped and the current address space is this one.
        unsafe {
            core::ptr::copy_nonoverlapping(b.as_ptr(), *sp as *mut u8, b.len());
            *((*sp + b.len() as u64) as *mut u8) = 0;
        }
        *sp
    };
    let execfn = push_bytes(&mut sp, path.as_bytes());
    let platform = push_bytes(&mut sp, b"x86_64");
    let arg_ptrs: Vec<u64> = argv.iter().map(|a| push_bytes(&mut sp, a.as_bytes())).collect();
    let env_ptrs: Vec<u64> = envp.iter().map(|e| push_bytes(&mut sp, e.as_bytes())).collect();
    sp &= !15;
    sp -= 16;
    let random = sp;
    let r1 = crate::syscall::random_u64();
    let r2 = crate::syscall::random_u64();
    // SAFETY: as above.
    unsafe {
        *(random as *mut u64) = r1;
        *((random + 8) as *mut u64) = r2;
    }
    let auxv: [(u64, u64); 16] = [
        (3, img.phdr),   // AT_PHDR
        (4, img.phent),  // AT_PHENT
        (5, img.phnum),  // AT_PHNUM
        (6, PAGE),       // AT_PAGESZ
        (7, 0),          // AT_BASE
        (8, 0),          // AT_FLAGS
        (9, img.entry),  // AT_ENTRY
        (11, 0),         // AT_UID
        (12, 0),         // AT_EUID
        (13, 0),         // AT_GID
        (14, 0),         // AT_EGID
        (16, crate::syscall::hwcap()), // AT_HWCAP
        (17, 100),       // AT_CLKTCK
        (23, 0),         // AT_SECURE
        (25, random),    // AT_RANDOM
        (31, execfn),    // AT_EXECFN
    ];
    let _ = platform;
    // words below the strings: argc, argv..., 0, envp..., 0, auxv pairs, AT_NULL
    let words = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + auxv.len() * 2 + 2;
    sp -= words as u64 * 8;
    sp &= !15;
    let mut w = sp as *mut u64;
    // SAFETY: as above; `words` slots were reserved.
    unsafe {
        let mut put = |v: u64| {
            *w = v;
            w = w.add(1);
        };
        put(arg_ptrs.len() as u64);
        for p in &arg_ptrs {
            put(*p);
        }
        put(0);
        for p in &env_ptrs {
            put(*p);
        }
        put(0);
        for (k, v) in auxv {
            put(k);
            put(v);
        }
        put(0);
        put(0);
    }
    Ok(sp)
}

fn console_fds() -> Vec<Option<File>> {
    let c = |flags| Some(File { kind: Kind::Console, pos: 0, flags, path: "/dev/console".to_string() });
    alloc::vec![c(0), c(1), c(1)]
}

/// Load `path` into a new address space (which becomes the current one). On success, the
/// entry point and stack pointer to start it with.
pub fn exec(path: &str, argv: &[String], envp: &[String], cwd: &str) -> Result<(u64, u64, u64, u64), i64> {
    let canon = cpio::resolve(cwd, path, true)?;
    let node = cpio::files().get(&canon).ok_or(cpio::ENOENT)?;
    if node.is_dir() {
        return Err(13); // EACCES
    }
    let pml4 = mem::new_address_space();
    mem::switch_to(pml4);
    let img = match load_elf(pml4, node.data) {
        Ok(i) => i,
        Err(e) => {
            mem::switch_to(mem::kernel_pml4());
            mem::free_lower_half(pml4);
            mem::free_frame(pml4);
            return Err(e);
        }
    };
    let sp = match build_stack(pml4, &img, path, argv, envp) {
        Ok(sp) => sp,
        Err(e) => {
            mem::switch_to(mem::kernel_pml4());
            mem::free_lower_half(pml4);
            mem::free_frame(pml4);
            return Err(e);
        }
    };
    let brk = (img.end + PAGE - 1) & !(PAGE - 1);
    Ok((pml4, img.entry, sp, brk))
}

/// Run a program to its end; its exit status.
pub fn run(path: &str, argv: &[String], envp: &[String]) -> Result<i64, i64> {
    // SAFETY: nothing runs between programs.
    unsafe { CURRENT = Some(Proc { name: argv.first().cloned().unwrap_or_default(), pml4: 0, brk_start: 0, brk: 0, mmap_top: MMAP_TOP, fds: console_fds(), cwd: String::new(), tid_address: 0 }) };
    let (pml4, entry, sp, brk) = match exec(path, argv, envp, "") {
        Ok(v) => v,
        Err(e) => {
            // SAFETY: as above.
            unsafe { CURRENT = None };
            return Err(e);
        }
    };
    {
        let p = current();
        p.pml4 = pml4;
        p.brk_start = brk;
        p.brk = brk;
    }
    // SAFETY: the address space is set up; enter_user returns when the program exits.
    let code = unsafe { arch::enter_user(entry, sp) };
    mem::switch_to(mem::kernel_pml4());
    // SAFETY: as above.
    unsafe {
        arch::wrmsr(arch::MSR_FS_BASE, 0);
        if let Some(p) = (*core::ptr::addr_of_mut!(CURRENT)).take() {
            mem::free_lower_half(p.pml4);
            mem::free_frame(p.pml4);
        }
    }
    Ok(code)
}

/// execve from a running program: replace its image. On success the program continues at the
/// returned entry point and stack.
pub fn replace(path: &str, argv: &[String], envp: &[String]) -> Result<(u64, u64), i64> {
    let p = current();
    let old = p.pml4;
    let cwd = p.cwd.clone();
    let (pml4, entry, sp, brk) = match exec(path, argv, envp, &cwd) {
        Ok(v) => v,
        Err(e) => {
            mem::switch_to(old);
            return Err(e);
        }
    };
    mem::free_lower_half(old);
    mem::free_frame(old);
    p.pml4 = pml4;
    p.brk_start = brk;
    p.brk = brk;
    p.mmap_top = MMAP_TOP;
    p.name = argv.first().cloned().unwrap_or_default();
    // close-on-exec descriptors
    for f in p.fds.iter_mut() {
        if f.as_ref().is_some_and(|f| f.flags & 0o2000000 != 0) {
            *f = None;
        }
    }
    Ok((entry, sp))
}

pub fn stack_limit() -> u64 {
    STACK_SIZE
}
