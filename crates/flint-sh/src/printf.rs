//! The `printf` builtin's formatting: %s %d %i %u %x %X %o %c %b %% with flags, width and
//! precision, backslash escapes, and the format reused while arguments remain.

/// Decode backslash escapes. Returns the bytes and whether `\c` (stop) was seen.
///
/// `zero_prefixed` selects the octal rule: `true` for `echo -e` and `%b` (`\0NNN`, up to three
/// digits after the zero), `false` for a printf format string (`\NNN`, one to three digits).
pub fn unescape(s: &str, zero_prefixed: bool) -> (Vec<u8>, bool) {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match b[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'e' => out.push(0x1b),
            b'f' => out.push(0x0c),
            b'v' => out.push(0x0b),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            b'\'' => out.push(b'\''),
            b'c' => return (out, true),
            b'0'..=b'7' => {
                if zero_prefixed && b[i] != b'0' {
                    out.push(b'\\');
                    out.push(b[i]);
                } else {
                    if zero_prefixed {
                        i += 1; // the leading zero
                    }
                    let (mut v, mut n) = (0u32, 0);
                    while n < 3 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                        v = v * 8 + u32::from(b[i] - b'0');
                        i += 1;
                        n += 1;
                    }
                    out.push(v as u8);
                    continue;
                }
            }
            b'x' => {
                let (mut v, mut n) = (0u32, 0);
                while n < 2 && i + 1 < b.len() && b[i + 1].is_ascii_hexdigit() {
                    v = v * 16 + (b[i + 1] as char).to_digit(16).unwrap_or(0);
                    i += 1;
                    n += 1;
                }
                if n == 0 {
                    out.extend_from_slice(b"\\x");
                } else {
                    out.push(v as u8);
                }
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
        i += 1;
    }
    (out, false)
}

fn parse_int(s: &str) -> Result<i64, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(0);
    }
    if let Some(rest) = t.strip_prefix('\'').or_else(|| t.strip_prefix('"')) {
        return rest.chars().next().map(|c| c as i64).ok_or_else(|| format!("invalid number '{s}'"));
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(h, 16)
    } else if body.len() > 1 && body.starts_with('0') {
        i64::from_str_radix(&body[1..], 8)
    } else {
        body.parse::<i64>()
    };
    v.map(|v| if neg { -v } else { v }).map_err(|_| format!("invalid number '{s}'"))
}

struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    zero: bool,
    alt: bool,
    width: usize,
    prec: Option<usize>,
}

fn pad(s: String, spec: &Spec, numeric: bool) -> String {
    let len = s.chars().count();
    if len >= spec.width {
        return s;
    }
    let fill = spec.width - len;
    if spec.left {
        format!("{s}{}", " ".repeat(fill))
    } else if spec.zero && numeric {
        let (sign, digits) = if s.starts_with(['-', '+', ' ']) { s.split_at(1) } else { ("", s.as_str()) };
        format!("{sign}{}{digits}", "0".repeat(fill))
    } else {
        format!("{}{s}", " ".repeat(fill))
    }
}

