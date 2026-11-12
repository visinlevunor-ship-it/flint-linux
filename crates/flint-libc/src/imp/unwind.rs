//! The unwinder: `_Unwind_*`, the Itanium C++ ABI interface that Rust's panics (and C++
//! exceptions) use, for x86-64.
//!
//! Frames are described by the program's DWARF call frame information: `.eh_frame_hdr`
//! (found through PT_GNU_EH_FRAME) gives a sorted table to binary-search for a function's
//! FDE; running the CIE's and FDE's CFA instructions up to the frame's address says where
//! the caller's registers are saved. Raising is two-phase, as the ABI asks: first search
//! for a handler without changing anything, then walk again running cleanups and land in
//! the handler. DWARF expressions in CFI (not used in Flint's static binaries) end a walk.

use crate::imp::misc;
use core::ptr;

const RA: usize = 16; // DWARF column of the return address
const NREG: usize = 17;
const RSP: usize = 7;

#[repr(C)]
pub struct Context {
    regs: [usize; NREG],
    cfa: usize,
    lsda: usize,
    func_start: usize,
    personality: usize,
    signal: bool,
}

#[repr(C)]
pub struct Exception {
    class: u64,
    cleanup: Option<unsafe extern "C" fn(i32, *mut Exception)>,
    private_1: usize,
    private_2: usize, // the CFA of the handler's frame, found in phase 1
}

type Personality = unsafe extern "C" fn(i32, i32, u64, *mut Exception, *mut Context) -> i32;

const URC_NO_REASON: i32 = 0;
const URC_FATAL_PHASE2_ERROR: i32 = 2;
const URC_FATAL_PHASE1_ERROR: i32 = 3;
const URC_END_OF_STACK: i32 = 5;
const URC_HANDLER_FOUND: i32 = 6;
const URC_INSTALL_CONTEXT: i32 = 7;
const URC_CONTINUE_UNWIND: i32 = 8;
const UA_SEARCH_PHASE: i32 = 1;
const UA_CLEANUP_PHASE: i32 = 2;
const UA_HANDLER_FRAME: i32 = 4;

// --- reading DWARF data ---

struct Reader {
    p: usize,
}

impl Reader {
    unsafe fn u8(&mut self) -> u8 {
        let v = *(self.p as *const u8);
        self.p += 1;
        v
    }
    unsafe fn u16(&mut self) -> u16 {
        let v = ptr::read_unaligned(self.p as *const u16);
        self.p += 2;
        v
    }
    unsafe fn u32(&mut self) -> u32 {
        let v = ptr::read_unaligned(self.p as *const u32);
        self.p += 4;
        v
    }
    unsafe fn u64(&mut self) -> u64 {
        let v = ptr::read_unaligned(self.p as *const u64);
        self.p += 8;
        v
    }
    unsafe fn uleb(&mut self) -> u64 {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = self.u8();
            if shift < 64 {
                v |= ((b & 0x7f) as u64) << shift;
            }
            shift += 7;
            if b & 0x80 == 0 {
                return v;
            }
        }
    }
    unsafe fn sleb(&mut self) -> i64 {
        let (mut v, mut shift) = (0i64, 0);
        loop {
            let b = self.u8();
            if shift < 64 {
                v |= ((b & 0x7f) as i64) << shift;
            }
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && b & 0x40 != 0 {
                    v |= -1i64 << shift;
                }
                return v;
            }
        }
    }
    /// A pointer in DW_EH_PE encoding `enc`; `datarel` is the base for DW_EH_PE_datarel.
    unsafe fn ptr(&mut self, enc: u8, datarel: usize) -> usize {
        if enc == 0xff {
            return 0;
        }
        let start = self.p;
        let v: usize = match enc & 0x0f {
            0x00 => self.u64() as usize,
            0x01 => self.uleb() as usize,
            0x02 => self.u16() as usize,
            0x03 => self.u32() as usize,
            0x04 => self.u64() as usize,
            0x09 => self.sleb() as usize,
            0x0a => self.u16() as i16 as isize as usize,
            0x0b => self.u32() as i32 as isize as usize,
            0x0c => self.u64() as usize,
            _ => return 0,
        };
        let mut v = match enc & 0x70 {
            0x00 => v,
            0x10 => start.wrapping_add(v),   // pcrel
            0x30 => datarel.wrapping_add(v), // datarel
            _ => v,
        };
        if v != 0 && enc & 0x80 != 0 {
            v = *(v as *const usize); // indirect
        }
        v
    }
}

