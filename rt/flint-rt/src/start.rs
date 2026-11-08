//! Process entry: `_start` receives the kernel's initial stack
//! (argc, argv..., NULL, envp..., NULL, auxv...) and hands control to `flint_main`.

use core::arch::global_asm;
use core::cell::UnsafeCell;

global_asm!(
    ".globl _start",
    ".type _start,@function",
    "_start:",
    "xor ebp, ebp",      // mark the outermost frame
    "mov rdi, rsp",      // first argument: the initial stack pointer
    "and rsp, -16",      // ABI: 16-byte alignment at the call
    "call {entry}",
    "ud2",
    entry = sym rt_entry,
);

extern "Rust" {
    /// Defined by the program through `entry!`.
    fn flint_main() -> i32;
}

struct Argv {
    argc: UnsafeCell<usize>,
    argv: UnsafeCell<*const *const u8>,
}
// SAFETY: written once in `rt_entry` before any other code runs; Flint is single threaded.
unsafe impl Sync for Argv {}

static ARGV: Argv = Argv { argc: UnsafeCell::new(0), argv: UnsafeCell::new(core::ptr::null()) };

unsafe extern "C" fn rt_entry(sp: *const usize) -> ! {
    *ARGV.argc.get() = *sp;
    *ARGV.argv.get() = sp.add(1) as *const *const u8;
    let code = flint_main();
    crate::sys::exit(code)
}

/// The program's arguments, argv[0] first. Arguments that are not valid UTF-8
/// are yielded as `"<non-utf8>"`.
pub fn args() -> Args {
    // SAFETY: set once at startup, read-only afterwards.
    unsafe { Args { next: 0, argc: *ARGV.argc.get(), argv: *ARGV.argv.get() } }
}

pub struct Args {
    next: usize,
    argc: usize,
    argv: *const *const u8,
}

impl Iterator for Args {
    type Item = &'static str;

    fn next(&mut self) -> Option<&'static str> {
        if self.next >= self.argc {
            return None;
        }
        // SAFETY: the kernel guarantees argc valid NUL-terminated strings that live for the process.
        unsafe {
            let p = *self.argv.add(self.next);
            self.next += 1;
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            Some(core::str::from_utf8(core::slice::from_raw_parts(p, len)).unwrap_or("<non-utf8>"))
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.argc - self.next;
        (n, Some(n))
    }
}

impl ExactSizeIterator for Args {}
