//! memcpy, memmove, memset, memcmp, bcmp, strlen.
//!
//! Written in assembly: Rust code for these would be recognised by the compiler and turned
//! back into calls to themselves. `rep movsb`/`rep stosb` are fast on every x86-64 CPU of the
//! last decade (ERMSB/FSRM) and small, which matters more here.

core::arch::global_asm!(
    // void *memcpy(void *dst, const void *src, size_t n)
    ".globl memcpy",
    ".type memcpy,@function",
    "memcpy:",
    "mov rax, rdi",
    "mov rcx, rdx",
    "rep movsb",
    "ret",
    // void *memmove(void *dst, const void *src, size_t n)
    ".globl memmove",
    ".type memmove,@function",
    "memmove:",
    "mov rax, rdi",
    "mov rcx, rdx",
    "mov r8, rdi",
    "sub r8, rsi",
    "cmp r8, rdx",       // (dst - src) < n (unsigned): dst overlaps the end of src
    "jb 2f",
    "rep movsb",
    "ret",
    "2:",
    "lea rsi, [rsi + rdx - 1]",
    "lea rdi, [rdi + rdx - 1]",
    "std",
    "rep movsb",
    "cld",
    "ret",
    // void *memset(void *dst, int c, size_t n)
    ".globl memset",
    ".type memset,@function",
    "memset:",
    "mov r8, rdi",
    "mov eax, esi",
    "mov rcx, rdx",
    "rep stosb",
    "mov rax, r8",
    "ret",
    // int memcmp(const void *a, const void *b, size_t n)
    ".globl memcmp",
    ".type memcmp,@function",
    "memcmp:",
    ".globl bcmp",
    ".type bcmp,@function",
    "bcmp:",
    "xor eax, eax",
    // 8 bytes at a time while they are equal
    "7:",
    "cmp rdx, 8",
    "jb 8f",
    "mov rcx, qword ptr [rdi]",
    "cmp rcx, qword ptr [rsi]",
    "jne 8f",
    "add rdi, 8",
    "add rsi, 8",
    "sub rdx, 8",
    "jmp 7b",
    // then byte by byte (also finds the first differing byte in a differing qword)
    "8:",
    "test rdx, rdx",
    "jz 4f",
    "3:",
    "movzx eax, byte ptr [rdi]",
    "movzx ecx, byte ptr [rsi]",
    "sub eax, ecx",
    "jnz 4f",
    "inc rdi",
    "inc rsi",
    "dec rdx",
    "jnz 3b",
    "4:",
    "ret",
    // size_t strlen(const char *s)
    ".globl strlen",
    ".type strlen,@function",
    "strlen:",
    "mov rax, rdi",
    "5:",
    "cmp byte ptr [rax], 0",
    "je 6f",
    "inc rax",
    "jmp 5b",
    "6:",
    "sub rax, rdi",
    "ret",
);

/// Length of a NUL-terminated string, for Rust code in this crate.
pub unsafe fn cstrlen(s: *const u8) -> usize {
    let mut n = 0;
    while *s.add(n) != 0 {
        n += 1;
    }
    n
}

pub unsafe fn cstr<'a>(s: *const u8) -> &'a [u8] {
    core::slice::from_raw_parts(s, cstrlen(s))
}