// --- finding the FDE for an address ---

static mut EH_FRAME_HDR: usize = 0;

unsafe fn eh_frame_hdr() -> usize {
    if EH_FRAME_HDR == 0 {
        let phdr = misc::getauxval(3) as usize;
        let phnum = misc::getauxval(5) as usize;
        let bias = misc::load_bias();
        for i in 0..phnum {
            let p = phdr + i * 56;
            if *(p as *const u32) == 0x6474_e550 {
                EH_FRAME_HDR = bias.wrapping_add(*((p + 16) as *const usize));
            }
        }
    }
    EH_FRAME_HDR
}

struct Fde {
    pc_begin: usize,
    pc_end: usize,
    cie_insns: (usize, usize),
    fde_insns: (usize, usize),
    code_align: u64,
    data_align: i64,
    ra_reg: usize,
    lsda: usize,
    personality: usize,
    fde_enc: u8,
    signal: bool,
}

unsafe fn find_fde(pc: usize) -> Option<Fde> {
    let hdr = eh_frame_hdr();
    if hdr == 0 {
        return None;
    }
    let mut r = Reader { p: hdr };
    if r.u8() != 1 {
        return None;
    }
    let ptr_enc = r.u8();
    let count_enc = r.u8();
    let table_enc = r.u8();
    let _eh_frame = r.ptr(ptr_enc, hdr);
    let count = r.ptr(count_enc, hdr);
    if table_enc != 0x3b || count == 0 {
        return None; // only the datarel|sdata4 table every linker writes
    }
    // binary search the table of (initial location, FDE address), 8 bytes per entry
    let table = r.p;
    let entry = |i: usize| -> (usize, usize) {
        let e = table + i * 8;
        (
            hdr.wrapping_add(ptr::read_unaligned(e as *const i32) as isize as usize),
            hdr.wrapping_add(ptr::read_unaligned((e + 4) as *const i32) as isize as usize),
        )
    };
    let (mut lo, mut hi) = (0usize, count);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if entry(mid).0 <= pc {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (start, fde) = entry(lo);
    if pc < start {
        return None;
    }
    let f = parse_fde(fde)?;
    if pc >= f.pc_end {
        return None;
    }
    Some(f)
}

unsafe fn parse_fde(fde: usize) -> Option<Fde> {
    let mut r = Reader { p: fde };
    let len = r.u32() as usize;
    if len == 0 || len == 0xffff_ffff {
        return None;
    }
    let end = r.p + len;
    let cie_ptr_pos = r.p;
    let cie_off = r.u32() as usize;
    let cie = cie_ptr_pos - cie_off;
    // the CIE
    let mut c = Reader { p: cie };
    let clen = c.u32() as usize;
    let cend = c.p + clen;
    if c.u32() != 0 {
        return None;
    }
    let version = c.u8();
    let aug_start = c.p;
    while c.u8() != 0 {}
    let aug = core::slice::from_raw_parts(aug_start as *const u8, c.p - aug_start - 1);
    if aug.starts_with(b"eh") {
        c.p += 8;
    }
    let code_align = c.uleb();
    let data_align = c.sleb();
    let ra_reg = if version == 1 { c.u8() as usize } else { c.uleb() as usize };
    let (mut fde_enc, mut lsda_enc, mut personality, mut signal) = (0u8, 0xffu8, 0usize, false);
    if aug.first() == Some(&b'z') {
        let alen = c.uleb() as usize;
        let adata_end = c.p + alen;
        for &ch in &aug[1..] {
            match ch {
                b'R' => fde_enc = c.u8(),
                b'L' => lsda_enc = c.u8(),
                b'P' => {
                    let enc = c.u8();
                    personality = c.ptr(enc, 0);
                }
                b'S' => signal = true,
                _ => break,
            }
        }
        c.p = adata_end;
    }
    let cie_insns = (c.p, cend);
    // the rest of the FDE
    let pc_begin = r.ptr(fde_enc, 0);
    let pc_range = r.ptr(fde_enc & 0x0f, 0);
    let mut lsda = 0;
    if aug.first() == Some(&b'z') {
        let alen = r.uleb() as usize;
        let aend = r.p + alen;
        if lsda_enc != 0xff && alen > 0 {
            lsda = r.ptr(lsda_enc, 0);
        }
        r.p = aend;
    }
    Some(Fde {
        pc_begin,
        pc_end: pc_begin.wrapping_add(pc_range),
        cie_insns,
        fde_insns: (r.p, end),
        code_align,
        data_align,
        ra_reg,
        lsda,
        personality,
        fde_enc,
        signal,
    })
}

// --- running the CFA program ---

#[derive(Clone, Copy, PartialEq)]
enum Rule {
    Undefined,
    Same,
    Offset(i64),
    ValOffset(i64),
    Register(usize),
    Expression,
}

#[derive(Clone, Copy)]
struct Row {
    cfa_reg: usize,
    cfa_off: i64,
    cfa_expr: bool,
    rules: [Rule; NREG],
}

unsafe fn run(f: &Fde, (start, end): (usize, usize), pc: usize, row: &mut Row, initial: &Row, stack: &mut [Row; 8], depth: &mut usize) {
    let mut r = Reader { p: start };
    let mut loc = f.pc_begin;
    let reg_ok = |reg: u64| (reg as usize) < NREG;
    while r.p < end {
        let op = r.u8();
        let (hi2, lo6) = (op & 0xc0, (op & 0x3f) as usize);
        match hi2 {
            0x40 => {
                loc += lo6 * f.code_align as usize;
                if loc > pc {
                    return;
                }
                continue;
            }
            0x80 => {
                let off = r.uleb() as i64 * f.data_align;
                if lo6 < NREG {
                    row.rules[lo6] = Rule::Offset(off);
                }
                continue;
            }
            0xc0 => {
                if lo6 < NREG {
                    row.rules[lo6] = initial.rules[lo6];
                }
                continue;
            }
            _ => {}
        }
        match op {
            0x00 => {}
            0x01 => {
                loc = r.ptr(f.fde_enc, 0);
                if loc > pc {
                    return;
                }
            }
            0x02..=0x04 => {
                let d = match op {
                    0x02 => r.u8() as usize,
                    0x03 => r.u16() as usize,
                    _ => r.u32() as usize,
                };
                loc += d * f.code_align as usize;
                if loc > pc {
                    return;
                }
            }
            0x05 => {
                let reg = r.uleb();
                let off = r.uleb() as i64 * f.data_align;
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::Offset(off);
                }
            }
            0x06 => {
                let reg = r.uleb();
                if reg_ok(reg) {
                    row.rules[reg as usize] = initial.rules[reg as usize];
                }
            }
            0x07 => {
                let reg = r.uleb();
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::Undefined;
                }
            }
            0x08 => {
                let reg = r.uleb();
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::Same;
                }
            }
            0x09 => {
                let (reg, reg2) = (r.uleb(), r.uleb());
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::Register(reg2 as usize);
                }
            }
            0x0a => {
                if *depth < stack.len() {
                    stack[*depth] = *row;
                    *depth += 1;
                }
            }
            0x0b => {
                if *depth > 0 {
                    *depth -= 1;
                    *row = stack[*depth];
                }
            }
            0x0c => {
                row.cfa_reg = r.uleb() as usize;
                row.cfa_off = r.uleb() as i64;
                row.cfa_expr = false;
            }
            0x0d => {
                row.cfa_reg = r.uleb() as usize;
                row.cfa_expr = false;
            }
            0x0e => row.cfa_off = r.uleb() as i64,
            0x0f => {
                let n = r.uleb() as usize;
                r.p += n;
                row.cfa_expr = true;
            }
            0x10 | 0x16 => {
                let reg = r.uleb();
                let n = r.uleb() as usize;
                r.p += n;
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::Expression;
                }
            }
            0x11 => {
                let reg = r.uleb();
                let off = r.sleb() * f.data_align;
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::Offset(off);
                }
            }
            0x12 => {
                row.cfa_reg = r.uleb() as usize;
                row.cfa_off = r.sleb() * f.data_align;
                row.cfa_expr = false;
            }
            0x13 => row.cfa_off = r.sleb() * f.data_align,
            0x14 => {
                let reg = r.uleb();
                let off = r.uleb() as i64 * f.data_align;
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::ValOffset(off);
                }
            }
            0x15 => {
                let reg = r.uleb();
                let off = r.sleb() * f.data_align;
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::ValOffset(off);
                }
            }
            0x2e => {
                r.uleb();
            }
            0x2f => {
                let reg = r.uleb();
                let off = -(r.uleb() as i64) * f.data_align;
                if reg_ok(reg) {
                    row.rules[reg as usize] = Rule::Offset(off);
                }
            }
            _ => return, // unknown: stop here, the row so far is the best there is
        }
    }
}

