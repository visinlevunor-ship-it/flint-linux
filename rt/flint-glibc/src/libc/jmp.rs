//! Non-local jumps and user contexts: setjmp/longjmp (and the sig*/_ forms, __longjmp_chk),
//! getcontext/setcontext/makecontext/swapcontext, with glibc's structure layouts. As glibc
//! does, the saved stack, frame and instruction pointers are mangled with the pointer guard.

use core::arch::global_asm;

type c_int = i32;

// struct __jmp_buf_tag: __jmpbuf[8] (rbx, rbp, r12, r13, r14, r15, rsp, pc), then
// __mask_was_saved (int) at 64 and __saved_mask (128 bytes) at 72.
const MASK_SAVED: usize = 64;
const SAVED_MASK: usize = 72;

/// Record the signal mask if asked; returns 0 (setjmp's direct return value).
#[no_mangle]
unsafe extern "C" fn __flint_sigjmp_save(env: *mut u8, savemask: c_int) -> c_int {
    *(env.add(MASK_SAVED) as *mut c_int) = (savemask != 0) as c_int;
    if savemask != 0 {
        crate::fl::sys::sc4(14 /* rt_sigprocmask */, 0, 0, env.add(SAVED_MASK) as usize, 8);
    }
    0
}

/// Restore the signal mask a sigsetjmp saved.
#[no_mangle]
unsafe extern "C" fn __flint_sigjmp_restore(env: *mut u8) {
    if *(env.add(MASK_SAVED) as *const c_int) != 0 {
        crate::fl::sys::sc4(14, 2 /* SIG_SETMASK */, env.add(SAVED_MASK) as usize, 0, 8);
    }
}

global_asm!(
    // int __sigsetjmp(jmp_buf env, int savemask)
    ".globl __sigsetjmp",
    ".type __sigsetjmp,@function",
    "__sigsetjmp:",
    ".globl sigsetjmp",
    ".type sigsetjmp,@function",
    "sigsetjmp:",
    "mov [rdi], rbx",
    "mov rax, rbp",
    "xor rax, fs:[0x30]",
    "rol rax, 0x11",
    "mov [rdi + 8], rax",
    "mov [rdi + 16], r12",
    "mov [rdi + 24], r13",
    "mov [rdi + 32], r14",
    "mov [rdi + 40], r15",
    "lea rdx, [rsp + 8]",
    "xor rdx, fs:[0x30]",
    "rol rdx, 0x11",
    "mov [rdi + 48], rdx",
    "mov rax, [rsp]",
    "xor rax, fs:[0x30]",
    "rol rax, 0x11",
    "mov [rdi + 56], rax",
    "jmp {save}",
    ".size __sigsetjmp, . - __sigsetjmp",
    ".globl _setjmp",
    ".type _setjmp,@function",
    "_setjmp:",
    "xor esi, esi",
    "jmp __sigsetjmp",
    ".size _setjmp, . - _setjmp",
    ".globl setjmp",
    ".type setjmp,@function",
    "setjmp:",
    "mov esi, 1",
    "jmp __sigsetjmp",
    ".size setjmp, . - setjmp",
    // void longjmp(jmp_buf env, int val), and its aliases
    ".globl longjmp",
    ".type longjmp,@function",
    "longjmp:",
    ".globl _longjmp",
    ".type _longjmp,@function",
    "_longjmp:",
    ".globl siglongjmp",
    ".type siglongjmp,@function",
    "siglongjmp:",
    ".globl __longjmp_chk",
    ".type __longjmp_chk,@function",
    "__longjmp_chk:",
    "push rdi",
    "push rsi",
    "sub rsp, 8",
    "call {restore}",
    "add rsp, 8",
    "pop rsi",
    "pop rdi",
    "mov rbx, [rdi]",
    "mov r12, [rdi + 16]",
    "mov r13, [rdi + 24]",
    "mov r14, [rdi + 32]",
    "mov r15, [rdi + 40]",
    "mov rax, [rdi + 8]",
    "ror rax, 0x11",
    "xor rax, fs:[0x30]",
    "mov rbp, rax",
    "mov rdx, [rdi + 48]",
    "ror rdx, 0x11",
    "xor rdx, fs:[0x30]",
    "mov rcx, [rdi + 56]",
    "ror rcx, 0x11",
    "xor rcx, fs:[0x30]",
    "mov eax, esi",
    "test eax, eax",
    "jnz 1f",
    "inc eax",
    "1:",
    "mov rsp, rdx",
    "jmp rcx",
    ".size longjmp, . - longjmp",
    save = sym __flint_sigjmp_save,
    restore = sym __flint_sigjmp_restore,
);

