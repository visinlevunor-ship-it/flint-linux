//! Processes: the process table, the scheduler, fork, execve, exit and wait, signals with
//! their default actions, and each process's memory (ELF loading, the stack, brk, mmap).
//!
//! One CPU. Each process has its own kernel stack; a process in the kernel is always in a
//! system call or an interrupt, and the scheduler switches stacks (context_switch) when one
//! has to wait, exits, or used up its time slice (the timer, from user mode only).

use crate::arch::{self, SyscallFrame};
use crate::mem::{self, P_USER, P_WRITE, PAGE};
use crate::syscall::{Fd, OpenFile};
use crate::vfs;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

pub const USER_TOP: u64 = 0x0000_8000_0000_0000;
const STACK_TOP: u64 = 0x7FFF_FFFF_F000;
const STACK_SIZE: u64 = 8 << 20;
const STACK_MAPPED: u64 = 256 << 10; // the rest is mapped when first touched
const MMAP_TOP: u64 = 0x7F00_0000_0000;
const PIE_BASE: u64 = 0x5555_5555_4000;
const KSTACK_WORDS: usize = 8192; // 64 KiB
const SLICE_TICKS: u64 = 5; // 50 ms

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Runnable,
    Blocked,
    /// exited; the wait status for the parent
    Zombie(i32),
}

pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub sid: i32,
    pub name: String,
    pub pml4: u64,
    kstack: Vec<u128>,
    ksp: u64,
    pub state: State,
    fs_base: u64,
    pub brk_start: u64,
    pub brk: u64,
    pub mmap_top: u64,
    pub fds: Vec<Option<Fd>>,
    pub cwd: String,
    /// per signal, what sigaction set: 0 default (SIG_DFL), 1 ignore (SIG_IGN), else the
    /// handler's address (handlers are not called yet)
    pub sigdisp: [u64; 65],
    /// a fatal signal waiting to be acted on
    pub killed: i32,
    /// a vfork parent waits until this child execs or exits
    pub vfork_parent: i32,
    pub umask: u32,
    slice_end: u64,
}

impl Proc {
    fn kstack_top(&self) -> u64 {
        self.kstack.as_ptr() as u64 + (self.kstack.len() * 16) as u64
    }
}

static mut PROCS: Option<BTreeMap<i32, Box<Proc>>> = None;
static mut CUR: i32 = 0;
static mut NEXT_PID: i32 = 1;
static mut BOOT_KSP: u64 = 0;

fn procs() -> &'static mut BTreeMap<i32, Box<Proc>> {
    // SAFETY: one CPU; the kernel never runs two things at once.
    unsafe { (*core::ptr::addr_of_mut!(PROCS)).get_or_insert_with(BTreeMap::new) }
}

pub fn current() -> &'static mut Proc {
    // SAFETY: as procs().
    let pid = unsafe { CUR };
    procs().get_mut(&pid).expect("no current process")
}

pub fn pid() -> i32 {
    // SAFETY: as procs().
    unsafe { CUR }
}

pub fn name() -> String {
    // SAFETY: as procs().
    let pid = unsafe { CUR };
    procs().get(&pid).map(|p| p.name.clone()).unwrap_or_default()
}

pub fn get(pid: i32) -> Option<&'static mut Proc> {
    procs().get_mut(&pid).map(|b| &mut **b)
}

pub fn count() -> usize {
    procs().len()
}

// ---------------------------------------------------------------------------------------
// Scheduling

/// Mark every waiting process runnable: each checks again what it waits for.
pub fn wake_all() {
    for p in procs().values_mut() {
        if p.state == State::Blocked {
            p.state = State::Runnable;
        }
    }
}

