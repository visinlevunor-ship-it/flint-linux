//! x86-64: segments, the interrupt table, the `syscall` entry, and entering and leaving
//! user mode.
//!
//! One CPU and interrupts stay off: a program runs until it makes a system call or faults,
//! and the kernel handles that to completion before it continues.

use crate::kprintln;
use core::arch::{asm, global_asm};
use core::ptr::addr_of;

// GDT: null, kernel code, kernel data, user data, user code, TSS (two slots). The order is
// the one `sysret` needs: user SS = STAR[63:48] + 8, user CS = STAR[63:48] + 16.
pub const KCODE: u16 = 0x08;
pub const KDATA: u16 = 0x10;
pub const UDATA: u16 = 0x18 | 3;
pub const UCODE: u16 = 0x20 | 3;
const TSS_SEL: u16 = 0x28;

#[repr(C, packed)]
struct Tss {
    _r0: u32,
    rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    _r1: u64,
    ist: [u64; 7],
    _r2: u64,
    _r3: u16,
    iomap: u16,
}

static mut TSS: Tss = Tss { _r0: 0, rsp0: 0, rsp1: 0, rsp2: 0, _r1: 0, ist: [0; 7], _r2: 0, _r3: 0, iomap: 104 };
static mut GDT: [u64; 7] = [
    0,
    0x00AF_9A00_0000_FFFF, // kernel code: 64-bit, DPL 0
    0x00CF_9200_0000_FFFF, // kernel data
    0x00CF_F200_0000_FFFF, // user data: DPL 3
    0x00AF_FA00_0000_FFFF, // user code: 64-bit, DPL 3
    0,
    0,
];

#[repr(C, align(16))]
pub struct Stack<const N: usize>(pub [u8; N]);

/// The kernel's own stack (start-up and everything between programs)
pub static mut KSTACK: Stack<{ 256 * 1024 }> = Stack([0; 256 * 1024]);
/// System calls run on this one
static mut SYSCALL_STACK: Stack<{ 256 * 1024 }> = Stack([0; 256 * 1024]);
/// Exceptions and interrupts from user mode arrive on this one (TSS.RSP0)
static mut TRAP_STACK: Stack<{ 64 * 1024 }> = Stack([0; 64 * 1024]);

pub fn stack_top<const N: usize>(s: *const Stack<N>) -> u64 {
    s as u64 + N as u64
}

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

pub unsafe fn init_gdt() {
    let tss = addr_of!(TSS) as u64;
    TSS.rsp0 = stack_top(addr_of!(TRAP_STACK));
    let limit = (core::mem::size_of::<Tss>() - 1) as u64;
    GDT[5] = (limit & 0xFFFF) | ((tss & 0xFF_FFFF) << 16) | (0x89 << 40) | (((limit >> 16) & 0xF) << 48) | (((tss >> 24) & 0xFF) << 56);
    GDT[6] = tss >> 32;
    let p = Pointer { limit: (core::mem::size_of_val(&*addr_of!(GDT)) - 1) as u16, base: addr_of!(GDT) as u64 };
    asm!(
        "lgdt [{p}]",
        "push {kcode}",
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",
        "2:",
        "mov ax, {kdata:x}",
        "mov ds, ax",
        "mov es, ax",
        "mov ss, ax",
        "xor eax, eax",
        "mov fs, ax",
        "mov gs, ax",
        "mov ax, {tss:x}",
        "ltr ax",
        p = in(reg) &p,
        kcode = in(reg) KCODE as u64,
        kdata = in(reg) KDATA as u64,
        tss = in(reg) TSS_SEL as u64,
        tmp = lateout(reg) _,
        out("rax") _,
    );
}

// ---------------------------------------------------------------------------------------
// Interrupts: only the CPU's exceptions for now.

#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    off_lo: u16,
    sel: u16,
    ist: u8,
    attr: u8,
    off_mid: u16,
    off_hi: u32,
    _r: u32,
}

static mut IDT: [Gate; 48] = [Gate { off_lo: 0, sel: 0, ist: 0, attr: 0, off_mid: 0, off_hi: 0, _r: 0 }; 48];