/// Fill in the frame's FDE-derived fields (lsda, function start, personality).
unsafe fn describe(ctx: &mut Context) -> Option<Fde> {
    let ip = ctx.regs[RA];
    // the return address points after the call, possibly into the next function
    let pc = if ctx.signal { ip } else { ip - 1 };
    let f = find_fde(pc)?;
    ctx.lsda = f.lsda;
    ctx.func_start = f.pc_begin;
    ctx.personality = f.personality;
    Some(f)
}

/// Move `ctx` to the calling frame. Returns false at the end of the stack.
unsafe fn step(ctx: &mut Context, f: &Fde) -> bool {
    let pc = if ctx.signal { ctx.regs[RA] } else { ctx.regs[RA] - 1 };
    let empty = Row { cfa_reg: RSP, cfa_off: 8, cfa_expr: false, rules: [Rule::Same; NREG] };
    let mut initial = empty;
    let mut stack = [empty; 8];
    let mut depth = 0;
    run(f, f.cie_insns, usize::MAX, &mut initial, &empty, &mut stack, &mut depth);
    let mut row = initial;
    run(f, f.fde_insns, pc, &mut row, &initial, &mut stack, &mut depth);
    if row.cfa_expr || row.cfa_reg >= NREG {
        return false;
    }
    let cfa = (ctx.regs[row.cfa_reg] as i64 + row.cfa_off) as usize;
    let mut regs = ctx.regs;
    for (i, rule) in row.rules.iter().enumerate() {
        match *rule {
            Rule::Same => {}
            Rule::Undefined => {
                if i == f.ra_reg {
                    return false;
                }
            }
            Rule::Offset(o) => regs[i] = *((cfa as i64 + o) as *const usize),
            Rule::ValOffset(o) => regs[i] = (cfa as i64 + o) as usize,
            Rule::Register(r) => {
                if r < NREG {
                    regs[i] = ctx.regs[r];
                }
            }
            Rule::Expression => return false,
        }
    }
    let ra = regs[f.ra_reg.min(NREG - 1)];
    if ra == 0 {
        return false;
    }
    regs[RSP] = cfa;
    regs[RA] = ra;
    ctx.regs = regs;
    ctx.cfa = cfa;
    ctx.signal = f.signal;
    true
}

