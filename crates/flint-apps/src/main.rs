//! `flint`: one binary for every Flint program.
//!
//! Each program is installed as a symlink with its normal name (`/bin/sh`, `/bin/pkg`,
//! `/sbin/init`, `/bin/ls`, ...) and is picked by the name it is started as. It can also be
//! run as `flint NAME [ARGS...]`. `su` and `passwd` are separate binaries because they are
//! setuid root, and this one must never be.

use std::path::Path;

/// Programs other than the core tools from flint-box.
const PROGRAMS: &[(&str, fn(Vec<String>) -> i32)] = &[
    ("devd", flint_dev::devd),
    ("fetch", flint_web::fetch),
    ("halt", flint_power::main),
    ("init", flint_init::main),
    ("login", flint_login::main),
    ("lsmod", flint_dev::lsmod),
    ("modload", flint_dev::modload),
    ("modprobe", flint_dev::modprobe),
    ("mountboot", flint_dev::mountboot),
    ("net", flint_net::net),
    ("netd", flint_net::netd),
    ("ping", flint_net::ping),
    ("pkg", flint_pkg::main),
    ("power", flint_power::main),
    ("poweroff", flint_power::main),
    ("reboot", flint_power::main),
    ("sh", flint_sh::main),
];

const USAGE: &str = "usage: flint NAME [ARGS...]   run one of Flint's programs
       flint --list                 list them
       flint --version
Each program is also installed under its own name, for example /bin/sh or /bin/pkg.";

/// Every program name, sorted.
fn all_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = PROGRAMS.iter().map(|(n, _)| *n).chain(flint_box::names()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Run `name`; `args[0]` is what the program sees as its own name.
fn dispatch(name: &str, args: Vec<String>) -> Option<i32> {
    if let Some((_, f)) = PROGRAMS.iter().find(|(n, _)| *n == name) {
        return Some(f(args));
    }
    flint_box::run(name, &args[1..])
}

/// `/bin/sh` -> `sh`, `-sh` (a login shell) -> `sh`.
fn program_name(argv0: &str) -> &str {
    let base = Path::new(argv0).file_name().and_then(|n| n.to_str()).unwrap_or(argv0);
    base.trim_start_matches('-')
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let argv0 = args.first().cloned().unwrap_or_else(|| "flint".to_string());
    let name = program_name(&argv0).to_string();

    let code = if name == "flint" {
        match args.get(1).map(String::as_str) {
            None | Some("-h" | "--help") => {
                println!("{USAGE}");
                if args.len() < 2 { 2 } else { 0 }
            }
            Some("--list") => {
                for n in all_names() {
                    println!("{n}");
                }
                0
            }
            Some("--version" | "version") => {
                println!("{}", flint_core::version::long());
                0
            }
            Some(prog) => {
                let prog = prog.to_string();
                dispatch(&prog, args[1..].to_vec()).unwrap_or_else(|| {
                    eprintln!("flint: no program called '{prog}' (flint --list shows them)");
                    127
                })
            }
        }
    } else {
        dispatch(&name, args).unwrap_or_else(|| {
            eprintln!("flint: no program called '{name}' (flint --list shows them)");
            127
        })
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_argv0() {
        assert_eq!(program_name("/bin/sh"), "sh");
        assert_eq!(program_name("-sh"), "sh");
        assert_eq!(program_name("/init"), "init");
        assert_eq!(program_name("pkg"), "pkg");
        assert_eq!(program_name("flint"), "flint");
    }

    /// apps.txt says where each symlink goes; the image script reads it.
    #[test]
    fn apps_txt_lists_every_program_once() {
        let mut listed: Vec<&str> = Vec::new();
        for line in include_str!("../apps.txt").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (dir, name) = line.split_once(char::is_whitespace).expect("DIR NAME");
            assert!(matches!(dir, "bin" | "sbin"), "{line}");
            listed.push(name.trim());
        }
        listed.sort_unstable();
        assert_eq!(listed, all_names());
    }
}
