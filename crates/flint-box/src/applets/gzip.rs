//! gzip, gunzip, zcat.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;

use flint_gzip::{compress, decompress, Level};

use crate::util::error;

const LIMIT: usize = 1 << 40;

struct Opts {
    decompress: bool,
    stdout: bool,
    keep: bool,
    force: bool,
    level: Level,
    files: Vec<String>,
}

fn parse(name: &str, args: &[String], mut o: Opts) -> Result<Opts, String> {
    let mut only_files = false;
    for a in args {
        if only_files || a == "-" || !a.starts_with('-') {
            o.files.push(a.clone());
            continue;
        }
        if a == "--" {
            only_files = true;
            continue;
        }
        match a.as_str() {
            "--stdout" | "--to-stdout" => o.stdout = true,
            "--decompress" | "--uncompress" => o.decompress = true,
            "--keep" => o.keep = true,
            "--force" => o.force = true,
            "--fast" => o.level = Level::FAST,
            "--best" => o.level = Level::BEST,
            _ if a.starts_with("--") => return Err(format!("{name}: unknown option {a}")),
            _ => {
                for c in a[1..].chars() {
                    match c {
                        'c' => o.stdout = true,
                        'd' => o.decompress = true,
                        'k' => o.keep = true,
                        'f' => o.force = true,
                        'n' | 'q' => {}
                        '1'..='9' => o.level = Level::from_number(c.to_digit(10).unwrap_or(6)),
                        _ => return Err(format!("{name}: unknown option -{c}\nusage: gzip [-cdfk1-9] [FILE...]")),
                    }
                }
            }
        }
    }
    Ok(o)
}

fn run(name: &str, args: &[String], decompress_default: bool, stdout_default: bool) -> i32 {
    let o = Opts { decompress: decompress_default, stdout: stdout_default, keep: false, force: false, level: Level::DEFAULT, files: Vec::new() };
    let o = match parse(name, args, o) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let files = if o.files.is_empty() { vec!["-".to_string()] } else { o.files.clone() };
    let mut status = 0;
    for f in &files {
        if let Err(e) = one(name, &o, f) {
            error(name, e);
            status = 1;
        }
    }
    status
}

fn one(name: &str, o: &Opts, f: &str) -> Result<(), String> {
    let input = if f == "-" {
        let mut v = Vec::new();
        io::stdin().lock().read_to_end(&mut v).map_err(|e| format!("stdin: {e}"))?;
        v
    } else {
        fs::read(f).map_err(|e| format!("{f}: {e}"))?
    };
    if !o.decompress && !o.stdout && f != "-" && f.ends_with(".gz") && !o.force {
        return Err(format!("{f} already has the .gz suffix"));
    }
    let output = if o.decompress {
        decompress(&input, LIMIT).map_err(|e| format!("{f}: {e}"))?
    } else {
        if f == "-" && !o.stdout && is_tty(1) && !o.force {
            return Err("compressed data not written to a terminal (use -f to force)".into());
        }
        compress(&input, o.level)
    };
    if o.stdout || f == "-" {
        let mut out = io::stdout().lock();
        return out.write_all(&output).and_then(|_| out.flush()).map_err(|e| e.to_string());
    }
    let target = if o.decompress {
        match f.strip_suffix(".gz").or_else(|| f.strip_suffix(".tgz").map(|_| "")) {
            Some("") => format!("{}.tar", &f[..f.len() - 4]),
            Some(t) => t.to_string(),
            None => return Err(format!("{f}: unknown suffix, ignored (use {name} -c to write to stdout)")),
        }
    } else {
        format!("{f}.gz")
    };
    if !o.force && fs::symlink_metadata(&target).is_ok() {
        return Err(format!("{target} already exists (use -f to overwrite)"));
    }
    let mode = fs::metadata(f).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o644);
    fs::write(&target, &output).map_err(|e| format!("{target}: {e}"))?;
    let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode));
    if !o.keep {
        fs::remove_file(f).map_err(|e| format!("{f}: {e}"))?;
    }
    Ok(())
}

fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

pub fn gzip(args: &[String]) -> i32 {
    run("gzip", args, false, false)
}

pub fn gunzip(args: &[String]) -> i32 {
    run("gunzip", args, true, false)
}

pub fn zcat(args: &[String]) -> i32 {
    run("zcat", args, true, true)
}