/// Every register at an exception or interrupt, from the lowest address up: what the stub
/// pushes, then what the CPU pushed.
#[repr(C)]
pub struct IntFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// One stub per vector: the CPU pushes an error code for some exceptions; the others push 0.
// 0-31 are the CPU's exceptions, 32-47 the legacy interrupt controller's lines.
global_asm!(
    ".macro ISR n, err",
    ".global isr\\n",
    "isr\\n:",
    ".if \\err == 0",
    "push 0",
    ".endif",
    "push \\n",
    "jmp isr_common",
    ".endm",
    "ISR 0,0", "ISR 1,0", "ISR 2,0", "ISR 3,0", "ISR 4,0", "ISR 5,0", "ISR 6,0", "ISR 7,0",
    "ISR 8,1", "ISR 9,0", "ISR 10,1", "ISR 11,1", "ISR 12,1", "ISR 13,1", "ISR 14,1", "ISR 15,0",
    "ISR 16,0", "ISR 17,1", "ISR 18,0", "ISR 19,0", "ISR 20,0", "ISR 21,1", "ISR 22,0", "ISR 23,0",
    "ISR 24,0", "ISR 25,0", "ISR 26,0", "ISR 27,0", "ISR 28,0", "ISR 29,1", "ISR 30,1", "ISR 31,0",
    "ISR 32,0", "ISR 33,0", "ISR 34,0", "ISR 35,0", "ISR 36,0", "ISR 37,0", "ISR 38,0", "ISR 39,0",
    "ISR 40,0", "ISR 41,0", "ISR 42,0", "ISR 43,0", "ISR 44,0", "ISR 45,0", "ISR 46,0", "ISR 47,0",
    "isr_common:",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "mov rbp, rsp",
    "and rsp, -16",
    "call {handler}",
    "mov rsp, rbp",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "add rsp, 16",
    "iretq",
    handler = sym interrupt_handler,
);

macro_rules! isr_table {
    ($($n:ident),*) => {
        extern "C" { $(fn $n();)* }
        const ISRS: [unsafe extern "C" fn(); 48] = [$($n),*];
    };
}
isr_table!(isr0, isr1, isr2, isr3, isr4, isr5, isr6, isr7, isr8, isr9, isr10, isr11, isr12, isr13, isr14, isr15,
    isr16, isr17, isr18, isr19, isr20, isr21, isr22, isr23, isr24, isr25, isr26, isr27, isr28, isr29, isr30, isr31,
    isr32, isr33, isr34, isr35, isr36, isr37, isr38, isr39, isr40, isr41, isr42, isr43, isr44, isr45, isr46, isr47);

const NAMES: [&str; 32] = [
    "divide error", "debug", "NMI", "breakpoint", "overflow", "bound range", "invalid opcode", "no FPU",
    "double fault", "coprocessor", "invalid TSS", "segment not present", "stack fault", "general protection",
    "page fault", "reserved", "x87 error", "alignment check", "machine check", "SIMD error", "virtualization",
    "control protection", "22", "23", "24", "25", "26", "27", "hypervisor", "VMM communication", "security", "31",
];

extern "sysv64" fn interrupt_handler(f: &mut IntFrame) {
    let from_user = f.cs & 3 == 3;
    if f.vector >= 32 {
        crate::dev::irq((f.vector - 32) as u8, if from_user { Some(f) } else { None });
        return;
    }
    let cr2: u64;
    // SAFETY: reading CR2 has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
    let name = NAMES[f.vector as usize];
    if from_user {
        // the program's own stack: grow it on demand, as Linux does
        if f.vector == 14 && crate::proc::grow_stack(cr2) {
            return;
        }
        // a program's fault ends the program, as the matching signal would on Linux
        if f.vector == 14 {
            kprintln!("[kernel] {} (pid {}): page fault at {:#x} (ip {:#x}); killed", crate::proc::name(), crate::proc::pid(), cr2, f.rip);
        } else {
            kprintln!("[kernel] {} (pid {}): {} (ip {:#x}); killed", crate::proc::name(), crate::proc::pid(), name, f.rip);
        }
        let sig = match f.vector {
            0 => 8,       // SIGFPE
            6 => 4,       // SIGILL
            3 => 5,       // SIGTRAP
            _ => 11,      // SIGSEGV
        };
        crate::proc::exit_signal(sig);
    }
    kprintln!("\n[kernel] PANIC: {} in the kernel at {:#x} (error {:#x}, cr2 {:#x})", name, f.rip, f.error, cr2);
    halt()
}

