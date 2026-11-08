//! Minimal stdio: a small buffer that is flushed when full, on `flush`, and on drop.

use core::fmt;

use crate::sys;

pub struct Writer {
    fd: i32,
    buf: [u8; 256],
    len: usize,
}

impl Writer {
    pub const fn new(fd: i32) -> Self {
        Writer { fd, buf: [0; 256], len: 0 }
    }

    pub fn flush(&mut self) {
        let _ = write_all(self.fd, &self.buf[..self.len]);
        self.len = 0;
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut rest = s.as_bytes();
        while !rest.is_empty() {
            if self.len == self.buf.len() {
                self.flush();
            }
            let n = rest.len().min(self.buf.len() - self.len);
            self.buf[self.len..self.len + n].copy_from_slice(&rest[..n]);
            self.len += n;
            rest = &rest[n..];
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Write everything, retrying on EINTR and short writes. Errors end the write silently.
pub fn write_all(fd: i32, mut buf: &[u8]) -> sys::Result<()> {
    while !buf.is_empty() {
        match sys::write(fd, buf) {
            Ok(0) => return Err(sys::Errno(5)), // EIO: nothing accepted
            Ok(n) => buf = &buf[n..],
            Err(sys::Errno::EINTR) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn _print(fd: i32, args: fmt::Arguments<'_>) {
    use fmt::Write;
    let mut w = Writer::new(fd);
    let _ = w.write_fmt(args);
}

#[macro_export]
macro_rules! print {
    ($($t:tt)*) => { $crate::io::_print(1, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($t:tt)*) => { $crate::io::_print(1, format_args!("{}\n", format_args!($($t)*))) };
}
#[macro_export]
macro_rules! eprint {
    ($($t:tt)*) => { $crate::io::_print(2, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! eprintln {
    () => { $crate::eprint!("\n") };
    ($($t:tt)*) => { $crate::io::_print(2, format_args!("{}\n", format_args!($($t)*))) };
}
