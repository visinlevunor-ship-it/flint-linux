//! The `test` / `[` builtin: file tests, string tests, integer comparison, `! -a -o ( )`.

use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

pub fn evaluate(args: &[String]) -> Result<bool, String> {
    if args.is_empty() {
        return Ok(false);
    }
    let mut p = P { a: args, i: 0 };
    let v = p.or()?;
    if p.i != args.len() {
        return Err(format!("unexpected argument '{}'", args[p.i]));
    }
    Ok(v)
}

struct P<'a> {
    a: &'a [String],
    i: usize,
}

const UNARY: [&str; 18] = ["-e", "-f", "-d", "-r", "-w", "-x", "-s", "-L", "-h", "-z", "-n", "-b", "-c", "-p", "-S", "-t", "-u", "-g"];
const BINARY: [&str; 9] = ["=", "==", "!=", "-eq", "-ne", "-lt", "-le", "-gt", "-ge"];

impl P<'_> {
    fn peek(&self) -> Option<&str> {
        self.a.get(self.i).map(String::as_str)
    }

    fn or(&mut self) -> Result<bool, String> {
        let mut v = self.and()?;
        while self.peek() == Some("-o") {
            self.i += 1;
            let r = self.and()?;
            v = v || r;
        }
        Ok(v)
    }

    fn and(&mut self) -> Result<bool, String> {
        let mut v = self.not()?;
        while self.peek() == Some("-a") {
            self.i += 1;
            let r = self.not()?;
            v = v && r;
        }
        Ok(v)
    }

    fn not(&mut self) -> Result<bool, String> {
        if self.peek() == Some("!") && self.i + 1 < self.a.len() {
            self.i += 1;
            return Ok(!self.not()?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<bool, String> {
        let Some(tok) = self.peek().map(str::to_string) else { return Err("argument expected".into()) };
        self.i += 1;
        if tok == "(" {
            let v = self.or()?;
            if self.peek() != Some(")") {
                return Err("missing ')'".into());
            }
            self.i += 1;
            return Ok(v);
        }
        // `a OP b` wins when the middle argument is a binary operator and there is a right side.
        if let (Some(op), true) = (self.peek(), self.i + 1 < self.a.len()) {
            if BINARY.contains(&op) {
                let op = op.to_string();
                let rhs = self.a[self.i + 1].clone();
                self.i += 2;
                return binary(&tok, &op, &rhs);
            }
        }
        if UNARY.contains(&tok.as_str()) {
            if let Some(arg) = self.peek().map(str::to_string) {
                self.i += 1;
                return Ok(unary(&tok, &arg));
            }
        }
        Ok(!tok.is_empty())
    }
}

fn unary(op: &str, arg: &str) -> bool {
    let meta = || fs::metadata(arg);
    match op {
        "-z" => arg.is_empty(),
        "-n" => !arg.is_empty(),
        "-e" => meta().is_ok(),
        "-f" => meta().map_or(false, |m| m.is_file()),
        "-d" => meta().map_or(false, |m| m.is_dir()),
        "-s" => meta().map_or(false, |m| m.len() > 0),
        "-L" | "-h" => fs::symlink_metadata(arg).map_or(false, |m| m.file_type().is_symlink()),
        "-b" => meta().map_or(false, |m| m.file_type().is_block_device()),
        "-c" => meta().map_or(false, |m| m.file_type().is_char_device()),
        "-p" => meta().map_or(false, |m| m.file_type().is_fifo()),
        "-S" => meta().map_or(false, |m| m.file_type().is_socket()),
        "-u" => meta().map_or(false, |m| m.mode() & 0o4000 != 0),
        "-g" => meta().map_or(false, |m| m.mode() & 0o2000 != 0),
        "-r" => access(arg, libc::R_OK),
        "-w" => access(arg, libc::W_OK),
        "-x" => access(arg, libc::X_OK),
        "-t" => arg.parse::<i32>().map_or(false, |fd| {
            // SAFETY: isatty accepts any integer.
            unsafe { libc::isatty(fd) == 1 }
        }),
        _ => false,
    }
}

fn access(path: &str, mode: i32) -> bool {
    match std::ffi::CString::new(path) {
        // SAFETY: valid NUL-terminated string.
        Ok(c) => unsafe { libc::access(c.as_ptr(), mode) == 0 },
        Err(_) => false,
    }
}

fn binary(a: &str, op: &str, b: &str) -> Result<bool, String> {
    match op {
        "=" | "==" => Ok(a == b),
        "!=" => Ok(a != b),
        _ => {
            let x: i64 = a.trim().parse().map_err(|_| format!("integer expression expected: '{a}'"))?;
            let y: i64 = b.trim().parse().map_err(|_| format!("integer expression expected: '{b}'"))?;
            Ok(match op {
                "-eq" => x == y,
                "-ne" => x != y,
                "-lt" => x < y,
                "-le" => x <= y,
                "-gt" => x > y,
                _ => x >= y,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(a: &[&str]) -> Result<bool, String> {
        evaluate(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn strings() {
        assert_eq!(t(&[]), Ok(false));
        assert_eq!(t(&["x"]), Ok(true));
        assert_eq!(t(&[""]), Ok(false));
        assert_eq!(t(&["-n", "x"]), Ok(true));
        assert_eq!(t(&["-n", ""]), Ok(false));
        assert_eq!(t(&["-z", ""]), Ok(true));
        assert_eq!(t(&["a", "=", "a"]), Ok(true));
        assert_eq!(t(&["a", "!=", "a"]), Ok(false));
        assert_eq!(t(&["a", "==", "b"]), Ok(false));
        assert_eq!(t(&["-n"]), Ok(true), "a lone -n is just a non-empty string");
    }

    #[test]
    fn integers() {
        assert_eq!(t(&["3", "-lt", "10"]), Ok(true));
        assert_eq!(t(&["3", "-ge", "10"]), Ok(false));
        assert_eq!(t(&["5", "-eq", "5"]), Ok(true));
        assert_eq!(t(&["5", "-ne", "5"]), Ok(false));
        assert!(t(&["a", "-eq", "1"]).is_err());
    }

    #[test]
    fn logic_and_grouping() {
        assert_eq!(t(&["!", "a", "=", "b"]), Ok(true));
        assert_eq!(t(&["a", "=", "a", "-a", "b", "=", "b"]), Ok(true));
        assert_eq!(t(&["a", "=", "b", "-o", "b", "=", "b"]), Ok(true));
        assert_eq!(t(&["(", "a", "=", "b", "-o", "c", "=", "c", ")", "-a", "d", "=", "d"]), Ok(true));
        assert_eq!(t(&["!", "!", "x"]), Ok(true));
        assert!(t(&["(", "a"]).is_err());
        assert!(t(&["a", "b"]).is_err());
    }

    #[test]
    fn files() {
        assert_eq!(t(&["-d", "/"]), Ok(true));
        assert_eq!(t(&["-f", "/"]), Ok(false));
        assert_eq!(t(&["-e", "/nonexistent-flint-sh"]), Ok(false));
        assert_eq!(t(&["-e", "/proc/self/status"]), Ok(true));
        assert_eq!(t(&["-r", "/proc/self/status"]), Ok(true));
        assert_eq!(t(&["-x", "/proc/self/status"]), Ok(false));
        assert_eq!(t(&["-s", "/proc/self/status"]), Ok(false), "procfs files report size 0");
    }
}