// --- capturing and installing register state ---

core::arch::global_asm!(
    // Each entry point saves the caller's callee-saved registers, stack pointer and return
    // address into a Context on its own stack, then calls the Rust implementation with it.
    // Context.regs is indexed by DWARF number: rax 0, rdx 1, rcx 2, rbx 3, rsi 4, rdi 5,
    // rbp 6, rsp 7, r8..r15 8..15, return address 16.
    ".macro FLINT_CAPTURE impl",
    "sub rsp, 200",
    "mov qword ptr [rsp + 3*8], rbx",
    "mov qword ptr [rsp + 6*8], rbp",
    "mov qword ptr [rsp + 12*8], r12",
    "mov qword ptr [rsp + 13*8], r13",
    "mov qword ptr [rsp + 14*8], r14",
    "mov qword ptr [rsp + 15*8], r15",
    "lea rax, [rsp + 208]", // the caller's rsp after our return
    "mov qword ptr [rsp + 7*8], rax",
    "mov rax, qword ptr [rsp + 200]", // our return address
    "mov qword ptr [rsp + 16*8], rax",
    "mov rsi, rsp", // second argument: the context (first is the caller's rdi)
    "call \\impl",
    "add rsp, 200",
    "ret",
    ".endm",
    ".globl _Unwind_RaiseException",
    ".type _Unwind_RaiseException,@function",
    "_Unwind_RaiseException:",
    "FLINT_CAPTURE {raise}",
    ".globl _Unwind_Resume",
    ".type _Unwind_Resume,@function",
    "_Unwind_Resume:",
    "FLINT_CAPTURE {resume}",
    ".globl _Unwind_Backtrace",
    ".type _Unwind_Backtrace,@function",
    "_Unwind_Backtrace:",
    // (trace_fn, arg): keep arg in rdx for the implementation's third parameter
    "mov rdx, rsi",
    "FLINT_CAPTURE {backtrace}",
    // void __flint_install(const Context *): load the registers and jump
    ".globl __flint_install",
    ".type __flint_install,@function",
    "__flint_install:",
    "mov rax, qword ptr [rdi + 7*8]", // target rsp
    "sub rax, 8",
    "mov rcx, qword ptr [rdi + 16*8]", // target ip
    "mov qword ptr [rax], rcx",        // pushed as a return address
    "mov rcx, qword ptr [rdi + 2*8]",
    "mov rdx, qword ptr [rdi + 1*8]",
    "mov rbx, qword ptr [rdi + 3*8]",
    "mov rsi, qword ptr [rdi + 4*8]",
    "mov rbp, qword ptr [rdi + 6*8]",
    "mov r8, qword ptr [rdi + 8*8]",
    "mov r9, qword ptr [rdi + 9*8]",
    "mov r10, qword ptr [rdi + 10*8]",
    "mov r11, qword ptr [rdi + 11*8]",
    "mov r12, qword ptr [rdi + 12*8]",
    "mov r13, qword ptr [rdi + 13*8]",
    "mov r14, qword ptr [rdi + 14*8]",
    "mov r15, qword ptr [rdi + 15*8]",
    "mov rsp, rax",
    "mov rax, qword ptr [rdi + 0*8]",
    "mov rdi, qword ptr [rdi + 5*8]",
    "ret",
    raise = sym raise_impl,
    resume = sym resume_impl,
    backtrace = sym backtrace_impl,
);

