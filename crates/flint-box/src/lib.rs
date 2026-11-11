//! The core tools (`ls`, `cat`, `cp`, `mount`, ...), run through the `flint` multicall binary.

mod applets;
pub mod regex;
mod util;

pub use applets::TABLE;

/// Names of all tools, in order.
pub fn names() -> impl Iterator<Item = &'static str> {
    applets::TABLE.iter().map(|(n, _)| *n)
}

/// Run tool `name` with `args` (the arguments after the program name).
/// `None` if there is no such tool.
pub fn run(name: &str, args: &[String]) -> Option<i32> {
    applets::find(name).map(|f| f(args).clamp(0, 255))
}
