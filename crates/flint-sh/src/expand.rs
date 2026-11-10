//! Word expansion: tilde, parameters (`$x ${x:-y} ${x#p}`), command substitution, arithmetic,
//! field splitting, pathname expansion and quote removal.

use crate::arith;
use crate::glob::{self, Pat};
use crate::shell::{Flow, Shell};
use crate::util::errln;

/// Characters with a flag saying whether they were quoted (and so are never special).
type Chars = Vec<(char, bool)>;

#[derive(Default)]
struct Fields {
    done: Vec<Chars>,
    cur: Chars,
    /// A field exists even if it is empty (`""`).
    open: bool,
    /// `"$@"` with no parameters produces no field at all.
    suppress_open: bool,
}

impl Fields {
    fn push(&mut self, c: char, quoted: bool) {
        self.cur.push((c, quoted));
        self.open = true;
    }

    fn push_str(&mut self, s: &str, quoted: bool) {
        for c in s.chars() {
            self.push(c, quoted);
        }
    }

    fn end_field(&mut self, force: bool) {
        if self.open || force {
            self.done.push(std::mem::take(&mut self.cur));
            self.open = false;
        }
    }

    /// Add the result of an unquoted expansion, splitting on IFS.
    fn split_in(&mut self, s: &str, ifs: &str) {
        for c in s.chars() {
            if ifs.contains(c) {
                self.end_field(!c.is_whitespace());
            } else {
                self.push(c, false);
            }
        }
    }

    fn finish(mut self) -> Vec<Chars> {
        self.end_field(false);
        self.done
    }
}

enum Value {
    One(String),
    /// `$@` (star = false) or `$*` (star = true)
    Many { items: Vec<String>, star: bool },
}

