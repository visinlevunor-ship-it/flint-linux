//! Panics print a message to stderr and exit with status 101. No unwinding.

use core::fmt::Write;
use core::panic::PanicInfo;

use crate::io::Writer;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let mut w = Writer::new(2);
    let _ = writeln!(w, "flint-rt: panic: {info}");
    w.flush();
    crate::sys::exit(101)
}

// Prebuilt `core`/`alloc` mention these even though panic=abort never unwinds.
#[no_mangle]
pub extern "C" fn rust_eh_personality() {}

#[no_mangle]
pub extern "C" fn _Unwind_Resume() -> ! {
    crate::sys::exit(101)
}
