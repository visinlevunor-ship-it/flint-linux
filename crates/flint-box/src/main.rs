//! flint-box: a multicall binary. Run it as `flint-box <applet> args...`, or through a
//! symlink named after the applet (`ln -s flint-box ls`).

mod applets;
mod util;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let invoked = args
        .first()
        .and_then(|a| Path::new(a).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("flint-box")
        .to_string();

    let (name, rest): (String, &[String]) = if invoked == "flint-box" {
        match args.get(1).map(String::as_str) {
            None | Some("-h" | "--help") => {
                eprintln!("usage: flint-box <applet> [args...]\n       flint-box --list\n(or run through a symlink named after an applet)");
                return ExitCode::from(if args.len() < 2 { 2 } else { 0 });
            }
            Some("--list") => {
                for (n, _) in applets::TABLE {
                    println!("{n}");
                }
                return ExitCode::SUCCESS;
            }
            Some(a) => (a.to_string(), &args[2..]),
        }
    } else {
        (invoked, &args[1..])
    };

    match applets::find(&name) {
        Some(f) => ExitCode::from(f(rest).clamp(0, 255) as u8),
        None => {
            eprintln!("flint-box: unknown applet '{name}'");
            ExitCode::from(127)
        }
    }
}
