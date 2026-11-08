//! `flint-version`: print the Flint Linux version.

use std::io::{self, Write};
use std::process::ExitCode;

use flint_core::version;

const USAGE: &str = "usage: flint-version [-s|--short | -b|--build | -h|--help]\n\
  (no option)    Flint Linux MAJOR.MINOR (build)\n\
  -s, --short    MAJOR.MINOR only\n\
  -b, --build    build id only\n";

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let mut out = io::stdout().lock();
    // Ignore write errors (e.g. closed pipe): this tool has nothing else to do.
    match arg.as_deref() {
        None => {
            let _ = writeln!(out, "{}", version::long());
            ExitCode::SUCCESS
        }
        Some("-s" | "--short") => {
            let _ = writeln!(out, "{}", version::NUMBER);
            ExitCode::SUCCESS
        }
        Some("-b" | "--build") => {
            let _ = writeln!(out, "{}", version::BUILD);
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") => {
            let _ = write!(out, "{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("flint-version: unknown option '{other}'\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
