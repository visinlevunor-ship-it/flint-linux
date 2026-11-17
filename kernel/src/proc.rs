//! Processes and threads: the task table, the scheduler, fork, clone (threads), execve, exit
//! and wait, signals (handlers too, with Linux's signal frame), futexes, and each process's
//! memory (ELF loading, the stack, brk, mmap).
//!
//! One CPU. Each task (a process or a thread) has its own kernel stack; a task in the kernel
//! is always in a system call or an interrupt, and the scheduler switches stacks
//! (context_switch) when one has to wait, exits, or used up its time slice (the timer, from
//! user mode only). A task never ends deep inside the kernel: a waiting system call returns
//! EINTR when a signal arrives, and the task acts on the signal on its way back to the program,
//! so nothing it holds is lost.

use crate::arch::{self, IntFrame, SyscallFrame};
use crate::mem::{self, P_USER, P_WRITE, PAGE};
use crate::syscall::Fd;
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
const INTERP_BASE: u64 = 0x7F80_0000_0000;
const KSTACK_WORDS: usize = 8192; // 64 KiB
const SLICE_TICKS: u64 = 5; // 50 ms

pub const EINTR: i64 = 4;
/// A waiting system call was interrupted by a signal; restarted if the handler asks for it.
pub const ERESTART: i64 = 512;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Runnable,
    Blocked,
    /// a process that ended; the wait status for its parent
    Zombie(i32),
    /// a thread that ended (freed by the scheduler)
    Dead,
    /// a process's first thread that ended while other threads still run
    Exited,
}

/// An address space and what goes with it; threads share one.
pub struct Mm {
    pub pml4: u64,
    pub brk_start: u64,
    pub brk: u64,
    pub mmap_top: u64,
}

impl Drop for Mm {
    fn drop(&mut self) {
        mem::switch_to(mem::kernel_pml4());
        mem::free_lower_half(self.pml4);
        mem::free_frame(self.pml4);
    }
}

#[derive(Clone, Copy, Default)]
pub struct SigAction {
    /// 0 SIG_DFL, 1 SIG_IGN, else the handler's address
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
const SA_SIGINFO: u64 = 4;

pub struct Proc {
    /// the task's id (a thread's tid)
    pub pid: i32,
    /// the process's id: the first thread's pid
    pub tgid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub sid: i32,
    pub name: String,
    pub mm: Rc<RefCell<Mm>>,
    kstack: Vec<u128>,
    ksp: u64,
    pub state: State,
    fs_base: u64,
    pub fds: Rc<RefCell<Vec<Option<Fd>>>>,
    pub cwd: Rc<RefCell<String>>,
    pub sig: Rc<RefCell<[SigAction; 65]>>,
    /// signals waiting to be delivered (bit n-1 for signal n)
    pub pending: u64,
    pub blocked: u64,
    pub altstack: (u64, u64),
    pub clear_tid: u64,
    /// a futex this task waits on (address), and whether a wake reached it
    pub futex: u64,
    pub futex_woken: bool,
    pub vfork_parent: i32,
    pub umask: u32,
    slice_end: u64,
    /// the exit status of the whole process (exit_group, or the first thread's exit)
    group_status: i32,
}

impl Proc {
    fn kstack_top(&self) -> u64 {
        self.kstack.as_ptr() as u64 + (self.kstack.len() * 16) as u64
    }
    pub fn pml4(&self) -> u64 {
        self.mm.borrow().pml4
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
    procs().values().filter(|p| p.pid == p.tgid).count()
}

fn alive(p: &Proc) -> bool {
    matches!(p.state, State::Runnable | State::Blocked)
}

// ---------------------------------------------------------------------------------------
// Scheduling

/// Mark every waiting task runnable: each checks again what it waits for.
pub fn wake_all() {
    for p in procs().values_mut() {
        if p.state == State::Blocked {
            p.state = State::Runnable;
        }
    }
}

/// Run something else: the next runnable task after the current one. Returns when the current
/// task is chosen again. With nothing to run, wait for an interrupt.
fn schedule() {
    loop {
        // SAFETY: as procs().
        let cur = unsafe { CUR };
        // free threads that ended, except the one whose stack this is
        let dead: Vec<i32> = procs().iter().filter(|(p, t)| t.state == State::Dead && **p != cur).map(|(p, _)| *p).collect();
        for p in dead {
            procs().remove(&p);
        }
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
                crate::dev::poll_devices();
                crate::persist::tick();
                arch::wait_for_interrupt();
            }
        }
    }
}