// --- ucontext --------------------------------------------------------------------------------------
//
// ucontext_t: uc_flags 0, uc_link 8, uc_stack 16 (ss_sp 16, ss_flags 24, ss_size 32),
// uc_mcontext 40 (gregs[23] 40..224, fpregs 224, reserved 232..296), uc_sigmask 296,
// __fpregs_mem 424 (fxsave layout: mxcsr at +24), __ssp 936; 968 bytes.

const UC_LINK: usize = 8;
const SS_SP: usize = 16;
const SS_SIZE: usize = 32;
const fn greg(i: usize) -> usize {
    40 + 8 * i
}
const REG_RDI: usize = 8;
const REG_RSI: usize = 9;
const REG_RBX: usize = 11;
const REG_RSP: usize = 15;
const REG_RIP: usize = 16;
const REG_RDX: usize = 12;
const REG_RCX: usize = 14;
const REG_R8: usize = 0;
const REG_R9: usize = 1;

global_asm!(
    // int getcontext(ucontext_t *ucp)
    ".globl getcontext",
    ".type getcontext,@function",
    "getcontext:",
    "mov [rdi + 40 + 8*11], rbx",
    "mov [rdi + 40 + 8*10], rbp",
    "mov [rdi + 40 + 8*4], r12",
    "mov [rdi + 40 + 8*5], r13",
    "mov [rdi + 40 + 8*6], r14",
    "mov [rdi + 40 + 8*7], r15",
    "mov [rdi + 40 + 8*8], rdi",
    "mov [rdi + 40 + 8*9], rsi",
    "mov [rdi + 40 + 8*12], rdx",
    "mov [rdi + 40 + 8*14], rcx",
    "mov [rdi + 40 + 8*0], r8",
    "mov [rdi + 40 + 8*1], r9",
    "mov rcx, [rsp]",
    "mov [rdi + 40 + 8*16], rcx",
    "lea rcx, [rsp + 8]",
    "mov [rdi + 40 + 8*15], rcx",
    // the floating-point state: fpregs points at __fpregs_mem
    "lea rcx, [rdi + 424]",
    "mov [rdi + 224], rcx",
    "fnstenv [rcx]",
    "fldenv [rcx]",
    "stmxcsr [rcx + 24]",
    // the signal mask
    "lea rdx, [rdi + 296]",
    "xor esi, esi",
    "xor edi, edi",
    "mov r10d, 8",
    "mov eax, 14",
    "syscall",
    "cmp rax, -4095",
    "jae 2f",
    "xor eax, eax",
    "ret",
    "2:",
    "mov eax, -1",
    "ret",
    ".size getcontext, . - getcontext",
    // int setcontext(const ucontext_t *ucp)
    ".globl setcontext",
    ".type setcontext,@function",
    "setcontext:",
    "push rdi",
    "lea rsi, [rdi + 296]",
    "xor edx, edx",
    "mov edi, 2",
    "mov r10d, 8",
    "mov eax, 14",
    "syscall",
    "pop rdx",
    "cmp rax, -4095",
    "jae 3f",
    // jump here with the context in rdx (swapcontext too)
    ".globl __flint_setcontext_regs",
    "__flint_setcontext_regs:",
    "mov rcx, [rdx + 224]",
    "test rcx, rcx",
    "jz 4f",
    "fldenv [rcx]",
    "ldmxcsr [rcx + 24]",
    "4:",
    "mov rsp, [rdx + 40 + 8*15]",
    "mov rbx, [rdx + 40 + 8*11]",
    "mov rbp, [rdx + 40 + 8*10]",
    "mov r12, [rdx + 40 + 8*4]",
    "mov r13, [rdx + 40 + 8*5]",
    "mov r14, [rdx + 40 + 8*6]",
    "mov r15, [rdx + 40 + 8*7]",
    "mov rcx, [rdx + 40 + 8*16]",
    "push rcx",
    "mov rdi, [rdx + 40 + 8*8]",
    "mov rsi, [rdx + 40 + 8*9]",
    "mov rcx, [rdx + 40 + 8*14]",
    "mov r8, [rdx + 40 + 8*0]",
    "mov r9, [rdx + 40 + 8*1]",
    "mov rdx, [rdx + 40 + 8*12]",
    "xor eax, eax",
    "ret",
    "3:",
    "mov eax, -1",
    "ret",
    ".size setcontext, . - setcontext",
    // int swapcontext(ucontext_t *oucp, const ucontext_t *ucp)
    ".globl swapcontext",
    ".type swapcontext,@function",
    "swapcontext:",
    "mov [rdi + 40 + 8*11], rbx",
    "mov [rdi + 40 + 8*10], rbp",
    "mov [rdi + 40 + 8*4], r12",
    "mov [rdi + 40 + 8*5], r13",
    "mov [rdi + 40 + 8*6], r14",
    "mov [rdi + 40 + 8*7], r15",
    "mov [rdi + 40 + 8*8], rdi",
    "mov [rdi + 40 + 8*9], rsi",
    "mov [rdi + 40 + 8*12], rdx",
    "mov [rdi + 40 + 8*14], rcx",
    "mov [rdi + 40 + 8*0], r8",
    "mov [rdi + 40 + 8*1], r9",
    "mov rcx, [rsp]",
    "mov [rdi + 40 + 8*16], rcx",
    "lea rcx, [rsp + 8]",
    "mov [rdi + 40 + 8*15], rcx",
    "lea rcx, [rdi + 424]",
    "mov [rdi + 224], rcx",
    "fnstenv [rcx]",
    "fldenv [rcx]",
    "stmxcsr [rcx + 24]",
    // rt_sigprocmask(SIG_SETMASK, &ucp->uc_sigmask, &oucp->uc_sigmask, 8)
    "push rsi",
    "lea rdx, [rdi + 296]",
    "lea rsi, [rsi + 296]",
    "mov edi, 2",
    "mov r10d, 8",
    "mov eax, 14",
    "syscall",
    "pop rdx",
    "cmp rax, -4095",
    "jae 5f",
    "jmp __flint_setcontext_regs",
    "5:",
    "mov eax, -1",
    "ret",
    ".size swapcontext, . - swapcontext",
    // where a makecontext function returns to: rbx points at the uc_link slot
    ".globl __flint_start_context",
    ".type __flint_start_context,@function",
    "__flint_start_context:",
    "mov rsp, rbx",
    "mov rdi, [rsp]",
    "test rdi, rdi",
    "jz 6f",
    "call setcontext",
    "6:",
    "xor edi, edi",
    "call {exit}",
    "hlt",
    ".size __flint_start_context, . - __flint_start_context",
    exit = sym super::start::exit,
);

