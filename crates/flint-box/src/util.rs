//! Shared helpers: option parsing, error output, password prompts.

use std::fmt::Display;
use std::io;

pub fn error(prog: &str, msg: impl Display) {
    eprintln!("{prog}: {msg}");
}

/// Result of [`parse`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    flags: Vec<char>,
    values: Vec<(char, String)>,
    pub operands: Vec<String>,
}

impl Parsed {
    pub fn has(&self, c: char) -> bool {
        self.flags.contains(&c)
    }

    /// The last value given for option `c`.
    pub fn value(&self, c: char) -> Option<&str> {
        self.values.iter().rev().find(|(k, _)| *k == c).map(|(_, v)| v.as_str())
    }
}

/// Parse short options. `flags` lists plain switches, `valued` lists options taking a
/// value (`-n5` or `-n 5`). Switches may be combined (`-la`). `--` ends options and `-`
/// alone is an operand. Options may appear after operands.
pub fn parse(args: &[String], flags: &str, valued: &str) -> Result<Parsed, String> {
    let mut p = Parsed::default();
    let mut i = 0;
    let mut only_operands = false;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if only_operands || a == "-" || !a.starts_with('-') {
            p.operands.push(a.clone());
            continue;
        }
        if a == "--" {
            only_operands = true;
            continue;
        }
        let chars: Vec<char> = a[1..].chars().collect();
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            j += 1;
            if valued.contains(c) {
                let rest: String = chars[j..].iter().collect();
                let value = if !rest.is_empty() {
                    rest
                } else if i < args.len() {
                    i += 1;
                    args[i - 1].clone()
                } else {
                    return Err(format!("option requires an argument -- '{c}'"));
                };
                p.values.push((c, value));
                break;
            } else if flags.contains(c) {
                p.flags.push(c);
            } else {
                return Err(format!("invalid option -- '{c}'"));
            }
        }
    }
    Ok(p)
}

/// Report a write error to stdout. A closed pipe (`ls | head`) is not an error.
pub fn finish_output(prog: &str, r: io::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            error(prog, format!("write error: {e}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn combined_flags_and_operands() {
        let p = parse(&v(&["-la", "dir", "-1", "file"]), "la1", "").unwrap();
        assert!(p.has('l') && p.has('a') && p.has('1'));
        assert_eq!(p.operands, vec!["dir", "file"]);
    }

    #[test]
    fn valued_options_both_forms() {
        let p = parse(&v(&["-n5", "x"]), "", "n").unwrap();
        assert_eq!(p.value('n'), Some("5"));
        let p = parse(&v(&["-n", "7", "x"]), "", "n").unwrap();
        assert_eq!(p.value('n'), Some("7"));
        assert_eq!(p.operands, vec!["x"]);
        let p = parse(&v(&["-n", "1", "-n", "2"]), "", "n").unwrap();
        assert_eq!(p.value('n'), Some("2"));
    }

    #[test]
    fn dash_and_double_dash() {
        let p = parse(&v(&["-", "--", "-l"]), "l", "").unwrap();
        assert_eq!(p.operands, vec!["-", "-l"]);
        assert!(!p.has('l'));
    }

    #[test]
    fn errors() {
        assert!(parse(&v(&["-z"]), "l", "").is_err());
        assert!(parse(&v(&["-n"]), "", "n").is_err());
    }
}