fn switch(from: i32, to: i32) {
    let next = procs().get_mut(&to).unwrap();
    let next_ksp = next.ksp;
    mem::switch_to(next.pml4());
    arch::set_kernel_stack(next.kstack_top());
    next.slice_end = crate::dev::ticks() + SLICE_TICKS;
    let fs = next.fs_base;
    // SAFETY: thread pointers belong to tasks; the old one is kept in its Proc.
    unsafe {
        if let Some(prev) = procs().get_mut(&from) {
            prev.fs_base = arch::rdmsr(arch::MSR_FS_BASE);
        }
        arch::wrmsr(arch::MSR_FS_BASE, fs);
        CUR = to;
    }
    let save: *mut u64 = match procs().get_mut(&from) {
        Some(p) => &mut p.ksp,
        None => core::ptr::addr_of_mut!(BOOT_KSP),
    };
    // SAFETY: both stacks are valid kernel stacks; `save` stays valid (the Proc is boxed).
    unsafe { arch::context_switch(save, next_ksp) };
}

/// Can a signal interrupt what the current task waits for?
fn signal_waiting(p: &Proc) -> bool {
    let sig = p.sig.borrow();
    let mut bits = p.pending & !p.blocked;
    while bits != 0 {
        let n = bits.trailing_zeros() as usize + 1;
        bits &= bits - 1;
        let h = sig[n].handler;
        if n == 9 || h > 1 || (h == 0 && fatal_by_default(n as i32)) {
            return true;
        }
    }
    false
}

/// Wait until woken (by an interrupt or another task). True: a signal arrived; the system
/// call should give up (return Err(ERESTART)).
#[must_use]
pub fn block() -> bool {
    if signal_waiting(current()) {
        return true;
    }
    current().state = State::Blocked;
    schedule();
    signal_waiting(current())
}

/// Give the CPU to someone else if anyone wants it.
pub fn yield_now() {
    current().state = State::Runnable;
    schedule();
}

/// The timer: ends the running program's time slice. `f`: the program's registers, when the
/// tick arrived while it ran.
pub fn tick(f: Option<&mut IntFrame>) {
    wake_all(); // sleepers check their clocks
    let Some(f) = f else { return };
    // SAFETY: as procs().
    if unsafe { CUR } == 0 {
        return;
    }
    if crate::dev::ticks() >= current().slice_end {
        yield_now();
    }
    let mut r = Regs::from_int(f);
    deliver_signals(&mut r, None);
    r.to_int(f);
}

// ---------------------------------------------------------------------------------------
// Creating tasks

fn new_kstack() -> Vec<u128> {
    alloc::vec![0u128; KSTACK_WORDS]
}

/// Set up a task's kernel stack so that its first run returns to the program with `frame`'s
/// registers (through the system call return path).
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

pub fn user_frame(rip: u64, rsp: u64) -> SyscallFrame {
    SyscallFrame {
        r15: 0, r14: 0, r13: 0, r12: 0, rbp: 0, rbx: 0, r11: 0, r10: 0, r9: 0, r8: 0, rdi: 0, rsi: 0, rdx: 0, rcx: 0, rax: 0,
        rip,
        cs: arch::UCODE as u64,
        rflags: 0x202,
        rsp,
        ss: arch::UDATA as u64,
    }
}

/// The first process (pid 1). Never returns: the boot stack is left for good.
pub fn start_init(path: &str, argv: &[String], envp: &[String], fds: Vec<Option<Fd>>) -> Result<(), i64> {
    let pid = alloc_pid();
    let (mm, entry, sp) = exec_image(path, argv, envp, "")?;
    let mut p = Box::new(Proc {
        pid,
        tgid: pid,
        ppid: 0,
        pgid: pid,
        sid: pid,
        name: argv.first().cloned().unwrap_or_default(),
        mm: Rc::new(RefCell::new(mm)),
        kstack: new_kstack(),
        ksp: 0,
        state: State::Runnable,
        fs_base: 0,
        fds: Rc::new(RefCell::new(fds)),
        cwd: Rc::new(RefCell::new(String::new())),
        sig: Rc::new(RefCell::new([SigAction::default(); 65])),
        pending: 0,
        blocked: 0,
        altstack: (0, 0),
        clear_tid: 0,
        futex: 0,
        futex_woken: false,
        vfork_parent: 0,
        umask: 0o022,
        slice_end: 0,
        group_status: 0,
    });
    prime(&mut p, &user_frame(entry, sp));
    procs().insert(pid, p);
    crate::tty::tty().fg_pgrp = pid;
    switch(0, pid);
    unreachable!("the boot stack is never resumed");
}