/// Index of the `)` matching the `(` at `open`.
fn match_paren(c: &[char], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < c.len() {
        match c[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            '\\' => i += 1,
            '\'' => {
                i += 1;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    c.len()
}

/// Index of the `}` matching the `{` at `open`.
fn match_brace(c: &[char], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < c.len() {
        match c[i] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            '\\' => i += 1,
            '\'' => {
                i += 1;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    c.len()
}

/// The command inside `` `...` `` starting at the opening backtick, and the index after the closing one.
fn backtick_body(c: &[char], open: usize) -> (String, usize) {
    let mut s = String::new();
    let mut i = open + 1;
    while i < c.len() && c[i] != '`' {
        if c[i] == '\\' && i + 1 < c.len() && matches!(c[i + 1], '$' | '`' | '\\') {
            s.push(c[i + 1]);
            i += 2;
        } else {
            s.push(c[i]);
            i += 1;
        }
    }
    (s, (i + 1).min(c.len()))
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Split `x:-word` into (`x`, `:-word`): a special parameter, a number, or an identifier.
fn split_name(s: &str) -> (&str, &str) {
    let mut it = s.char_indices();
    let Some((_, first)) = it.next() else { return ("", "") };
    let end = if matches!(first, '?' | '$' | '!' | '@' | '*' | '-' | '#') {
        first.len_utf8()
    } else if first.is_ascii_digit() {
        s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len())
    } else if is_name_start(first) {
        s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(s.len())
    } else {
        0
    };
    s.split_at(end)
}

impl Shell {
    fn ifs(&self) -> String {
        self.get("IFS").unwrap_or(" \t\n").to_string()
    }

    /// The value of a parameter, or `None` if unset.
    pub fn param(&self, name: &str) -> Option<String> {
        match name {
            "?" => Some(self.last_status.to_string()),
            "$" => Some(self.pid.to_string()),
            "!" => (self.last_bg != 0).then(|| self.last_bg.to_string()),
            "#" => Some(self.positional.len().to_string()),
            "0" => Some(self.arg0.clone()),
            "-" => Some(String::new()),
            _ if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) => {
                let n: usize = name.parse().ok()?;
                n.checked_sub(1).and_then(|i| self.positional.get(i)).cloned()
            }
            _ => self.get(name).map(str::to_string),
        }
    }

    /// Expand one word into fields of (char, quoted). With `split` false everything stays in
    /// one field and nothing is split on IFS.
    fn expand_chars(&mut self, raw: &str, split: bool) -> Vec<Chars> {
        let c: Vec<char> = raw.chars().collect();
        let mut f = Fields::default();
        let mut i = 0;
        if c.first() == Some(&'~') && (c.len() == 1 || c[1] == '/') {
            let home = self.get("HOME").unwrap_or("/").to_string();
            f.push_str(&home, true);
            i = 1;
        }
        while i < c.len() {
            match c[i] {
                '\\' => {
                    if i + 1 < c.len() {
                        f.push(c[i + 1], true);
                        i += 2;
                    } else {
                        f.push('\\', true);
                        i += 1;
                    }
                }
                '\'' => {
                    f.open = true;
                    i += 1;
                    while i < c.len() && c[i] != '\'' {
                        f.push(c[i], true);
                        i += 1;
                    }
                    i += 1;
                }
                '"' => i = self.expand_dq(&c, i + 1, &mut f, true),
                '$' => i = self.expand_dollar(&c, i, &mut f, false, split),
                '`' => {
                    let (src, next) = backtick_body(&c, i);
                    let result = self.command_subst(&src);
                    let ifs = self.ifs();
                    if split {
                        f.split_in(&result, &ifs);
                    } else {
                        f.push_str(&result, false);
                    }
                    i = next;
                }
                ch => {
                    f.push(ch, false);
                    i += 1;
                }
            }
        }
        f.finish()
    }

    /// Inside double quotes (or a here-document when `stop_at_quote` is false). `i` is just
    /// after the opening quote; returns the index after the closing one.
    fn expand_dq(&mut self, c: &[char], mut i: usize, f: &mut Fields, stop_at_quote: bool) -> usize {
        while i < c.len() {
            let ch = c[i];
            if ch == '"' && stop_at_quote {
                i += 1;
                break;
            }
            match ch {
                '\\' => match c.get(i + 1) {
                    Some(&n) if matches!(n, '$' | '`' | '"' | '\\') => {
                        f.push(n, true);
                        i += 2;
                    }
                    Some(&'\n') => i += 2,
                    _ => {
                        f.push('\\', true);
                        i += 1;
                    }
                },
                '$' => i = self.expand_dollar(c, i, f, true, stop_at_quote),
                '`' => {
                    let (src, next) = backtick_body(c, i);
                    let result = self.command_subst(&src);
                    f.push_str(&result, true);
                    i = next;
                }
                _ => {
                    f.push(ch, true);
                    i += 1;
                }
            }
        }
        if stop_at_quote && !f.suppress_open {
            f.open = true;
        }
        f.suppress_open = false;
        i
    }

    /// At a `$`. Returns the index after the expansion.
    fn expand_dollar(&mut self, c: &[char], i: usize, f: &mut Fields, in_dq: bool, split: bool) -> usize {
        let value;
        let next;
        match c.get(i + 1).copied() {
            Some('(') => {
                let end = match_paren(c, i + 1);
                next = (end + 1).min(c.len());
                if c.get(i + 2) == Some(&'(') && end >= i + 4 && c[end - 1] == ')' {
                    let expr: String = c[i + 3..end - 1].iter().collect();
                    value = Value::One(self.arith_value(&expr));
                } else {
                    let body: String = c[i + 2..end.min(c.len())].iter().collect();
                    value = Value::One(self.command_subst(&body));
                }
            }
            Some('{') => {
                let end = match_brace(c, i + 1);
                next = (end + 1).min(c.len());
                let inner: Vec<char> = c[i + 2..end.min(c.len())].to_vec();
                value = self.param_expand(&inner);
            }
            Some(ch) if is_name_start(ch) => {
                let mut j = i + 1;
                while j < c.len() && (c[j].is_ascii_alphanumeric() || c[j] == '_') {
                    j += 1;
                }
                let name: String = c[i + 1..j].iter().collect();
                value = Value::One(self.param(&name).unwrap_or_default());
                next = j;
            }
            Some(ch) if ch.is_ascii_digit() => {
                value = Value::One(self.param(&ch.to_string()).unwrap_or_default());
                next = i + 2;
            }
            Some(ch @ ('@' | '*')) => {
                value = Value::Many { items: self.positional.clone(), star: ch == '*' };
                next = i + 2;
            }
            Some(ch) if matches!(ch, '?' | '$' | '!' | '#' | '-') => {
                value = Value::One(self.param(&ch.to_string()).unwrap_or_default());
                next = i + 2;
            }
            _ => {
                f.push('$', in_dq);
                return i + 1;
            }
        }
        self.add_value(f, value, in_dq, split);
        next
    }

    fn add_value(&mut self, f: &mut Fields, v: Value, in_dq: bool, split: bool) {
        let ifs = self.ifs();
        match v {
            Value::One(s) => {
                if in_dq {
                    f.push_str(&s, true);
                } else if split {
                    f.split_in(&s, &ifs);
                } else {
                    f.push_str(&s, false);
                }
            }
            Value::Many { items, star } => {
                if in_dq && star {
                    let sep: String = ifs.chars().next().map(String::from).unwrap_or_default();
                    f.push_str(&items.join(&sep), true);
                } else if in_dq && split {
                    // "$@": one field per parameter
                    if items.is_empty() {
                        f.suppress_open = true;
                    }
                    for (k, item) in items.iter().enumerate() {
                        if k > 0 {
                            f.end_field(true);
                        }
                        f.push_str(item, true);
                    }
                } else if in_dq || !split {
                    f.push_str(&items.join(" "), in_dq);
                } else {
                    for item in &items {
                        f.split_in(item, &ifs);
                        f.end_field(false);
                    }
                }
            }
        }
    }

    fn arith_value(&mut self, expr: &str) -> String {
        let expanded = self.expand_one(expr);
        let result = arith::eval(&expanded, &mut |n| self.param(n).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0));
        match result {
            Ok(v) => v.to_string(),
            Err(e) => {
                errln(&format!("arithmetic: {e}"));
                self.cmdsub_status = Some(1);
                "0".to_string()
            }
        }
    }

    /// `${...}` with the braces removed.
    fn param_expand(&mut self, inner: &[char]) -> Value {
        let s: String = inner.iter().collect();
        if s == "#" {
            return Value::One(self.positional.len().to_string());
        }
        if let Some(rest) = s.strip_prefix('#') {
            let (name, tail) = split_name(rest);
            if !name.is_empty() && tail.is_empty() {
                return Value::One(self.param(name).unwrap_or_default().chars().count().to_string());
            }
        }
        let (name, rest) = split_name(&s);
        if name.is_empty() {
            errln(&format!("${{{s}}}: bad substitution"));
            self.cmdsub_status = Some(1);
            return Value::One(String::new());
        }
        let current = if name == "@" || name == "*" { Some(String::new()) } else { self.param(name) };
        if rest.is_empty() {
            return match name {
                "@" | "*" => Value::Many { items: self.positional.clone(), star: name == "*" },
                _ => Value::One(current.unwrap_or_default()),
            };
        }
        let (colon, op_rest) = match rest.strip_prefix(':') {
            Some(r) => (true, r),
            None => (false, rest),
        };
        let unset_or_null = match &current {
            None => true,
            Some(v) => colon && v.is_empty(),
        };
        let mut ch = op_rest.chars();
        let op = ch.next();
        let word = ch.as_str().to_string();
        let val = current.clone().unwrap_or_default();
        Value::One(match op {
            Some('-') => {
                if unset_or_null {
                    self.expand_one(&word)
                } else {
                    val
                }
            }
            Some('=') => {
                if unset_or_null {
                    let w = self.expand_one(&word);
                    self.set(name, w.clone());
                    w
                } else {
                    val
                }
            }
            Some('+') => {
                if unset_or_null {
                    String::new()
                } else {
                    self.expand_one(&word)
                }
            }
            Some('?') if unset_or_null => {
                let msg = if word.is_empty() { "parameter null or not set".to_string() } else { self.expand_one(&word) };
                errln(&format!("{name}: {msg}"));
                self.cmdsub_status = Some(1);
                if !self.interactive {
                    self.flow = Some(Flow::Exit(1));
                }
                String::new()
            }
            Some('?') => val,
            Some(op @ ('#' | '%')) if !colon => {
                let longest = word.starts_with(op);
                let pat_src = if longest { &word[1..] } else { word.as_str() };
                let pat = self.expand_pattern(pat_src);
                if op == '#' {
                    glob::strip_prefix(&val, &pat, longest)
                } else {
                    glob::strip_suffix(&val, &pat, longest)
                }
            }
            _ => {
                errln(&format!("${{{s}}}: bad substitution"));
                self.cmdsub_status = Some(1);
                String::new()
            }
        })
    }

    /// Expand without splitting or globbing (assignments, redirection targets, `case` words).
    pub fn expand_one(&mut self, raw: &str) -> String {
        let fields = self.expand_chars(raw, false);
        fields.iter().map(|f| f.iter().map(|&(c, _)| c).collect::<String>()).collect::<Vec<_>>().join(" ")
    }

    /// Expand into a pattern: quoted characters stay literal.
    pub fn expand_pattern(&mut self, raw: &str) -> Pat {
        self.expand_chars(raw, false).into_iter().next().unwrap_or_default()
    }

    /// Full expansion of a command word: fields after splitting and pathname expansion.
    pub fn expand_fields(&mut self, raw: &str) -> Vec<String> {
        let mut out = Vec::new();
        for field in self.expand_chars(raw, true) {
            if glob::has_meta(&field) {
                let matches = glob::expand_path(&field);
                if !matches.is_empty() {
                    out.extend(matches);
                    continue;
                }
            }
            out.push(field.iter().map(|&(c, _)| c).collect());
        }
        out
    }

    /// Here-document bodies and prompts: like double quotes, without the quote characters.
    pub fn expand_dq_text(&mut self, text: &str) -> String {
        let c: Vec<char> = text.chars().collect();
        let mut f = Fields::default();
        self.expand_dq(&c, 0, &mut f, false);
        f.finish().iter().map(|fl| fl.iter().map(|&(ch, _)| ch).collect::<String>()).collect::<Vec<_>>().join(" ")
    }
}