/// Format once through `fmt`, taking arguments from `args` starting at `*next`.
/// Returns the output bytes and whether `\c` or `%b`'s `\c` stopped output.
fn one_pass(fmt: &str, args: &[String], next: &mut usize, errors: &mut Vec<String>) -> (Vec<u8>, bool) {
    let f: Vec<char> = fmt.chars().collect();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    let take = |next: &mut usize| -> Option<String> {
        let a = args.get(*next).cloned();
        if a.is_some() {
            *next += 1;
        }
        a
    };
    while i < f.len() {
        match f[i] {
            '\\' => {
                // collect the escape sequence text and decode it
                let start = i;
                i += 1;
                if i < f.len() {
                    i += 1;
                    if f[i - 1] == '0' || f[i - 1].is_digit(8) {
                        let mut n = 0;
                        while n < 2 && i < f.len() && f[i].is_digit(8) {
                            i += 1;
                            n += 1;
                        }
                    } else if f[i - 1] == 'x' {
                        let mut n = 0;
                        while n < 2 && i < f.len() && f[i].is_ascii_hexdigit() {
                            i += 1;
                            n += 1;
                        }
                    }
                }
                let seq: String = f[start..i].iter().collect();
                let (bytes, stop) = unescape(&seq, false);
                out.extend(bytes);
                if stop {
                    return (out, true);
                }
            }
            '%' => {
                i += 1;
                if i >= f.len() {
                    out.push(b'%');
                    break;
                }
                if f[i] == '%' {
                    out.push(b'%');
                    i += 1;
                    continue;
                }
                let mut spec = Spec { left: false, plus: false, space: false, zero: false, alt: false, width: 0, prec: None };
                while i < f.len() {
                    match f[i] {
                        '-' => spec.left = true,
                        '+' => spec.plus = true,
                        ' ' => spec.space = true,
                        '0' => spec.zero = true,
                        '#' => spec.alt = true,
                        _ => break,
                    }
                    i += 1;
                }
                while i < f.len() && f[i].is_ascii_digit() {
                    spec.width = spec.width * 10 + f[i].to_digit(10).unwrap_or(0) as usize;
                    i += 1;
                }
                if i < f.len() && f[i] == '.' {
                    i += 1;
                    let mut p = 0;
                    while i < f.len() && f[i].is_ascii_digit() {
                        p = p * 10 + f[i].to_digit(10).unwrap_or(0) as usize;
                        i += 1;
                    }
                    spec.prec = Some(p);
                }
                let Some(&conv) = f.get(i) else {
                    out.extend(b"%");
                    break;
                };
                i += 1;
                match conv {
                    's' | 'b' => {
                        let arg = take(next).unwrap_or_default();
                        let mut stop = false;
                        let mut s = if conv == 'b' {
                            let (bytes, st) = unescape(&arg, true);
                            stop = st;
                            String::from_utf8_lossy(&bytes).into_owned()
                        } else {
                            arg
                        };
                        if let Some(p) = spec.prec {
                            s = s.chars().take(p).collect();
                        }
                        out.extend(pad(s, &spec, false).bytes());
                        if stop {
                            return (out, true);
                        }
                    }
                    'c' => {
                        let arg = take(next).unwrap_or_default();
                        let s: String = arg.chars().take(1).collect();
                        out.extend(pad(s, &spec, false).bytes());
                    }
                    'd' | 'i' | 'u' | 'x' | 'X' | 'o' => {
                        let arg = take(next).unwrap_or_default();
                        let n = match parse_int(&arg) {
                            Ok(n) => n,
                            Err(e) => {
                                errors.push(e);
                                0
                            }
                        };
                        let mut body = match conv {
                            'd' | 'i' => n.unsigned_abs().to_string(),
                            'u' => (n as u64).to_string(),
                            'x' => format!("{:x}", n as u64),
                            'X' => format!("{:X}", n as u64),
                            _ => format!("{:o}", n as u64),
                        };
                        if let Some(p) = spec.prec {
                            while body.len() < p {
                                body.insert(0, '0');
                            }
                        }
                        if spec.alt {
                            match conv {
                                'x' if n != 0 => body.insert_str(0, "0x"),
                                'X' if n != 0 => body.insert_str(0, "0X"),
                                'o' if !body.starts_with('0') => body.insert(0, '0'),
                                _ => {}
                            }
                        }
                        let signed = matches!(conv, 'd' | 'i');
                        let sign = if signed && n < 0 {
                            "-"
                        } else if signed && spec.plus {
                            "+"
                        } else if signed && spec.space {
                            " "
                        } else {
                            ""
                        };
                        let zero_ok = spec.prec.is_none();
                        let spec2 = Spec { zero: spec.zero && zero_ok, ..spec };
                        out.extend(pad(format!("{sign}{body}"), &spec2, true).bytes());
                    }
                    other => {
                        errors.push(format!("invalid format character '%{other}'"));
                        return (out, true);
                    }
                }
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend(c.encode_utf8(&mut buf).bytes());
                i += 1;
            }
        }
    }
    (out, false)
}

/// Format `fmt` with `args`. The format is reused while arguments remain. The second value
/// lists non-fatal problems (bad numbers) to report on stderr.
pub fn format(fmt: &str, args: &[String]) -> (Vec<u8>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut next = 0;
    loop {
        let before = next;
        let (bytes, stop) = one_pass(fmt, args, &mut next, &mut errors);
        out.extend(bytes);
        if stop || next >= args.len() || next == before {
            break;
        }
    }
    (out, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(fmt: &str, args: &[&str]) -> String {
        let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        String::from_utf8(format(fmt, &a).0).unwrap()
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(f("hello\\n", &[]), "hello\n");
        assert_eq!(f("%s-%s", &["a", "b"]), "a-b");
        assert_eq!(f("%5s|%-5s|", &["ab", "cd"]), "   ab|cd   |");
        assert_eq!(f("%.2s", &["abcdef"]), "ab");
        assert_eq!(f("%s", &[]), "");
        assert_eq!(f("100%%", &[]), "100%");
        assert_eq!(f("a\\tb\\x41\\102", &[]), "a\tbAB", "printf: \\NNN is octal");
        assert_eq!(f("\\0102", &[]), "\u{8}2", "printf: \\0102 is \\010 followed by 2");
        assert_eq!(f("%b", &["\\0102"]), "B", "%b: \\0NNN");
        assert_eq!(f("%c", &["xyz"]), "x");
        assert_eq!(f("%b", &["a\\nb"]), "a\nb");
        assert_eq!(f("one\\ctwo", &[]), "one");
    }

    #[test]
    fn numbers() {
        assert_eq!(f("%d", &["42"]), "42");
        assert_eq!(f("%d", &["-7"]), "-7");
        assert_eq!(f("%05d", &["42"]), "00042");
        assert_eq!(f("%5d|%-5d|", &["42", "42"]), "   42|42   |");
        assert_eq!(f("%+d", &["5"]), "+5");
        assert_eq!(f("%x %X %o", &["255", "255", "8"]), "ff FF 10");
        assert_eq!(f("%#x", &["255"]), "0xff");
        assert_eq!(f("%d", &["0x10"]), "16");
        assert_eq!(f("%d", &["010"]), "8");
        assert_eq!(f("%d", &["'A"]), "65");
        assert_eq!(f("%u", &["3"]), "3");
        assert_eq!(f("%.3d", &["5"]), "005");
        assert_eq!(f("%d", &[]), "0");
        let (out, errs) = format("%d", &["abc".to_string()]);
        assert_eq!(out, b"0");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn format_is_reused_while_arguments_remain() {
        assert_eq!(f("%s\\n", &["a", "b", "c"]), "a\nb\nc\n");
        assert_eq!(f("%s=%s;", &["k", "v", "k2", "v2"]), "k=v;k2=v2;");
        assert_eq!(f("%s %s\\n", &["a", "b", "c"]), "a b\nc \n");
        assert_eq!(f("no conversions", &["x"]), "no conversions", "no infinite loop without conversions");
    }
}