pub const CLONE_VM: u64 = 0x100;
pub const CLONE_FS: u64 = 0x200;
pub const CLONE_FILES: u64 = 0x400;
pub const CLONE_SIGHAND: u64 = 0x800;
pub const CLONE_VFORK: u64 = 0x4000;
pub const CLONE_THREAD: u64 = 0x10000;
pub const CLONE_SETTLS: u64 = 0x80000;
pub const CLONE_PARENT_SETTID: u64 = 0x100000;
pub const CLONE_CHILD_CLEARTID: u64 = 0x200000;
pub const CLONE_CHILD_SETTID: u64 = 0x1000000;

/// fork, vfork and clone: a new task continuing from the same system call with 0. `flags`
/// as clone's; `stack`, `tls`: the new task's stack and thread pointer, if given.
pub fn clone(frame: &SyscallFrame, flags: u64, stack: u64, ptid: u64, ctid: u64, tls: u64) -> Result<i32, i64> {
    let pid = alloc_pid();
    let parent = current();
    // SAFETY: the parent's thread pointer is the current one.
    let fs_base = if flags & CLONE_SETTLS != 0 { tls } else { unsafe { arch::rdmsr(arch::MSR_FS_BASE) } };
    let mm = if flags & CLONE_VM != 0 && flags & CLONE_VFORK == 0 {
        parent.mm.clone()
    } else {
        let m = parent.mm.borrow();
        Rc::new(RefCell::new(Mm { pml4: mem::clone_user(m.pml4), brk_start: m.brk_start, brk: m.brk, mmap_top: m.mmap_top }))
    };
    let thread = flags & CLONE_THREAD != 0;
    let mut child = Box::new(Proc {
        pid,
        tgid: if thread { parent.tgid } else { pid },
        ppid: if thread { parent.ppid } else { parent.tgid },
        pgid: parent.pgid,
        sid: parent.sid,
        name: parent.name.clone(),
        mm,
        kstack: new_kstack(),
        ksp: 0,
        state: State::Runnable,
        fs_base,
        fds: if flags & CLONE_FILES != 0 { parent.fds.clone() } else { Rc::new(RefCell::new(parent.fds.borrow().clone())) },
        cwd: if flags & CLONE_FS != 0 { parent.cwd.clone() } else { Rc::new(RefCell::new(parent.cwd.borrow().clone())) },
        sig: if flags & CLONE_SIGHAND != 0 { parent.sig.clone() } else { Rc::new(RefCell::new(*parent.sig.borrow())) },
        pending: 0,
        blocked: parent.blocked,
        altstack: if thread { (0, 0) } else { parent.altstack },
        clear_tid: if flags & CLONE_CHILD_CLEARTID != 0 { ctid } else { 0 },
        futex: 0,
        futex_woken: false,
        vfork_parent: if flags & CLONE_VFORK != 0 { parent.pid } else { 0 },
        umask: parent.umask,
        slice_end: 0,
        group_status: 0,
    });
    if flags & CLONE_PARENT_SETTID != 0 && ptid != 0 {
        if let Ok(s) = user_slice(ptid, 4) {
            s.copy_from_slice(&pid.to_le_bytes());
        }
    }
    let mut f = *frame;
    f.rax = 0;
    if stack != 0 {
        f.rsp = stack;
    }
    prime(&mut child, &f);
    let child_pml4 = child.pml4();
    procs().insert(pid, child);
    if flags & CLONE_CHILD_SETTID != 0 && ctid != 0 {
        // in the child's memory (the same as ours for a thread)
        let cur = current().pml4();
        mem::switch_to(child_pml4);
        if let Some(e) = mem::lookup(child_pml4, ctid & !(PAGE - 1)) {
            let dst = mem::phys_to_virt((e & 0x000F_FFFF_FFFF_F000) + (ctid & (PAGE - 1)));
            // SAFETY: a mapped user page of the child, through the physical mapping.
            unsafe { core::ptr::copy_nonoverlapping(pid.to_le_bytes().as_ptr(), dst, 4) };
        }
        mem::switch_to(cur);
    }
    if flags & CLONE_VFORK != 0 {
        // the parent waits until the child has exec'd or exited
        while get(pid).is_some_and(|c| c.vfork_parent != 0 && alive(c)) {
            let _ = block();
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

// ---------------------------------------------------------------------------------------
// Ending tasks and processes

/// The current task ends. `group`: the whole process ends (exit_group, a fatal signal) with
/// `status`; otherwise only this thread.
fn do_exit(status: i32, group: bool) -> ! {
    let me = pid();
    let tgid = current().tgid;
    if group {
        // the other threads end too: they unwind out of what they wait for and exit
        for t in procs().values_mut() {
            if t.tgid == tgid && t.pid != me && alive(t) {
                t.pending |= 1 << 8; // SIGKILL
                t.state = State::Runnable;
            }
        }
        if let Some(leader) = get(tgid) {
            leader.group_status = status;
        }
    } else if me == tgid {
        current().group_status = status;
    }
    {
        let p = current();
        // CLONE_CHILD_CLEARTID: tell whoever joins this thread
        if p.clear_tid != 0 {
            if let Ok(s) = user_slice(p.clear_tid, 4) {
                s.copy_from_slice(&0u32.to_le_bytes());
            }
            futex_wake(p.clear_tid, 1);
        }
        release_vfork_parent(p);
    }
    let others_alive = procs().values().any(|t| t.tgid == tgid && t.pid != me && alive(t));
    {
        let p = current();
        // drop this task's share of the files and memory (the last one frees them)
        p.fds = Rc::new(RefCell::new(Vec::new()));
        mem::switch_to(mem::kernel_pml4());
        p.mm = kernel_mm();
    }
    if others_alive {
        current().state = if me == tgid { State::Exited } else { State::Dead };
    } else {
        // the last thread: the process is over
        let st = get(tgid).map(|l| l.group_status).unwrap_or(status);
        let ppid = get(tgid).map(|l| l.ppid).unwrap_or(1);
        if me != tgid {
            current().state = State::Dead;
        }
        if let Some(l) = get(tgid) {
            l.state = State::Zombie(st);
        }
        // children go to pid 1
        for c in procs().values_mut() {
            if c.ppid == tgid {
                c.ppid = 1;
            }
        }
        if tgid == 1 {
            crate::persist::save();
            crate::kprintln!("\n[kernel] the first program ended (status {:#x}); nothing left to run. The machine can be switched off.", st);
            arch::halt();
        }
        signal(ppid, 17); // SIGCHLD
    }
    wake_all(); // the parent may be in wait
    schedule();
    unreachable!("an ended task never runs again");
}

/// An address space with nothing in it, for tasks that have ended.
fn kernel_mm() -> Rc<RefCell<Mm>> {
    static mut KMM: Option<Rc<RefCell<Mm>>> = None;
    // SAFETY: one CPU. This Mm is never dropped (a clone is kept here).
    unsafe {
        (*core::ptr::addr_of_mut!(KMM))
            .get_or_insert_with(|| Rc::new(RefCell::new(Mm { pml4: mem::new_address_space(), brk_start: 0, brk: 0, mmap_top: 0 })))
            .clone()
    }
}

pub fn exit_thread(code: i64) -> ! {
    do_exit(((code as i32) & 0xFF) << 8, false)
}

pub fn exit_current(code: i64) -> ! {
    do_exit(((code as i32) & 0xFF) << 8, true)
}

pub fn exit_signal(sig: i32) -> ! {
    do_exit(sig & 0x7F, true)
}

/// wait4: a child process that has ended (pid > 0: that one; -1: any; 0: same group; < -1:
/// group -pid). Returns (pid, status), or None with WNOHANG and nothing ended yet.
pub fn wait(pid: i32, nohang: bool) -> Result<Option<(i32, i32)>, i64> {
    loop {
        let me = current();
        let (my_pid, my_pgid) = (me.tgid, me.pgid);
        let mut have_child = false;
        let mut found = None;
        for (cp, c) in procs().iter() {
            if c.ppid != my_pid || c.pid != c.tgid {
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
        if block() {
            return Err(ERESTART);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Signals

/// Does `sig`'s default action end the process?
fn fatal_by_default(sig: i32) -> bool {
    !matches!(sig, 17 | 18 | 19 | 20 | 21 | 22 | 23 | 28) // CHLD CONT STOP TSTP TTIN TTOU URG WINCH
}

/// Send `sig` to process (or thread) `pid`. False if there is no such task.
pub fn signal(pid: i32, sig: i32) -> bool {
    if sig == 0 {
        return get(pid).is_some_and(|p| alive(p) || p.state == State::Exited);
    }
    // to a process: the first of its threads that does not block it
    let target = {
        let Some(p) = get(pid) else { return false };
        if p.pid == p.tgid {
            let tg = p.tgid;
            procs().values().find(|t| t.tgid == tg && alive(t) && t.blocked & (1 << (sig - 1)) == 0).or_else(|| procs().values().find(|t| t.tgid == tg && alive(t))).map(|t| t.pid)
        } else if alive(p) {
            Some(p.pid)
        } else {
            None
        }
    };
    let Some(t) = target.and_then(get) else { return get(pid).is_some() };
    let h = t.sig.borrow()[sig as usize].handler;
    if sig != 9 && (h == 1 || (h == 0 && !fatal_by_default(sig))) {
        return true; // ignored
    }
    t.pending |= 1 << (sig - 1);
    if t.state == State::Blocked {
        t.state = State::Runnable;
    }
    true
}

pub fn signal_group(pgrp: i32, sig: i32) -> usize {
    let pids: Vec<i32> = procs().values().filter(|p| p.pgid == pgrp && p.pid == p.tgid && alive(p)).map(|p| p.pid).collect();
    for p in &pids {
        signal(*p, sig);
    }
    pids.len()
}

pub fn all_tasks_of(tgid: i32) -> Vec<i32> {
    procs().values().filter(|t| t.tgid == tgid).map(|t| t.pid).collect()
}

pub fn all_pids() -> Vec<i32> {
    procs().values().filter(|p| p.pid == p.tgid && alive(p)).map(|p| p.pid).collect()
}

/// A program's registers, from either kind of kernel entry.
pub struct Regs {
    pub r: [u64; 16], // rax rbx rcx rdx rsi rdi rbp rsp r8 .. r15
    pub rip: u64,
    pub rflags: u64,
}

impl Regs {
    pub fn from_sys(f: &SyscallFrame) -> Regs {
        Regs { r: [f.rax, f.rbx, f.rcx, f.rdx, f.rsi, f.rdi, f.rbp, f.rsp, f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15], rip: f.rip, rflags: f.rflags }
    }
    pub fn to_sys(&self, f: &mut SyscallFrame) {
        let r = &self.r;
        (f.rax, f.rbx, f.rcx, f.rdx, f.rsi, f.rdi, f.rbp, f.rsp) = (r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]);
        (f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15) = (r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15]);
        f.rip = self.rip;
        f.rflags = self.rflags;
        f.cs = arch::UCODE as u64;
        f.ss = arch::UDATA as u64;
    }
    pub fn from_int(f: &IntFrame) -> Regs {
        Regs { r: [f.rax, f.rbx, f.rcx, f.rdx, f.rsi, f.rdi, f.rbp, f.rsp, f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15], rip: f.rip, rflags: f.rflags }
    }
    pub fn to_int(&self, f: &mut IntFrame) {
        let r = &self.r;
        (f.rax, f.rbx, f.rcx, f.rdx, f.rsi, f.rdi, f.rbp, f.rsp) = (r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]);
        (f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15) = (r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15]);
        f.rip = self.rip;
        f.rflags = self.rflags;
    }
}

const FRAME_UC: u64 = 8; // after the return address
const UC_MCONTEXT: u64 = 40;
const UC_SIGMASK: u64 = 40 + 256;
const FRAME_INFO: u64 = 8 + 304;
const FRAME_SIZE: u64 = 8 + 304 + 128;

/// On the way back to the program: act on pending signals. A handler gets Linux's signal
/// frame on the program's stack. `restart`: the system call number when it was interrupted
/// (Err(ERESTART)), to run it again if the handler has SA_RESTART.
pub fn deliver_signals(r: &mut Regs, restart: Option<u64>) {
    let p = current();
    let mut bits = p.pending & !p.blocked;
    if p.pending & (1 << 8) != 0 {
        bits |= 1 << 8; // SIGKILL cannot be blocked
    }
    if bits == 0 {
        if restart.is_some() {
            r.r[0] = (-EINTR) as u64;
        }
        return;
    }
    let sig = bits.trailing_zeros() as i32 + 1;
    p.pending &= !(1u64 << (sig - 1));
    let act = p.sig.borrow()[sig as usize];
    if sig == 9 || act.handler == 0 {
        if fatal_by_default(sig) {
            exit_signal(sig);
        }
        if restart.is_some() {
            r.r[0] = (-EINTR) as u64;
        }
        return;
    }
    if act.handler == 1 {
        if restart.is_some() {
            r.r[0] = (-EINTR) as u64;
        }
        return;
    }
    // the interrupted system call: again after the handler, or EINTR
    if let Some(nr) = restart {
        if act.flags & SA_RESTART != 0 {
            r.r[0] = nr;
            r.rip -= 2; // the syscall instruction
        } else {
            r.r[0] = (-EINTR) as u64;
        }
    }
    if push_frame(p, r, sig, &act).is_err() {
        exit_signal(11);
    }
    if act.flags & SA_NODEFER == 0 {
        p.blocked |= 1 << (sig - 1);
    }
    p.blocked |= act.mask;
    p.blocked &= !((1 << 8) | (1 << 18)); // never SIGKILL, SIGSTOP
    if act.flags & SA_RESETHAND != 0 {
        p.sig.borrow_mut()[sig as usize].handler = 0;
    }
}

fn push_frame(p: &mut Proc, r: &mut Regs, sig: i32, act: &SigAction) -> Result<(), i64> {
    let mut sp = r.r[7];
    let (alt_sp, alt_size) = p.altstack;
    if act.flags & SA_ONSTACK != 0 && alt_size != 0 && !(sp > alt_sp && sp <= alt_sp + alt_size) {
        sp = alt_sp + alt_size;
    } else {
        sp -= 128; // the red zone
    }
    // the FPU/SSE state
    sp -= 512;
    sp &= !63;
    let fx = sp;
    user_slice(fx, 512)?;
    // SAFETY: fx is a mapped, 64-byte aligned user buffer of 512 bytes in this address space;
    // the kernel itself never touches the FPU, so it still holds the program's state.
    unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) fx, options(nostack)) };
    sp -= FRAME_SIZE;
    sp = (sp & !15) - 8; // the handler starts as if called: (rsp + 8) % 16 == 0
    let frame = user_slice(sp, FRAME_SIZE)?;
    frame.fill(0);
    let put = |f: &mut [u8], off: u64, v: u64| f[off as usize..off as usize + 8].copy_from_slice(&v.to_le_bytes());
    put(frame, 0, act.restorer);
    // ucontext: flags, link, stack, mcontext, sigmask
    put(frame, FRAME_UC + 16, alt_sp);
    put(frame, FRAME_UC + 32, alt_size);
    let mc = FRAME_UC + UC_MCONTEXT;
    let regs = &r.r;
    // sigcontext order: r8..r15, rdi, rsi, rbp, rbx, rdx, rax, rcx, rsp, rip, eflags
    let order = [regs[8], regs[9], regs[10], regs[11], regs[12], regs[13], regs[14], regs[15], regs[5], regs[4], regs[6], regs[1], regs[3], regs[0], regs[2], regs[7], r.rip, r.rflags];
    for (i, v) in order.iter().enumerate() {
        put(frame, mc + i as u64 * 8, *v);
    }
    put(frame, mc + 144, (arch::UCODE as u64) | ((arch::UDATA as u64) << 48));
    put(frame, mc + 184, fx); // fpstate
    put(frame, FRAME_UC + UC_SIGMASK, p.blocked);
    // siginfo: signo, errno, code (SI_USER = 0)
    frame[FRAME_INFO as usize..FRAME_INFO as usize + 4].copy_from_slice(&sig.to_le_bytes());
    r.r[7] = sp;
    r.rip = act.handler;
    r.r[5] = sig as u64; // rdi
    r.r[4] = sp + FRAME_INFO; // rsi
    r.r[3] = sp + FRAME_UC; // rdx
    r.r[0] = 0;
    r.rflags &= !0x400; // DF
    let _ = SA_SIGINFO;
    Ok(())
}

/// rt_sigreturn: back to where the handler interrupted the program.
pub fn sigreturn(r: &mut Regs) -> Result<(), i64> {
    let frame = r.r[7] - 8; // the handler's ret took the return address
    let f = user_slice(frame, FRAME_SIZE)?.to_vec();
    let get = |off: u64| u64::from_le_bytes(f[off as usize..off as usize + 8].try_into().unwrap());
    let mc = FRAME_UC + UC_MCONTEXT;
    let v: Vec<u64> = (0..18).map(|i| get(mc + i * 8)).collect();
    // back from sigcontext order into ours
    r.r = [v[13], v[11], v[14], v[12], v[9], v[8], v[10], v[15], v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]];
    r.rip = v[16];
    r.rflags = (v[17] & 0xCD5) | 0x202; // the flags a program may change, interrupts on
    if r.rip >= USER_TOP {
        return Err(14);
    }
    let fx = get(mc + 184);
    if fx != 0 && fx & 15 == 0 && user_ok(fx, 512) {
        // SAFETY: a mapped user buffer; fxrstor checks its contents (MXCSR) itself.
        let mxcsr = u32::from_le_bytes(user_slice(fx + 24, 4)?.try_into().unwrap());
        if mxcsr & 0xFFFF_0000 == 0 {
            unsafe { core::arch::asm!("fxrstor64 [{}]", in(reg) fx, options(nostack)) };
        }
    }
    let p = current();
    p.blocked = get(FRAME_UC + UC_SIGMASK) & !((1 << 8) | (1 << 18));
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Futexes

/// FUTEX_WAIT: sleep while *addr == val, until a wake for addr or `deadline_ns`.
pub fn futex_wait(addr: u64, val: u32, deadline_ns: Option<u64>) -> Result<i64, i64> {
    if addr & 3 != 0 {
        return Err(22);
    }
    let cur = u32::from_le_bytes(user_slice(addr, 4)?.try_into().unwrap());
    if cur != val {
        return Err(11); // EAGAIN
    }
    let p = current();
    p.futex = addr;
    p.futex_woken = false;
    loop {
        if current().futex_woken {
            current().futex = 0;
            return Ok(0);
        }
        if deadline_ns.is_some_and(|d| crate::syscall::now_ns() >= d) {
            current().futex = 0;
            return Err(110); // ETIMEDOUT
        }
        if block() {
            current().futex = 0;
            return Err(ERESTART);
        }
    }
}

/// FUTEX_WAKE: wake up to `n` tasks of this address space waiting on addr.
pub fn futex_wake(addr: u64, n: u32) -> i64 {
    let mm = Rc::as_ptr(&current().mm);
    let mut woken = 0;
    for p in procs().values_mut() {
        if woken >= n as i64 {
            break;
        }
        if p.futex == addr && !p.futex_woken && Rc::as_ptr(&p.mm) == mm {
            p.futex_woken = true;
            if p.state == State::Blocked {
                p.state = State::Runnable;
            }
            woken += 1;
        }
    }
    woken
}

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
    let pml4 = current().pml4();
    map_range(pml4, addr & !(PAGE - 1), (addr & !(PAGE - 1)) + PAGE, P_WRITE).is_ok()
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
    let pml4 = current().pml4();
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
    base: u64,
    interp: Option<String>,
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

/// Map an ELF executable or shared object. `dyn_base`: where a position-independent one goes.
fn load_elf(pml4: u64, f: &[u8], dyn_base: u64) -> Result<Image, i64> {
    if f.len() < 64 || &f[..4] != b"\x7fELF" || f[4] != 2 || f[5] != 1 || rd16(f, 18) != 0x3E {
        return Err(ENOEXEC);
    }
    let base = match rd16(f, 16) {
        2 => 0,
        3 => dyn_base,
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
    let mut interp = None;
    for i in 0..phnum {
        let h = &f[phoff + i * phent..];
        let kind = rd32(h, 0) as u32;
        let off = rd64(h, 8);
        let vaddr = base + rd64(h, 16);
        let filesz = rd64(h, 32);
        let memsz = rd64(h, 40);
        match kind {
            PT_INTERP => {
                let path = f.get(off as usize..(off + filesz) as usize).ok_or(ENOEXEC)?;
                let path = path.split(|&b| b == 0).next().unwrap_or(&[]);
                interp = Some(String::from_utf8_lossy(path).into_owned());
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
    Ok(Image { entry: base + rd64(f, 24), phdr, phent: phent as u64, phnum: phnum as u64, end, base, interp })
}

/// Lay out the stack Linux gives a new program: argc, argv, envp, auxv, then the strings.
fn build_stack(pml4: u64, img: &Image, interp_base: u64, path: &str, argv: &[String], envp: &[String]) -> Result<u64, i64> {
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
    let platform = push_bytes(&mut sp, b"x86_64");
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
    let auxv: [(u64, u64); 17] = [
        (3, img.phdr),
        (4, img.phent),
        (5, img.phnum),
        (6, PAGE),
        (7, interp_base), // AT_BASE: where the interpreter is
        (8, 0),
        (9, img.entry),
        (11, 0),
        (12, 0),
        (13, 0),
        (14, 0),
        (15, platform),
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

fn file_bytes(cwd: &str, path: &str) -> Result<(vfs::Node, Vec<u8>), i64> {
    let canon = vfs::resolve(cwd, path, true)?;
    let node = vfs::lookup(&canon).ok_or(2)?;
    if node.is_dir() {
        return Err(13); // EACCES
    }
    let data = node.bytes();
    Ok((node, data))
}

/// Load `path` into a new address space (which becomes the current one): (its Mm, the entry
/// point, the stack pointer). A dynamically linked program gets its interpreter (the dynamic
/// loader named in PT_INTERP, flint-glibc's for glibc programs) loaded too, and starts there.
pub fn exec_image(path: &str, argv: &[String], envp: &[String], cwd: &str) -> Result<(Mm, u64, u64), i64> {
    let (node, data) = file_bytes(cwd, path)?;
    if node.mode() & 0o111 == 0 {
        return Err(13);
    }
    if let Some((interp, arg)) = shebang(&data) {
        let mut args = alloc::vec![interp.clone()];
        args.extend(arg);
        args.push(path.to_string());
        args.extend(argv.iter().skip(1).cloned());
        return exec_image(&interp, &args, envp, cwd);
    }
    let pml4 = mem::new_address_space();
    mem::switch_to(pml4);
    let mut mm = Mm { pml4, brk_start: 0, brk: 0, mmap_top: MMAP_TOP };
    // on an error, dropping `mm` frees the half-built address space
    let img = load_elf(pml4, &data, PIE_BASE)?;
    let mut entry = img.entry;
    let mut interp_base = 0;
    if let Some(ip) = &img.interp {
        let (_, idata) = file_bytes("", ip)?;
        let i = load_elf(pml4, &idata, INTERP_BASE)?;
        if i.interp.is_some() {
            return Err(ENOEXEC);
        }
        entry = i.entry;
        interp_base = i.base;
    }
    let sp = build_stack(pml4, &img, interp_base, path, argv, envp)?;
    let brk = (img.end + PAGE - 1) & !(PAGE - 1);
    mm.brk_start = brk;
    mm.brk = brk;
    let _ = img.base;
    Ok((mm, entry, sp))
}

/// execve: replace the current process's program. On success it continues at the returned
/// entry point and stack.
pub fn replace(path: &str, argv: &[String], envp: &[String]) -> Result<(u64, u64), i64> {
    let cwd = current().cwd.borrow().clone();
    let old_pml4 = current().pml4();
    let (mm, entry, sp) = match exec_image(path, argv, envp, &cwd) {
        Ok(v) => v,
        Err(e) => {
            mem::switch_to(old_pml4);
            return Err(e);
        }
    };
    let me = pid();
    let tgid = current().tgid;
    // the other threads of this process end
    for t in procs().values_mut() {
        if t.tgid == tgid && t.pid != me && alive(t) {
            t.pending |= 1 << 8;
            t.state = State::Runnable;
        }
    }
    let p = current();
    p.mm = Rc::new(RefCell::new(mm)); // drops ours if no thread shares it
    mem::switch_to(p.pml4());
    p.name = path.rsplit('/').next().unwrap_or(path).to_string();
    p.fs_base = 0;
    // handlers do not survive exec; ignored signals stay ignored; a private table from now on
    let mut acts = *p.sig.borrow();
    for a in acts.iter_mut() {
        if a.handler > 1 {
            *a = SigAction::default();
        }
    }
    p.sig = Rc::new(RefCell::new(acts));
    p.altstack = (0, 0);
    let fds: Vec<Option<Fd>> = p.fds.borrow().iter().map(|f| f.clone().filter(|f| !f.cloexec)).collect();
    p.fds = Rc::new(RefCell::new(fds));
    release_vfork_parent(p);
    Ok((entry, sp))
}

pub fn stack_limit() -> u64 {
    STACK_SIZE
}

/// The console as standard input, output and error.
pub fn console_fds() -> Vec<Option<Fd>> {
    let f = Rc::new(RefCell::new(crate::syscall::OpenFile::console()));
    alloc::vec![Some(Fd { file: f.clone(), cloexec: false }), Some(Fd { file: f.clone(), cloexec: false }), Some(Fd { file: f, cloexec: false })]
}