/// Run something else: the next runnable process after the current one. Returns when the
/// current process is chosen again. With nothing to run, wait for an interrupt.
fn schedule() {
    loop {
        // SAFETY: as procs().
        let cur = unsafe { CUR };
        let next = procs()
            .range(cur + 1..)
            .chain(procs().range(..=cur))
            .find(|(_, p)| p.state == State::Runnable)
            .map(|(pid, _)| *pid);
        match next {
            Some(n) if n == cur => return,
            Some(n) => {
                switch(cur, n);
                return;
            }
            None => {
                crate::dev::poll_serial();
                arch::wait_for_interrupt();
            }
        }
    }
}

fn switch(from: i32, to: i32) {
    let next = procs().get_mut(&to).unwrap();
    let next_ksp = next.ksp;
    mem::switch_to(next.pml4);
    arch::set_kernel_stack(next.kstack_top());
    next.slice_end = crate::dev::ticks() + SLICE_TICKS;
    let fs = next.fs_base;
    // SAFETY: thread pointers belong to processes; the old one is kept in its Proc.
    unsafe {
        if let Some(prev) = procs().get_mut(&from) {
            prev.fs_base = arch::rdmsr(arch::MSR_FS_BASE);
        }
        arch::wrmsr(arch::MSR_FS_BASE, fs);
        CUR = to;
    }
    let save: *mut u64 = match procs().get_mut(&from) {
        Some(p) => &mut p.ksp,
        // SAFETY: the boot stack's slot, used once.
        None => unsafe { core::ptr::addr_of_mut!(BOOT_KSP) },
    };
    // SAFETY: both stacks are valid kernel stacks; `save` stays valid (the Proc is boxed).
    unsafe { arch::context_switch(save, next_ksp) };
}

/// Wait until woken (by an interrupt or another process), then act on a fatal signal.
pub fn block() {
    current().state = State::Blocked;
    schedule();
    check_signals();
}

/// Give the CPU to someone else if anyone wants it.
pub fn yield_now() {
    current().state = State::Runnable;
    schedule();
    check_signals();
}

pub fn check_signals() {
    let p = current();
    if p.killed != 0 {
        let s = p.killed;
        exit_signal(s);
    }
}

/// The timer: ends the running program's time slice.
pub fn tick(from_user: bool) {
    wake_all(); // sleepers check their clocks
    // SAFETY: as procs().
    if !from_user || unsafe { CUR } == 0 {
        return;
    }
    if crate::dev::ticks() >= current().slice_end {
        yield_now();
    } else {
        check_signals();
    }
}

// ---------------------------------------------------------------------------------------
// Creating, ending and waiting for processes

fn new_kstack() -> Vec<u128> {
    alloc::vec![0u128; KSTACK_WORDS]
}

/// Set up a process's kernel stack so that its first run returns to the program with
/// `frame`'s registers (through the system call return path).
fn prime(p: &mut Proc, frame: &SyscallFrame) {
    let top = p.kstack_top();
    let fsize = core::mem::size_of::<SyscallFrame>() as u64;
    let fp = top - fsize;
    // SAFETY: inside the new kernel stack.
    unsafe {
        core::ptr::copy_nonoverlapping(frame as *const SyscallFrame, fp as *mut SyscallFrame, 1);
        let mut sp = fp;
        sp -= 8;
        *(sp as *mut u64) = arch::syscall_return_addr();
        for _ in 0..6 {
            sp -= 8;
            *(sp as *mut u64) = 0; // r15..rbx for context_switch to pop
        }
        p.ksp = sp;
    }
}

fn alloc_pid() -> i32 {
    // SAFETY: one CPU.
    unsafe {
        loop {
            let pid = NEXT_PID;
            NEXT_PID = if NEXT_PID >= 32767 { 2 } else { NEXT_PID + 1 };
            if !procs().contains_key(&pid) {
                return pid;
            }
        }
    }
}

fn empty_frame(rip: u64, rsp: u64) -> SyscallFrame {
    SyscallFrame { r15: 0, r14: 0, r13: 0, r12: 0, rbp: 0, rbx: 0, r9: 0, r8: 0, r10: 0, rdx: 0, rsi: 0, rdi: 0, rax: 0, rflags: 0x202, rip, rsp }
}