pub unsafe fn init_idt() {
    for (i, h) in ISRS.iter().enumerate() {
        let a = *h as *const () as u64;
        IDT[i] = Gate { off_lo: a as u16, sel: KCODE, ist: 0, attr: 0x8E, off_mid: (a >> 16) as u16, off_hi: (a >> 32) as u32, _r: 0 };
    }
    let p = Pointer { limit: (core::mem::size_of_val(&*addr_of!(IDT)) - 1) as u16, base: addr_of!(IDT) as u64 };
    asm!("lidt [{}]", in(reg) &p, options(nostack));
}

// ---------------------------------------------------------------------------------------
// System calls

const MSR_EFER: u32 = 0xC000_0080;
const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_FMASK: u32 = 0xC000_0084;
pub const MSR_FS_BASE: u32 = 0xC000_0100;

pub unsafe fn wrmsr(msr: u32, v: u64) {
    asm!("wrmsr", in("ecx") msr, in("eax") v as u32, in("edx") (v >> 32) as u32, options(nostack, preserves_flags));
}

pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nostack, preserves_flags));
    (hi as u64) << 32 | lo as u64
}

/// The registers of a program at its system call, from the lowest address up. The top five
/// are an iretq frame, so returning restores every register (needed after signal handlers).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SyscallFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[no_mangle]
static mut SYSCALL_USER_RSP: u64 = 0;
#[no_mangle]
static mut SYSCALL_STACK_TOP: u64 = 0;

global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "mov [rip + SYSCALL_USER_RSP], rsp",
    "mov rsp, [rip + SYSCALL_STACK_TOP]",
    "push {udata}",
    "push qword ptr [rip + SYSCALL_USER_RSP]",
    "push r11",
    "push {ucode}",
    "push rcx",
    "push rax",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov rdi, rsp",
    "call {dispatch}",
    // back to the program; a new process (fork's child, the first program) starts here too,
    // with its SyscallFrame at rsp
    ".global syscall_return",
    "syscall_return:",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rax",
    "iretq",
    dispatch = sym crate::syscall::dispatch,
    udata = const UDATA,
    ucode = const UCODE,
);

extern "C" {
    fn syscall_entry();
    fn syscall_return();
}

pub fn syscall_return_addr() -> u64 {
    syscall_return as *const () as u64
}

pub unsafe fn init_syscall() {
    SYSCALL_STACK_TOP = stack_top(addr_of!(SYSCALL_STACK));
    wrmsr(MSR_EFER, rdmsr(MSR_EFER) | 1); // SCE
    wrmsr(MSR_STAR, (0x10u64 << 48) | ((KCODE as u64) << 32));
    wrmsr(MSR_LSTAR, syscall_entry as *const () as u64);
    wrmsr(MSR_FMASK, 0x4_0700); // IF, TF, DF, AC cleared on entry
    // SSE for programs (the kernel itself is built without it)
    let mut cr0: u64;
    asm!("mov {}, cr0", out(reg) cr0);
    cr0 = (cr0 & !(1 << 2)) | (1 << 1); // EM off, MP on
    asm!("mov cr0, {}", in(reg) cr0);
    let mut cr4: u64;
    asm!("mov {}, cr4", out(reg) cr4);
    cr4 |= (1 << 9) | (1 << 10); // OSFXSR, OSXMMEXCPT
    cr4 &= !(1 << 21); // no SMAP: the kernel reads programs' buffers directly
    asm!("mov cr4, {}", in(reg) cr4);
}

/// The kernel stack for the process about to run: system calls and interrupts from user
/// mode arrive on it.
pub fn set_kernel_stack(top: u64) {
    // SAFETY: one CPU; read only on the next entry from user mode.
    unsafe {
        SYSCALL_STACK_TOP = top;
        TSS.rsp0 = top;
    }
}

// ---------------------------------------------------------------------------------------
// Switching between processes

global_asm!(
    // context_switch(save: *mut u64, next: u64): save this kernel stack's place, continue on
    // `next`'s. Returns when something switches back to `save`.
    ".global context_switch",
    "context_switch:",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
);

// The UEFI target's "C" is Microsoft's calling convention; the assembly above uses System V
// (arguments in rdi, rsi).
extern "sysv64" {
    pub fn context_switch(save: *mut u64, next: u64);
}

/// Wait for an interrupt (with interrupts on just for the wait).
pub fn wait_for_interrupt() {
    // SAFETY: sti; hlt is the usual idle sequence; interrupts go off again after.
    unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) };
}

pub fn halt() -> ! {
    loop {
        // SAFETY: interrupts are off; hlt just stops the CPU.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

pub fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: rdtsc has no side effects.
    unsafe { asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags)) };
    (hi as u64) << 32 | lo as u64
}
