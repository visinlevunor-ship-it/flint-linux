//! flint-mkimage: build tool for Flint images. Runs on the build machine.

mod cpio;

use std::fs;
use std::io::BufWriter;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: flint-mkimage cpio DIR OUTPUT        pack DIR as a newc cpio archive (owned by root)";

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("cpio") => {
            let [_, dir, out] = args else { return Err(USAGE.into()) };
            let f = fs::File::create(out).map_err(|e| format!("{out}: {e}"))?;
            let mut w = cpio::Writer::new(BufWriter::new(f));
            cpio::add_tree(&mut w, Path::new(dir), "").map_err(|e| format!("{dir}: {e}"))?;
            w.finish().map_err(|e| format!("{out}: {e}"))?;
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flint-mkimage: {e}");
            ExitCode::FAILURE
        }
    }
}