/// The first process (pid 1): `path` with the console as its terminal. Never returns: the
/// boot stack is left for good.
pub fn start_init(path: &str, argv: &[String], envp: &[String], fds: Vec<Option<Fd>>) -> Result<(), i64> {
    let pid = alloc_pid();
    let (pml4, entry, sp, brk) = exec_image(path, argv, envp, "")?;
    let mut p = Box::new(Proc {
        pid,
        ppid: 0,
        pgid: pid,
        sid: pid,
        name: argv.first().cloned().unwrap_or_default(),
        pml4,
        kstack: new_kstack(),
        ksp: 0,
        state: State::Runnable,
        fs_base: 0,
        brk_start: brk,
        brk,
        mmap_top: MMAP_TOP,
        fds,
        cwd: String::new(),
        sigdisp: [0; 65],
        killed: 0,
        vfork_parent: 0,
        umask: 0o022,
        slice_end: 0,
    });
    prime(&mut p, &empty_frame(entry, sp));
    procs().insert(pid, p);
    crate::tty::tty().fg_pgrp = pid;
    switch(0, pid);
    unreachable!("the boot stack is never resumed");
}

/// fork: a copy of the current process that continues from the same system call with 0.
pub fn fork(frame: &SyscallFrame, vfork: bool) -> Result<i32, i64> {
    let pid = alloc_pid();
    let parent = current();
    // SAFETY: the parent's thread pointer is the current one.
    let fs_base = unsafe { arch::rdmsr(arch::MSR_FS_BASE) };
    let pml4 = mem::clone_user(parent.pml4);
    let mut child = Box::new(Proc {
        pid,
        ppid: parent.pid,
        pgid: parent.pgid,
        sid: parent.sid,
        name: parent.name.clone(),
        pml4,
        kstack: new_kstack(),
        ksp: 0,
        state: State::Runnable,
        fs_base,
        brk_start: parent.brk_start,
        brk: parent.brk,
        mmap_top: parent.mmap_top,
        fds: parent.fds.clone(),
        cwd: parent.cwd.clone(),
        sigdisp: parent.sigdisp,
        killed: 0,
        vfork_parent: if vfork { parent.pid } else { 0 },
        umask: parent.umask,
        slice_end: 0,
    });
    let mut f = SyscallFrame { ..*frame };
    f.rax = 0;
    prime(&mut child, &f);
    procs().insert(pid, child);
    if vfork {
        // the parent waits until the child has exec'd or exited
        while get(pid).is_some_and(|c| c.vfork_parent != 0 && !matches!(c.state, State::Zombie(_))) {
            block();
        }
    }
    Ok(pid)
}

fn release_vfork_parent(p: &mut Proc) {
    if p.vfork_parent != 0 {
        p.vfork_parent = 0;
        wake_all();
    }
}

/// End the current process with wait status `status`.
fn do_exit(status: i32) -> ! {
    let me = pid();
    {
        let p = current();
        p.fds.clear(); // closes files and pipe ends
        release_vfork_parent(p);
        mem::switch_to(mem::kernel_pml4());
        mem::free_lower_half(p.pml4);
        mem::free_frame(p.pml4);
        p.pml4 = mem::kernel_pml4();
        p.state = State::Zombie(status);
    }
    // children go to pid 1
    for c in procs().values_mut() {
        if c.ppid == me {
            c.ppid = 1;
        }
    }
    if me == 1 {
        crate::kprintln!("\n[kernel] the first program ended (status {:#x}); nothing left to run. The machine can be switched off.", status);
        arch::halt();
    }
    wake_all(); // the parent may be in wait
    schedule();
    unreachable!("a zombie never runs again");
}

pub fn exit_current(code: i64) -> ! {
    do_exit(((code as i32) & 0xFF) << 8)
}

pub fn exit_signal(sig: i32) -> ! {
    do_exit(sig & 0x7F)
}

