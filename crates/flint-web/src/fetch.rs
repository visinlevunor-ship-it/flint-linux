//! `fetch URL...`: download files over HTTP or HTTPS (named after FreeBSD's fetch).

use std::fs;
use std::io::{self, IsTerminal, Write};

use crate::http;

const USAGE: &str = "usage: fetch [-q] [-o FILE] URL...   (-o - writes to standard output)";

pub fn fetch(args: Vec<String>) -> i32 {
    let mut quiet = false;
    let mut out: Option<String> = None;
    let mut urls = Vec::new();
    let mut it = args.into_iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-q" => quiet = true,
            "-o" => match it.next() {
                Some(o) => out = Some(o),
                None => {
                    eprintln!("fetch: -o needs a file\n{USAGE}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            s if s.starts_with('-') => {
                eprintln!("fetch: unknown option {s}\n{USAGE}");
                return 2;
            }
            _ => urls.push(a),
        }
    }
    if urls.is_empty() || (out.is_some() && urls.len() > 1) {
        eprintln!("{USAGE}");
        return 2;
    }
    let tty = io::stderr().is_terminal();
    let mut status = 0;
    for u in &urls {
        let name = match &out {
            Some(o) => o.clone(),
            None => {
                let n = u.split('?').next().unwrap_or(u).trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
                if n.is_empty() || n.contains(':') {
                    "index.html".into()
                } else {
                    n
                }
            }
        };
        let mut last_shown = 0u64;
        let mut progress = |got: u64, total: Option<u64>| {
            if quiet || !tty || got < last_shown + 256 * 1024 {
                return;
            }
            last_shown = got;
            match total {
                Some(t) if t > 0 => eprint!("\r{name}: {} of {} KiB ({}%)", got / 1024, t / 1024, got * 100 / t),
                _ => eprint!("\r{name}: {} KiB", got / 1024),
            }
        };
        match http::get(u, 4 << 30, &mut progress) {
            Ok(r) if (200..300).contains(&r.status) => {
                if !quiet && tty && last_shown > 0 {
                    eprintln!();
                }
                let w = if name == "-" { io::stdout().write_all(&r.body).and_then(|_| io::stdout().flush()) } else { fs::write(&name, &r.body) };
                match w {
                    Ok(()) => {
                        if !quiet && name != "-" {
                            eprintln!("{name}: {} bytes", r.body.len());
                        }
                    }
                    Err(e) => {
                        eprintln!("fetch: {name}: {e}");
                        status = 1;
                    }
                }
            }
            Ok(r) => {
                eprintln!("fetch: {}: HTTP status {}", r.url, r.status);
                status = 1;
            }
            Err(e) => {
                eprintln!("fetch: {u}: {e}");
                status = 1;
            }
        }
    }
    status
}