extern "C" {
    fn __flint_install(ctx: *const Context) -> !;
}

unsafe fn fresh(ctx: *mut Context) -> &'static mut Context {
    let c = &mut *ctx;
    c.cfa = c.regs[RSP];
    c.lsda = 0;
    c.func_start = 0;
    c.personality = 0;
    c.signal = false;
    c
}

/// Phase 2 from `ctx`: run cleanups, install the handler's context.
unsafe fn phase2(exc: *mut Exception, ctx: &mut Context) -> i32 {
    loop {
        let Some(f) = describe(ctx) else { return URC_FATAL_PHASE2_ERROR };
        if ctx.personality != 0 {
            let p: Personality = core::mem::transmute(ctx.personality);
            let mut actions = UA_CLEANUP_PHASE;
            // the CFA of the frame the context describes: what phase 1 recorded
            let frame_cfa = frame_cfa(ctx, &f);
            if frame_cfa == (*exc).private_2 {
                actions |= UA_HANDLER_FRAME;
            }
            match p(1, actions, (*exc).class, exc, ctx) {
                URC_INSTALL_CONTEXT => __flint_install(ctx),
                URC_CONTINUE_UNWIND => {}
                _ => return URC_FATAL_PHASE2_ERROR,
            }
            if actions & UA_HANDLER_FRAME != 0 {
                return URC_FATAL_PHASE2_ERROR; // the handler frame must install
            }
        }
        if !step(ctx, &f) {
            return URC_FATAL_PHASE2_ERROR;
        }
    }
}