/// wait4: a child that has ended (pid > 0: that one; -1: any; 0: same group; < -1: group
/// -pid). Returns (pid, status), or None with WNOHANG and nothing ended yet.
pub fn wait(pid: i32, nohang: bool) -> Result<Option<(i32, i32)>, i64> {
    loop {
        let me = current();
        let (my_pid, my_pgid) = (me.pid, me.pgid);
        let mut have_child = false;
        let mut found = None;
        for (cp, c) in procs().iter() {
            if c.ppid != my_pid {
                continue;
            }
            let matches = match pid {
                -1 => true,
                0 => c.pgid == my_pgid,
                p if p > 0 => *cp == p,
                p => c.pgid == -p,
            };
            if !matches {
                continue;
            }
            have_child = true;
            if let State::Zombie(st) = c.state {
                found = Some((*cp, st));
                break;
            }
        }
        if let Some((cp, st)) = found {
            procs().remove(&cp); // frees its kernel stack
            return Ok(Some((cp, st)));
        }
        if !have_child {
            return Err(10); // ECHILD
        }
        if nohang {
            return Ok(None);
        }
        block();
    }
}

/// Does `sig`'s default action end the process?
fn fatal_by_default(sig: i32) -> bool {
    !matches!(sig, 17 | 18 | 19 | 20 | 21 | 22 | 23 | 28) // CHLD CONT STOP TSTP TTIN TTOU URG WINCH
}

/// Send `sig` to `pid` (its default action, or nothing if it ignores or handles it: handlers
/// are not called yet). Returns false if there is no such process.
pub fn signal(pid: i32, sig: i32) -> bool {
    let Some(p) = get(pid) else { return false };
    if matches!(p.state, State::Zombie(_)) || sig == 0 {
        return true;
    }
    let disp = p.sigdisp.get(sig as usize).copied().unwrap_or(0);
    if (sig == 9 || disp == 0) && fatal_by_default(sig) {
        p.killed = sig;
        if p.state == State::Blocked {
            p.state = State::Runnable;
        }
    }
    true
}

pub fn signal_group(pgrp: i32, sig: i32) -> usize {
    let pids: Vec<i32> = procs().values().filter(|p| p.pgid == pgrp && !matches!(p.state, State::Zombie(_))).map(|p| p.pid).collect();
    for p in &pids {
        signal(*p, sig);
    }
    pids.len()
}

