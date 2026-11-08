//! Minimal logging to stderr (the console, for PID 1).

use std::fmt;
use std::io::Write;
use std::sync::OnceLock;

static TAG: OnceLock<&'static str> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Set the program tag shown in front of every line. First call wins.
pub fn init(tag: &'static str) {
    let _ = TAG.set(tag);
}

pub fn emit(level: Level, args: fmt::Arguments<'_>) {
    let tag = TAG.get().copied().unwrap_or("flint");
    let line = match level {
        Level::Info => format!("{tag}: {args}\n"),
        Level::Warn => format!("{tag}: warning: {args}\n"),
        Level::Error => format!("{tag}: error: {args}\n"),
    };
    // Logging must never fail the caller (stderr may be closed very early in boot).
    let _ = std::io::stderr().write_all(line.as_bytes());
}

#[macro_export]
macro_rules! flint_info {
    ($($t:tt)*) => { $crate::log::emit($crate::log::Level::Info, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! flint_warn {
    ($($t:tt)*) => { $crate::log::emit($crate::log::Level::Warn, format_args!($($t)*)) };
}
#[macro_export]
macro_rules! flint_err {
    ($($t:tt)*) => { $crate::log::emit($crate::log::Level::Error, format_args!($($t)*)) };
}
