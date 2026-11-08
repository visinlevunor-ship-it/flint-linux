//! `flint-version`: print the Flint Linux version. Built on flint-rt, no libc.

#![no_std]
#![no_main]

use flint_core::version;
use flint_rt::{args, eprintln, entry, println};

entry!(main);

const USAGE: &str = r"usage: flint-version [option]
  (no option)      Flint Linux MAJOR.MINOR (build)
  -s, --short      MAJOR.MINOR only
  -b, --build      build id only
      --rt-selftest  exercise the flint-rt runtime (allocator, mem*, syscalls)
";

fn main() -> i32 {
    match args().nth(1) {
        None => println!("{}", version::Long),
        Some("-s" | "--short") => println!("{}", version::NUMBER),
        Some("-b" | "--build") => println!("{}", version::BUILD),
        Some("-h" | "--help") => flint_rt::print!("{USAGE}"),
        Some("--rt-selftest") => {
            let failures = flint_rt::selftest::run();
            if failures == 0 {
                println!("selftest: all passed");
            } else {
                eprintln!("selftest: {failures} failed");
                return 1;
            }
        }
        Some(other) => {
            eprintln!("flint-version: unknown option '{other}'\n{USAGE}");
            return 2;
        }
    }
    0
}
