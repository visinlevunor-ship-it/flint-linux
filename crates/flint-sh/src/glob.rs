//! Shell patterns. A pattern is a list of `(char, literal)` pairs: `literal` is true for
//! characters that were quoted, which therefore never act as `*`, `?` or `[`.

use std::fs;
use std::path::Path;

pub type Pat = Vec<(char, bool)>;

/// Every character active (nothing quoted).
#[cfg(test)]
pub fn active(s: &str) -> Pat {
    s.chars().map(|c| (c, false)).collect()
}

pub fn has_meta(p: &[(char, bool)]) -> bool {
    p.iter().any(|&(c, lit)| !lit && matches!(c, '*' | '?' | '['))
}

pub fn matches(p: &[(char, bool)], text: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    m(p, &t)
}

struct Class {
    items: Vec<(char, char)>,
    negate: bool,
    /// Index in the pattern just after the closing `]`.
    end: usize,
}

/// Parse a bracket expression starting at `p[0] == '['`. `None` if it is not terminated
/// (then the `[` is an ordinary character).
fn parse_class(p: &[(char, bool)]) -> Option<Class> {
    let mut j = 1;
    let mut negate = false;
    if j < p.len() && !p[j].1 && matches!(p[j].0, '!' | '^') {
        negate = true;
        j += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    while j < p.len() {
        let (c, lit) = p[j];
        if c == ']' && !lit && !first {
            return Some(Class { items, negate, end: j + 1 });
        }
        first = false;
        if j + 2 < p.len() && p[j + 1] == ('-', false) && !(p[j + 2].0 == ']' && !p[j + 2].1) {
            items.push((c, p[j + 2].0));
            j += 3;
        } else {
            items.push((c, c));
            j += 1;
        }
    }
    None
}

fn m(p: &[(char, bool)], t: &[char]) -> bool {
    let Some(&(c, lit)) = p.first() else { return t.is_empty() };
    if lit {
        return !t.is_empty() && t[0] == c && m(&p[1..], &t[1..]);
    }
    match c {
        '*' => {
            let mut rest = &p[1..];
            while rest.first() == Some(&('*', false)) {
                rest = &rest[1..];
            }
            (0..=t.len()).any(|k| m(rest, &t[k..]))
        }
        '?' => !t.is_empty() && m(&p[1..], &t[1..]),
        '[' => match parse_class(p) {
            Some(cl) => {
                !t.is_empty() && (cl.items.iter().any(|&(lo, hi)| lo <= t[0] && t[0] <= hi) != cl.negate) && m(&p[cl.end..], &t[1..])
            }
            None => !t.is_empty() && t[0] == '[' && m(&p[1..], &t[1..]),
        },
        _ => !t.is_empty() && t[0] == c && m(&p[1..], &t[1..]),
    }
}

/// `${text#pat}` / `${text##pat}`: remove the shortest (or longest) matching prefix.
pub fn strip_prefix(text: &str, pat: &[(char, bool)], longest: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let range: Vec<usize> = if longest { (0..=chars.len()).rev().collect() } else { (0..=chars.len()).collect() };
    for n in range {
        if m(pat, &chars[..n]) {
            return chars[n..].iter().collect();
        }
    }
    text.to_string()
}

/// `${text%pat}` / `${text%%pat}`: remove the shortest (or longest) matching suffix.
pub fn strip_suffix(text: &str, pat: &[(char, bool)], longest: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let range: Vec<usize> = if longest { (0..=chars.len()).collect() } else { (0..=chars.len()).rev().collect() };
    for n in range {
        if m(pat, &chars[n..]) {
            return chars[..n].iter().collect();
        }
    }
    text.to_string()
}

fn join(prefix: &str, name: &str) -> String {
    match prefix {
        "" => name.to_string(),
        "/" => format!("/{name}"),
        _ => format!("{prefix}/{name}"),
    }
}

/// Expand a pattern against the filesystem. Sorted matches; empty if nothing matches.
pub fn expand_path(p: &[(char, bool)]) -> Vec<String> {
    let absolute = p.first().map(|&(c, _)| c) == Some('/');
    let trailing_slash = p.len() > 1 && p.last().map(|&(c, _)| c) == Some('/');
    let comps: Vec<Pat> = p.split(|&(c, _)| c == '/').filter(|c| !c.is_empty()).map(<[_]>::to_vec).collect();
    let mut out = Vec::new();
    walk(if absolute { "/" } else { "" }, &comps, trailing_slash, &mut out);
    out.sort();
    out
}

fn walk(prefix: &str, comps: &[Pat], trailing_slash: bool, out: &mut Vec<String>) {
    let Some((comp, rest)) = comps.split_first() else {
        let ok = !prefix.is_empty() && fs::symlink_metadata(prefix).is_ok() && (!trailing_slash || Path::new(prefix).is_dir());
        if ok {
            out.push(if trailing_slash { format!("{prefix}/") } else { prefix.to_string() });
        }
        return;
    };
    if !has_meta(comp) {
        let name: String = comp.iter().map(|&(c, _)| c).collect();
        let next = join(prefix, &name);
        if rest.is_empty() || Path::new(&next).is_dir() {
            walk(&next, rest, trailing_slash, out);
        }
        return;
    }
    let dir = if prefix.is_empty() { "." } else { prefix };
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut names: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    names.sort();
    let dot_ok = comp.first().map(|&(c, _)| c) == Some('.');
    for name in names {
        if name.starts_with('.') && !dot_ok {
            continue;
        }
        if matches(comp, &name) {
            let next = join(prefix, &name);
            if rest.is_empty() || Path::new(&next).is_dir() {
                walk(&next, rest, trailing_slash, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mt(p: &str, t: &str) -> bool {
        matches(&active(p), t)
    }

    #[test]
    fn wildcards() {
        assert!(mt("*", ""));
        assert!(mt("*", "anything"));
        assert!(mt("a*c", "abc"));
        assert!(mt("a*c", "ac"));
        assert!(mt("a*c", "abcbc"));
        assert!(!mt("a*c", "abcd"));
        assert!(mt("?", "x"));
        assert!(!mt("?", ""));
        assert!(mt("*.txt", "notes.txt"));
        assert!(!mt("*.txt", "notes.tx"));
        assert!(mt("**a", "bba"));
        assert!(mt("a?c", "abc"));
    }

    #[test]
    fn classes() {
        assert!(mt("[abc]", "b"));
        assert!(!mt("[abc]", "d"));
        assert!(mt("[a-c]x", "bx"));
        assert!(mt("[!a-c]", "z"));
        assert!(!mt("[!a-c]", "b"));
        assert!(mt("[^0-9]", "x"));
        assert!(mt("[]]", "]"));
        assert!(mt("[a-]", "-"));
        assert!(mt("[", "["), "an unterminated [ is a plain character");
        assert!(mt("a[", "a["));
        assert!(mt("[0-9][0-9]", "42"));
    }

    #[test]
    fn quoted_characters_are_literal() {
        let mut p = active("a");
        p.push(('*', true));
        assert!(matches(&p, "a*"));
        assert!(!matches(&p, "abc"));
        assert!(!has_meta(&p));
        assert!(has_meta(&active("a*")));
    }

    #[test]
    fn prefix_and_suffix_removal() {
        let p = |s: &str| active(s);
        assert_eq!(strip_prefix("a/b/c", &p("*/"), false), "b/c");
        assert_eq!(strip_prefix("a/b/c", &p("*/"), true), "c");
        assert_eq!(strip_suffix("file.tar.gz", &p(".*"), false), "file.tar");
        assert_eq!(strip_suffix("file.tar.gz", &p(".*"), true), "file");
        assert_eq!(strip_prefix("abc", &p("x"), false), "abc");
        assert_eq!(strip_suffix("abc", &p("c"), false), "ab");
        assert_eq!(strip_suffix("abc", &p("*"), true), "");
    }

    #[test]
    fn filesystem_expansion() {
        let dir = std::env::temp_dir().join(format!("flint-sh-glob-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["a.txt", "b.txt", "c.rs", ".hidden", "sub/d.txt"] {
            fs::write(dir.join(f), "").unwrap();
        }
        let d = dir.to_str().unwrap();
        let expand = |s: &str| -> Vec<String> { expand_path(&active(&format!("{d}/{s}"))).iter().map(|p| p.strip_prefix(&format!("{d}/")).unwrap().to_string()).collect() };
        assert_eq!(expand("*.txt"), ["a.txt", "b.txt"]);
        assert_eq!(expand("*"), ["a.txt", "b.txt", "c.rs", "sub"], "hidden files are skipped");
        assert_eq!(expand(".*"), [".hidden"]);
        assert_eq!(expand("sub/*"), ["sub/d.txt"]);
        assert_eq!(expand("*/d.txt"), ["sub/d.txt"]);
        assert_eq!(expand("*/"), ["sub/"]);
        assert_eq!(expand("[ab].txt"), ["a.txt", "b.txt"]);
        assert!(expand("*.zip").is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