pub fn all_pids() -> Vec<i32> {
    procs().keys().copied().collect()
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

/// A page fault below the program's stack, within its 8 MiB: map more stack.
pub fn grow_stack(addr: u64) -> bool {
    if addr >= STACK_TOP || addr < STACK_TOP - STACK_SIZE {
        return false;
    }
    let p = current();
    map_range(p.pml4, addr & !(PAGE - 1), (addr & !(PAGE - 1)) + PAGE, P_WRITE).is_ok()
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
            _ => {
                // the stack grows on demand
                if !grow_stack(a) {
                    return false;
                }
            }
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
    let base = match rd16(f, 16) {
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
            PT_INTERP => return Err(ENOEXEC), // dynamically linked: not yet
            PT_PHDR => phdr = vaddr,
            PT_LOAD => {
                if first_load.is_none() {
                    first_load = Some(vaddr - off);
                }
                if off as usize + filesz as usize > f.len() || memsz < filesz {
                    return Err(ENOEXEC);
                }
                map_range(pml4, vaddr, vaddr + memsz, P_WRITE)?;
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
    let total: usize = path.len() + argv.iter().chain(envp).map(|s| s.len() + 1).sum::<usize>() + (argv.len() + envp.len()) * 8;
    if total as u64 > STACK_MAPPED / 2 {
        return Err(7); // E2BIG
    }
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
    let arg_ptrs: Vec<u64> = argv.iter().map(|a| push_bytes(&mut sp, a.as_bytes())).collect();
    let env_ptrs: Vec<u64> = envp.iter().map(|e| push_bytes(&mut sp, e.as_bytes())).collect();
    sp &= !15;
    sp -= 16;
    let random = sp;
    let (r1, r2) = (crate::syscall::random_u64(), crate::syscall::random_u64());
    // SAFETY: as above.
    unsafe {
        *(random as *mut u64) = r1;
        *((random + 8) as *mut u64) = r2;
    }
    let auxv: [(u64, u64); 16] = [
        (3, img.phdr),
        (4, img.phent),
        (5, img.phnum),
        (6, PAGE),
        (7, 0),
        (8, 0),
        (9, img.entry),
        (11, 0),
        (12, 0),
        (13, 0),
        (14, 0),
        (16, crate::syscall::hwcap()),
        (17, 100),
        (23, 0),
        (25, random),
        (31, execfn),
    ];
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

/// A script (#!): the interpreter and its argument, if any.
fn shebang(data: &[u8]) -> Option<(String, Option<String>)> {
    if !data.starts_with(b"#!") {
        return None;
    }
    let line = data[2..].split(|&b| b == b'\n').next()?;
    let line = core::str::from_utf8(line).ok()?.trim();
    let mut parts = line.splitn(2, |c: char| c == ' ' || c == '\t');
    let interp = parts.next()?.to_string();
    let arg = parts.next().map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
    Some((interp, arg))
}

/// Load `path` into a new address space (which becomes the current one): (address space,
/// entry point, stack pointer, end of the program for brk).
pub fn exec_image(path: &str, argv: &[String], envp: &[String], cwd: &str) -> Result<(u64, u64, u64, u64), i64> {
    let canon = vfs::resolve(cwd, path, true)?;
    let node = vfs::lookup(&canon).ok_or(2)?;
    if node.is_dir() {
        return Err(13); // EACCES
    }
    if node.mode() & 0o111 == 0 {
        return Err(13);
    }
    let owned;
    let data: &[u8] = match node.base_bytes() {
        Some(d) => d,
        None => {
            owned = node.bytes();
            &owned
        }
    };
    if let Some((interp, arg)) = shebang(data) {
        let mut args = alloc::vec![interp.clone()];
        args.extend(arg);
        args.push(path.to_string());
        args.extend(argv.iter().skip(1).cloned());
        return exec_image(&interp, &args, envp, cwd);
    }
    let pml4 = mem::new_address_space();
    mem::switch_to(pml4);
    let undo = |e: i64| {
        mem::switch_to(mem::kernel_pml4());
        mem::free_lower_half(pml4);
        mem::free_frame(pml4);
        e
    };
    let img = load_elf(pml4, data).map_err(undo)?;
    let sp = build_stack(pml4, &img, path, argv, envp).map_err(undo)?;
    let brk = (img.end + PAGE - 1) & !(PAGE - 1);
    Ok((pml4, img.entry, sp, brk))
}

/// execve: replace the current process's program. On success it continues at the returned
/// entry point and stack.
pub fn replace(path: &str, argv: &[String], envp: &[String]) -> Result<(u64, u64), i64> {
    let p = current();
    let old = p.pml4;
    let cwd = p.cwd.clone();
    let (pml4, entry, sp, brk) = match exec_image(path, argv, envp, &cwd) {
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
    p.name = path.rsplit('/').next().unwrap_or(path).to_string();
    p.fs_base = 0;
    // handlers do not survive exec; ignored signals stay ignored
    for d in p.sigdisp.iter_mut() {
        if *d > 1 {
            *d = 0;
        }
    }
    for f in p.fds.iter_mut() {
        if f.as_ref().is_some_and(|f| f.cloexec) {
            *f = None;
        }
    }
    release_vfork_parent(p);
    Ok((entry, sp))
}

pub fn stack_limit() -> u64 {
    STACK_SIZE
}

/// The console as standard input, output and error.
pub fn console_fds() -> Vec<Option<Fd>> {
    let f = Rc::new(RefCell::new(OpenFile::console()));
    alloc::vec![Some(Fd { file: f.clone(), cloexec: false }), Some(Fd { file: f.clone(), cloexec: false }), Some(Fd { file: f, cloexec: false })]
}
