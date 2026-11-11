//! Version comparison and dependency constraints (`name`, `name>=1.2`, `name=1.0`).

use std::cmp::Ordering;

#[derive(Debug, PartialEq, Eq)]
enum Seg {
    Num(String),
    Alpha(String),
}

/// Split into runs of digits and runs of letters; every other character is a separator.
fn segments(s: &str) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digits: Option<bool> = None;
    let mut flush = |cur: &mut String, digits: Option<bool>| {
        if !cur.is_empty() {
            let text = std::mem::take(cur);
            out.push(if digits == Some(true) { Seg::Num(text) } else { Seg::Alpha(text) });
        }
    };
    for c in s.chars() {
        if c.is_ascii_digit() {
            if digits == Some(false) {
                flush(&mut cur, digits);
            }
            cur.push(c);
            digits = Some(true);
        } else if c.is_ascii_alphabetic() {
            if digits == Some(true) {
                flush(&mut cur, digits);
            }
            cur.push(c);
            digits = Some(false);
        } else {
            flush(&mut cur, digits);
            digits = None;
        }
    }
    flush(&mut cur, digits);
    out
}

fn cmp_num(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Compare two versions. Numbers compare numerically (`1.10 > 1.9`), a number beats letters
/// at the same position (`1.0.1 > 1.0a`), and a trailing letter segment marks a
/// pre-release (`1.0rc1 < 1.0`).
pub fn compare(a: &str, b: &str) -> Ordering {
    let (x, y) = (segments(a), segments(b));
    for (p, q) in x.iter().zip(&y) {
        let o = match (p, q) {
            (Seg::Num(m), Seg::Num(n)) => cmp_num(m, n),
            (Seg::Alpha(m), Seg::Alpha(n)) => m.cmp(n),
            (Seg::Num(_), Seg::Alpha(_)) => Ordering::Greater,
            (Seg::Alpha(_), Seg::Num(_)) => Ordering::Less,
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    match x.len().cmp(&y.len()) {
        Ordering::Equal => Ordering::Equal,
        Ordering::Greater => {
            if matches!(x[y.len()], Seg::Alpha(_)) {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        Ordering::Less => {
            if matches!(y[x.len()], Seg::Alpha(_)) {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Ge,
    Le,
    Gt,
    Lt,
    Eq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    pub name: String,
    pub req: Option<(Op, String)>,
}

pub fn valid_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase() || f.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '.' | '_' | '-'))
}

pub fn valid_version(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_' | '~'))
}

impl Constraint {
    pub fn parse(s: &str) -> Result<Constraint, String> {
        let s = s.trim();
        let pos = s.find(['>', '<', '=']);
        let (name, req) = match pos {
            None => (s, None),
            Some(i) => {
                let name = s[..i].trim();
                let rest = &s[i..];
                let (op, ver) = if let Some(v) = rest.strip_prefix(">=") {
                    (Op::Ge, v)
                } else if let Some(v) = rest.strip_prefix("<=") {
                    (Op::Le, v)
                } else if let Some(v) = rest.strip_prefix('>') {
                    (Op::Gt, v)
                } else if let Some(v) = rest.strip_prefix('<') {
                    (Op::Lt, v)
                } else if let Some(v) = rest.strip_prefix('=') {
                    (Op::Eq, v)
                } else {
                    return Err(format!("bad constraint '{s}'"));
                };
                let ver = ver.trim();
                if !valid_version(ver) {
                    return Err(format!("bad version in constraint '{s}'"));
                }
                (name, Some((op, ver.to_string())))
            }
        };
        if !valid_name(name) {
            return Err(format!("bad package name in '{s}'"));
        }
        Ok(Constraint { name: name.to_string(), req })
    }

    pub fn satisfied_by(&self, version: &str) -> bool {
        match &self.req {
            None => true,
            Some((op, v)) => {
                let o = compare(version, v);
                match op {
                    Op::Ge => o != Ordering::Less,
                    Op::Le => o != Ordering::Greater,
                    Op::Gt => o == Ordering::Greater,
                    Op::Lt => o == Ordering::Less,
                    Op::Eq => o == Ordering::Equal,
                }
            }
        }
    }
}

impl std::fmt::Display for Constraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.req {
            None => write!(f, "{}", self.name),
            Some((op, v)) => {
                let o = match op {
                    Op::Ge => ">=",
                    Op::Le => "<=",
                    Op::Gt => ">",
                    Op::Lt => "<",
                    Op::Eq => "=",
                };
                write!(f, "{}{o}{v}", self.name)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    #[test]
    fn ordering() {
        for (a, b, want) in [
            ("1.0", "1.0", Equal),
            ("1.0", "1.0.1", Less),
            ("1.0.1", "1.1", Less),
            ("1.9", "1.10", Less),
            ("1.10", "2", Less),
            ("2", "1.99", Greater),
            ("1.01", "1.1", Equal),
            ("1.0rc1", "1.0", Less),
            ("1.0rc1", "1.0rc2", Less),
            ("1.0a", "1.0.1", Less),
            ("1.0.1", "1.0a", Greater),
            ("1.0-1", "1.0-2", Less),
            ("0.9.9", "1.0", Less),
            ("20240101", "20231231", Greater),
            ("1.0", "1.0.0", Less),
        ] {
            assert_eq!(compare(a, b), want, "{a} vs {b}");
            assert_eq!(compare(b, a), want.reverse(), "{b} vs {a}");
        }
    }

    #[test]
    fn constraints() {
        let c = Constraint::parse("foo>=1.2").unwrap();
        assert_eq!(c.name, "foo");
        assert!(c.satisfied_by("1.2") && c.satisfied_by("1.10") && c.satisfied_by("2"));
        assert!(!c.satisfied_by("1.1") && !c.satisfied_by("1.2rc1"));
        assert!(Constraint::parse("foo=1.0").unwrap().satisfied_by("1.0"));
        assert!(!Constraint::parse("foo=1.0").unwrap().satisfied_by("1.0.1"));
        assert!(Constraint::parse("foo<2").unwrap().satisfied_by("1.9"));
        assert!(!Constraint::parse("foo<2").unwrap().satisfied_by("2"));
        assert!(Constraint::parse("foo>1").unwrap().satisfied_by("1.1"));
        assert!(Constraint::parse("foo<=1").unwrap().satisfied_by("1"));
        assert!(Constraint::parse("foo").unwrap().satisfied_by("anything"));
        assert!(Constraint::parse(" foo >= 1.0 ").unwrap().satisfied_by("1.0"));
        assert_eq!(Constraint::parse("foo>=1.2").unwrap().to_string(), "foo>=1.2");
        for bad in ["", "Foo", "foo>=", "foo>=a b", "-foo", "foo bar", "foo=>1"] {
            assert!(Constraint::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn names_and_versions() {
        assert!(valid_name("lib-foo_2.0+x"));
        assert!(!valid_name("Foo"));
        assert!(!valid_name(""));
        assert!(!valid_name("a/b"));
        assert!(valid_version("1.0.0-rc1+git~abc"));
        assert!(!valid_version(""));
        assert!(!valid_version("1 0"));
    }
}