/// The CFA of the frame `ctx` is in (computed without moving).
unsafe fn frame_cfa(ctx: &Context, f: &Fde) -> usize {
    let mut probe = Context { regs: ctx.regs, cfa: 0, lsda: 0, func_start: 0, personality: 0, signal: ctx.signal };
    if step(&mut probe, f) {
        probe.cfa
    } else {
        0
    }
}

unsafe extern "C" fn raise_impl(exc: *mut Exception, ctx: *mut Context) -> i32 {
    let start = core::ptr::read(ctx);
    let ctx = fresh(ctx);
    // phase 1: look for a handler
    let mut c = core::ptr::read(ctx);
    loop {
        let Some(f) = describe(&mut c) else { return URC_END_OF_STACK };
        if c.personality != 0 {
            let p: Personality = core::mem::transmute(c.personality);
            match p(1, UA_SEARCH_PHASE, (*exc).class, exc, &mut c) {
                URC_HANDLER_FOUND => {
                    (*exc).private_1 = 0;
                    (*exc).private_2 = frame_cfa(&c, &f);
                    break;
                }
                URC_CONTINUE_UNWIND => {}
                _ => return URC_FATAL_PHASE1_ERROR,
            }
        }
        if !step(&mut c, &f) {
            return URC_END_OF_STACK;
        }
    }
    // phase 2, from the start again
    let mut c2 = start;
    let c2 = fresh(&mut c2);
    phase2(exc, c2)
}

unsafe extern "C" fn resume_impl(exc: *mut Exception, ctx: *mut Context) -> ! {
    let ctx = fresh(ctx);
    phase2(exc, ctx);
    crate::imp::start::abort()
}

type TraceFn = unsafe extern "C" fn(*mut Context, *mut core::ffi::c_void) -> i32;

unsafe extern "C" fn backtrace_impl(trace: TraceFn, ctx: *mut Context, arg: *mut core::ffi::c_void) -> i32 {
    let ctx = fresh(ctx);
    loop {
        let f = describe(ctx);
        if trace(ctx, arg) != URC_NO_REASON {
            return URC_FATAL_PHASE1_ERROR;
        }
        let Some(f) = f else { return URC_END_OF_STACK };
        if !step(ctx, &f) {
            return URC_END_OF_STACK;
        }
    }
}

// --- what personality routines call ---

#[no_mangle]
pub unsafe extern "C" fn _Unwind_GetGR(ctx: *mut Context, i: i32) -> usize {
    (*ctx).regs.get(i as usize).copied().unwrap_or(0)
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_SetGR(ctx: *mut Context, i: i32, v: usize) {
    if let Some(r) = (*ctx).regs.get_mut(i as usize) {
        *r = v;
    }
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_GetIP(ctx: *mut Context) -> usize {
    (*ctx).regs[RA]
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_GetIPInfo(ctx: *mut Context, before: *mut i32) -> usize {
    *before = (*ctx).signal as i32;
    (*ctx).regs[RA]
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_SetIP(ctx: *mut Context, ip: usize) {
    (*ctx).regs[RA] = ip;
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_GetCFA(ctx: *mut Context) -> usize {
    (*ctx).cfa
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_GetLanguageSpecificData(ctx: *mut Context) -> usize {
    (*ctx).lsda
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_GetRegionStart(ctx: *mut Context) -> usize {
    (*ctx).func_start
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_GetDataRelBase(_ctx: *mut Context) -> usize {
    0
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_GetTextRelBase(_ctx: *mut Context) -> usize {
    0
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_DeleteException(exc: *mut Exception) {
    if let Some(c) = (*exc).cleanup {
        c(1 /* _URC_FOREIGN_EXCEPTION_CAUGHT */, exc);
    }
}
#[no_mangle]
pub unsafe extern "C" fn _Unwind_FindEnclosingFunction(pc: usize) -> usize {
    find_fde(pc).map(|f| f.pc_begin).unwrap_or(0)
}