extern "C" {
    fn __flint_start_context();
}

/// void makecontext(ucontext_t *ucp, void (*func)(void), int argc, ...): func runs on
/// uc_stack with up to six arguments in registers and the rest on the stack, and returns to
/// uc_link (or exits).
#[no_mangle]
pub unsafe extern "C" fn makecontext(ucp: *mut u8, func: unsafe extern "C" fn(), argc: c_int, mut args: ...) {
    let argc = argc.max(0) as usize;
    let mut va = super::printf::Va(&mut args as *mut _ as *mut super::printf::RawVa);
    let stack_args = argc.saturating_sub(6);
    let top = *(ucp.add(SS_SP) as *const usize) + *(ucp.add(SS_SIZE) as *const usize);
    // [return address][stack args...][uc_link], with rsp + 8 16-byte aligned at entry
    let link_idx = stack_args + 1;
    let mut sp = top - (link_idx + 1) * 8;
    sp = (sp & !15) - 8;
    let s = sp as *mut usize;
    *s = __flint_start_context as usize;
    *s.add(link_idx) = *(ucp.add(UC_LINK) as *const usize);
    let regs = [REG_RDI, REG_RSI, REG_RDX, REG_RCX, REG_R8, REG_R9];
    for i in 0..argc {
        let v = va.int() as usize;
        if i < 6 {
            *(ucp.add(greg(regs[i])) as *mut usize) = v;
        } else {
            *s.add(1 + i - 6) = v;
        }
    }
    *(ucp.add(greg(REG_RIP)) as *mut usize) = func as usize;
    *(ucp.add(greg(REG_RSP)) as *mut usize) = sp;
    *(ucp.add(greg(REG_RBX)) as *mut usize) = s.add(link_idx) as usize;
}
