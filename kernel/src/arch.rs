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

static mut IDT: [Gate; 32] = [Gate { off_lo: 0, sel: 0, ist: 0, attr: 0, off_mid: 0, off_hi: 0, _r: 0 }; 32];

/// What the exception stubs push, from the lowest address up.
#[repr(C)]
pub struct TrapFrame {
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// One stub per exception: the CPU pushes an error code for some; the others push 0.
global_asm!(
    ".macro TRAP n, err",
    ".global trap\\n",
    "trap\\n:",
    ".if \\err == 0",
    "push 0",
    ".endif",
    "push \\n",
    "jmp trap_common",
    ".endm",
    "TRAP 0,0", "TRAP 1,0", "TRAP 2,0", "TRAP 3,0", "TRAP 4,0", "TRAP 5,0", "TRAP 6,0", "TRAP 7,0",
    "TRAP 8,1", "TRAP 9,0", "TRAP 10,1", "TRAP 11,1", "TRAP 12,1", "TRAP 13,1", "TRAP 14,1", "TRAP 15,0",
    "TRAP 16,0", "TRAP 17,1", "TRAP 18,0", "TRAP 19,0", "TRAP 20,0", "TRAP 21,1", "TRAP 22,0", "TRAP 23,0",
    "TRAP 24,0", "TRAP 25,0", "TRAP 26,0", "TRAP 27,0", "TRAP 28,0", "TRAP 29,1", "TRAP 30,1", "TRAP 31,0",
    "trap_common:",
    // the frame is at rsp; keep the stack 16-byte aligned for the call
    "mov rdi, rsp",
    "and rsp, -16",
    "call {handler}",
    "ud2",
    handler = sym trap_handler,
);

extern "C" {
    fn trap0(); fn trap1(); fn trap2(); fn trap3(); fn trap4(); fn trap5(); fn trap6(); fn trap7();
    fn trap8(); fn trap9(); fn trap10(); fn trap11(); fn trap12(); fn trap13(); fn trap14(); fn trap15();
    fn trap16(); fn trap17(); fn trap18(); fn trap19(); fn trap20(); fn trap21(); fn trap22(); fn trap23();
    fn trap24(); fn trap25(); fn trap26(); fn trap27(); fn trap28(); fn trap29(); fn trap30(); fn trap31();
}

const NAMES: [&str; 32] = [
    "divide error", "debug", "NMI", "breakpoint", "overflow", "bound range", "invalid opcode", "no FPU",
    "double fault", "coprocessor", "invalid TSS", "segment not present", "stack fault", "general protection",
    "page fault", "reserved", "x87 error", "alignment check", "machine check", "SIMD error", "virtualization",
    "control protection", "22", "23", "24", "25", "26", "27", "hypervisor", "VMM communication", "security", "31",
];

extern "sysv64" fn trap_handler(f: &TrapFrame) -> ! {
    let cr2: u64;
    // SAFETY: reading CR2 has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
    let name = NAMES[(f.vector & 31) as usize];
    if f.cs & 3 == 3 {
        // a program's fault ends the program, as a signal would on Linux
        if f.vector == 14 {
            kprintln!("[kernel] {}: page fault at {:#x} (ip {:#x}, error {:#x}); killed", crate::proc::name(), cr2, f.rip, f.error);
        } else {
            kprintln!("[kernel] {}: {} (ip {:#x}, error {:#x}); killed", crate::proc::name(), name, f.rip, f.error);
        }
        crate::proc::exit_current(128 + if f.vector == 14 || f.vector == 13 { 11 } else { 4 });
    }
    kprintln!("\n[kernel] PANIC: {} in the kernel at {:#x} (error {:#x}, cr2 {:#x})", name, f.rip, f.error, cr2);
    halt()
}

pub unsafe fn init_idt() {
    let handlers: [unsafe extern "C" fn(); 32] = [
        trap0, trap1, trap2, trap3, trap4, trap5, trap6, trap7, trap8, trap9, trap10, trap11, trap12, trap13, trap14, trap15,
        trap16, trap17, trap18, trap19, trap20, trap21, trap22, trap23, trap24, trap25, trap26, trap27, trap28, trap29, trap30,
        trap31,
    ];
    for (i, h) in handlers.iter().enumerate() {
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

/// The registers of a program at its system call, from the lowest address up.
#[repr(C)]
pub struct SyscallFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r9: u64,
    pub r8: u64,
    pub r10: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rax: u64,
    pub rflags: u64,
    pub rip: u64,
    pub rsp: u64,
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
    "push qword ptr [rip + SYSCALL_USER_RSP]",
    "push rcx",
    "push r11",
    "push rax",
    "push rdi",
    "push rsi",
    "push rdx",
    "push r10",
    "push r8",
    "push r9",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov rdi, rsp",
    "call {dispatch}",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "pop r9",
    "pop r8",
    "pop r10",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop rax",
    "pop r11",
    "pop rcx",
    "pop rsp",
    "sysretq",
    dispatch = sym crate::syscall::dispatch,
);

extern "C" {
    fn syscall_entry();
}

pub unsafe fn init_syscall() {
    SYSCALL_STACK_TOP = stack_top(addr_of!(SYSCALL_STACK));
    wrmsr(MSR_EFER, rdmsr(MSR_EFER) | 1); // SCE
    wrmsr(MSR_STAR, ((0x10u64 | 0) << 48) | ((KCODE as u64) << 32));
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
    asm!("mov cr4, {}", in(reg) cr4);
}

// ---------------------------------------------------------------------------------------
// Into a program and back

/// The kernel's registers while a program runs: `exit_to_kernel` returns from `enter_user`.
#[no_mangle]
static mut KERNEL_RSP: u64 = 0;

global_asm!(
    // enter_user(rip, rsp): save the kernel's callee-saved registers, then iretq to ring 3
    ".global enter_user",
    "enter_user:",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rip + KERNEL_RSP], rsp",
    "push {udata}",
    "push rsi",
    "push 0x2",
    "push {ucode}",
    "push rdi",
    // a program starts with clean registers
    "xor eax, eax", "xor ebx, ebx", "xor ecx, ecx", "xor edx, edx", "xor esi, esi", "xor edi, edi",
    "xor ebp, ebp", "xor r8d, r8d", "xor r9d, r9d", "xor r10d, r10d", "xor r11d, r11d",
    "xor r12d, r12d", "xor r13d, r13d", "xor r14d, r14d", "xor r15d, r15d",
    "mov ax, {udata}",
    "mov ds, ax",
    "mov es, ax",
    "xor eax, eax",
    "iretq",
    // exit_to_kernel(code): back to enter_user's caller, which gets `code`
    ".global exit_to_kernel",
    "exit_to_kernel:",
    "mov rsp, [rip + KERNEL_RSP]",
    "mov ax, {kdata}",
    "mov ds, ax",
    "mov es, ax",
    "mov rax, rdi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
    udata = const UDATA,
    ucode = const UCODE,
    kdata = const KDATA,
);

// The UEFI target's "C" is Microsoft's calling convention; the assembly above uses System V
// (arguments in rdi, rsi).
extern "sysv64" {
    /// Run the current program from `rip` with stack `rsp` until it exits; its exit status.
    pub fn enter_user(rip: u64, rsp: u64) -> i64;
    /// Leave the running program for good.
    pub fn exit_to_kernel(code: i64) -> !;
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
