//! Regular expressions for grep, sed, awk and vi: POSIX basic (BRE) and extended (ERE)
//! syntax with the common GNU extensions (`\+ \? \|` in BRE, `\w \W \s \S \b \B \< \>`),
//! leftmost-longest matching as POSIX requires, and capture groups.
//!
//! Matching is a Pike VM (Thompson NFA simulation): time is linear in the input for a given
//! pattern, so no pattern can make a tool hang. Back-references inside a pattern (`\(a\)\1`)
//! are the one POSIX feature this cannot do, and they are reported as unsupported.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    Basic,
    Extended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error(msg.into()))
}

// ---------------------------------------------------------------------------------------
// Characters: patterns and input are bytes; valid UTF-8 sequences count as one character.

/// The character at `i` and its length in bytes. Invalid UTF-8 is one byte per character
/// (mapped into the private area U+DC80.. so it never equals a real character).
pub fn char_at(s: &[u8], i: usize) -> (char, usize) {
    let b = s[i];
    if b < 0x80 {
        return (b as char, 1);
    }
    let len = match b {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    };
    if len > 0 && i + len <= s.len() {
        if let Ok(st) = std::str::from_utf8(&s[i..i + len]) {
            if let Some(c) = st.chars().next() {
                return (c, len);
            }
        }
    }
    (char::from_u32(0xDC00 + u32::from(b)).unwrap_or('\u{FFFD}'), 1)
}

fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[derive(Debug, Clone, PartialEq)]
enum ClassItem {
    Char(char),
    Range(char, char),
    Named(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    items: Vec<ClassItem>,
    negated: bool,
}

fn named_class(name: &str, c: char) -> bool {
    match name {
        "alpha" => c.is_alphabetic(),
        "digit" => c.is_ascii_digit(),
        "alnum" => c.is_alphanumeric(),
        "upper" => c.is_uppercase(),
        "lower" => c.is_lowercase(),
        "space" => c.is_whitespace(),
        "blank" => c == ' ' || c == '\t',
        "punct" => c.is_ascii_punctuation(),
        "print" => !c.is_control(),
        "graph" => !c.is_control() && !c.is_whitespace(),
        "cntrl" => c.is_control(),
        "xdigit" => c.is_ascii_hexdigit(),
        "word" => is_word(c),
        _ => false,
    }
}

impl Class {
    fn matches(&self, c: char, icase: bool) -> bool {
        let test = |c: char| {
            self.items.iter().any(|it| match *it {
                ClassItem::Char(x) => x == c,
                ClassItem::Range(a, b) => a <= c && c <= b,
                ClassItem::Named(n) => named_class(n, c),
            })
        };
        let hit = test(c) || (icase && (test(fold(c)) || c.to_uppercase().any(test)));
        hit != self.negated
    }
}

// ---------------------------------------------------------------------------------------
// Parsing

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Class),
    Start,
    End,
    WordB,
    NotWordB,
    WordStart,
    WordEnd,
    Group(Box<Node>, usize),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, u32, Option<u32>),
}

const MAX_REPEAT: u32 = 1000;

struct Parser<'a> {
    p: &'a [u8],
    i: usize,
    syntax: Syntax,
    groups: usize,
    groups_open: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.p.get(self.i).copied()
    }

    fn at_escape(&self, c: u8) -> bool {
        self.peek() == Some(b'\\') && self.p.get(self.i + 1) == Some(&c)
    }

    fn is_alt(&self) -> bool {
        match self.syntax {
            Syntax::Extended => self.peek() == Some(b'|'),
            Syntax::Basic => self.at_escape(b'|'),
        }
    }

    fn is_close(&self) -> bool {
        match self.syntax {
            Syntax::Extended => self.peek() == Some(b')') && self.groups_open > 0,
            Syntax::Basic => self.at_escape(b')'),
        }
    }

    fn parse_alt(&mut self) -> Result<Node, Error> {
        let mut alts = vec![self.parse_concat()?];
        while self.is_alt() {
            self.i += if self.syntax == Syntax::Extended { 1 } else { 2 };
            alts.push(self.parse_concat()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn parse_concat(&mut self) -> Result<Node, Error> {
        let mut items: Vec<Node> = Vec::new();
        let start = self.i;
        while self.peek().is_some() && !self.is_alt() && !self.is_close() {
            let at_start = self.i == start;
            let atom = self.parse_atom(at_start)?;
            let atom = self.parse_repeats(atom, at_start && items.is_empty())?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Concat(items),
        })
    }

    fn parse_repeats(&mut self, mut atom: Node, _first: bool) -> Result<Node, Error> {
        loop {
            let ere = self.syntax == Syntax::Extended;
            let (min, max, len) = match self.peek() {
                Some(b'*') => (0, None, 1),
                Some(b'+') if ere => (1, None, 1),
                Some(b'?') if ere => (0, Some(1), 1),
                Some(b'\\') if !ere && self.at_escape(b'+') => (1, None, 2),
                Some(b'\\') if !ere && self.at_escape(b'?') => (0, Some(1), 2),
                Some(b'{') if ere && self.looks_like_interval(self.i + 1) => {
                    self.i += 1;
                    let (a, b) = self.parse_interval(b"}")?;
                    atom = repeat(atom, a, b)?;
                    continue;
                }
                Some(b'\\') if !ere && self.at_escape(b'{') => {
                    self.i += 2;
                    let (a, b) = self.parse_interval(b"\\}")?;
                    atom = repeat(atom, a, b)?;
                    continue;
                }
                _ => return Ok(atom),
            };
            self.i += len;
            atom = repeat(atom, min, max)?;
        }
    }

    fn looks_like_interval(&self, at: usize) -> bool {
        // ERE: `{` not followed by a valid interval is a literal brace (as GNU does).
        let rest = &self.p[at.min(self.p.len())..];
        let end = rest.iter().position(|&c| c == b'}');
        match end {
            Some(e) => {
                let body = &rest[..e];
                !body.is_empty() && body.iter().all(|&c| c.is_ascii_digit() || c == b',') && body[0] != b','
                    || (!body.is_empty() && body[0] == b',' && body[1..].iter().all(u8::is_ascii_digit))
            }
            None => false,
        }
    }

    fn number(&mut self) -> Option<u32> {
        let s = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.p[s..self.i]).ok()?.parse().ok()
    }

    fn parse_interval(&mut self, close: &[u8]) -> Result<(u32, Option<u32>), Error> {
        let min = self.number().unwrap_or(0);
        let max = if self.peek() == Some(b',') {
            self.i += 1;
            self.number()
        } else {
            Some(min)
        };
        if !self.p[self.i..].starts_with(close) {
            return err("invalid interval {m,n}");
        }
        self.i += close.len();
        if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT || m < min) {
            return err(format!("invalid interval (counts must be at most {MAX_REPEAT}, and min <= max)"));
        }
        Ok((min, max))
    }

    fn parse_atom(&mut self, at_start: bool) -> Result<Node, Error> {
        let ere = self.syntax == Syntax::Extended;
        let c = self.peek().unwrap();
        match c {
            b'^' if ere || at_start => {
                self.i += 1;
                Ok(Node::Start)
            }
            b'$' => {
                // BRE: `$` is an anchor only at the end of the pattern or of a group/branch.
                let end_here = ere || {
                    let n = self.i + 1;
                    n == self.p.len() || self.p[n..].starts_with(b"\\)") || self.p[n..].starts_with(b"\\|")
                };
                self.i += 1;
                Ok(if end_here { Node::End } else { Node::Char('$') })
            }
            b'.' => {
                self.i += 1;
                Ok(Node::Any)
            }
            b'[' => {
                self.i += 1;
                self.parse_class().map(Node::Class)
            }
            b'(' if ere => {
                self.i += 1;
                self.group()
            }
            b'*' if at_start => {
                self.i += 1;
                Ok(Node::Char('*'))
            }
            b'*' | b'+' | b'?' if ere => err(format!("nothing to repeat before '{}'", c as char)),
            b')' if ere => err("unmatched )"),
            b'{' if ere && self.looks_like_interval(self.i + 1) => err("nothing to repeat before '{'"),
            b'\\' => self.parse_escape(),
            _ => {
                let (ch, len) = char_at(self.p, self.i);
                self.i += len;
                Ok(Node::Char(ch))
            }
        }
    }

    fn group(&mut self) -> Result<Node, Error> {
        self.groups += 1;
        let n = self.groups;
        self.groups_open += 1;
        let inner = self.parse_alt()?;
        self.groups_open -= 1;
        let ok = match self.syntax {
            Syntax::Extended => self.peek() == Some(b')'),
            Syntax::Basic => self.at_escape(b')'),
        };
        if !ok {
            return err("unmatched ( or \\(");
        }
        self.i += if self.syntax == Syntax::Extended { 1 } else { 2 };
        Ok(Node::Group(Box::new(inner), n))
    }

    fn parse_escape(&mut self) -> Result<Node, Error> {
        let ere = self.syntax == Syntax::Extended;
        self.i += 1;
        let Some(c) = self.peek() else { return err("trailing backslash") };
        self.i += 1;
        let class = |name: &'static str, negated: bool| Node::Class(Class { items: vec![ClassItem::Named(name)], negated });
        Ok(match c {
            b'(' if !ere => return self.group(),
            b'{' if !ere => return err("nothing to repeat before \\{"),
            b'1'..=b'9' => return err("back-references in patterns are not supported"),
            b'w' => class("word", false),
            b'W' => class("word", true),
            b's' => class("space", false),
            b'S' => class("space", true),
            b'd' if ere => class("digit", false),
            b'b' => Node::WordB,
            b'B' => Node::NotWordB,
            b'<' => Node::WordStart,
            b'>' => Node::WordEnd,
            b'`' => Node::Start,
            b'\'' => Node::End,
            b'n' => Node::Char('\n'),
            b't' => Node::Char('\t'),
            _ => {
                self.i -= 1;
                let (ch, len) = char_at(self.p, self.i);
                self.i += len;
                Node::Char(ch)
            }
        })
    }

    fn parse_class(&mut self) -> Result<Class, Error> {
        let mut c = Class { items: Vec::new(), negated: false };
        if self.peek() == Some(b'^') {
            c.negated = true;
            self.i += 1;
        }
        let mut first = true;
        loop {
            let Some(b) = self.peek() else { return err("unmatched [") };
            if b == b']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            if self.p[self.i..].starts_with(b"[:") {
                let rest = &self.p[self.i + 2..];
                let Some(end) = rest.windows(2).position(|w| w == b":]") else { return err("unmatched [:") };
                let name = std::str::from_utf8(&rest[..end]).unwrap_or("");
                let known = ["alpha", "digit", "alnum", "upper", "lower", "space", "blank", "punct", "print", "graph", "cntrl", "xdigit"];
                let Some(k) = known.iter().find(|k| **k == name) else { return err(format!("unknown class [:{name}:]")) };
                c.items.push(ClassItem::Named(k));
                self.i += 2 + end + 2;
                continue;
            }
            if self.p[self.i..].starts_with(b"[=") || self.p[self.i..].starts_with(b"[.") {
                // Equivalence classes and collating symbols: one character, taken literally.
                let close: &[u8] = if self.p[self.i + 1] == b'=' { b"=]" } else { b".]" };
                let rest = &self.p[self.i + 2..];
                let Some(end) = rest.windows(2).position(|w| w == close) else { return err("unmatched [= or [.") };
                if let Some(ch) = std::str::from_utf8(&rest[..end]).ok().and_then(|s| s.chars().next()) {
                    c.items.push(ClassItem::Char(ch));
                }
                self.i += 2 + end + 2;
                continue;
            }
            let (lo, len) = char_at(self.p, self.i);
            self.i += len;
            if self.peek() == Some(b'-') && self.p.get(self.i + 1).is_some_and(|&n| n != b']') {
                self.i += 1;
                let (hi, len) = char_at(self.p, self.i);
                self.i += len;
                if hi < lo {
                    return err(format!("invalid range {lo}-{hi}"));
                }
                c.items.push(ClassItem::Range(lo, hi));
            } else {
                c.items.push(ClassItem::Char(lo));
            }
        }
        Ok(c)
    }
}

fn repeat(atom: Node, min: u32, max: Option<u32>) -> Result<Node, Error> {
    if matches!(atom, Node::Start | Node::End) {
        return Ok(atom); // `^*` and the like: GNU treats the repeat as a no-op
    }
    Ok(Node::Repeat(Box::new(atom), min, max))
}

// The open-group count lives outside the struct definition above for readability.
impl Parser<'_> {
    #[allow(clippy::new_ret_no_self)]
    fn new(p: &[u8], syntax: Syntax) -> Parser<'_> {
        Parser { p, i: 0, syntax, groups: 0, groups_open: 0 }
    }
}

// ---------------------------------------------------------------------------------------
// Compiling to a program

#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    Any,
    Class(Box<Class>),
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    Assert(Node),
    Match,
}

struct Compiler {
    prog: Vec<Inst>,
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> usize {
        self.prog.push(i);
        self.prog.len() - 1
    }

    fn compile(&mut self, n: &Node) -> Result<(), Error> {
        if self.prog.len() > 100_000 {
            return err("pattern too large");
        }
        match n {
            Node::Empty => {}
            Node::Char(c) => {
                self.emit(Inst::Char(*c));
            }
            Node::Any => {
                self.emit(Inst::Any);
            }
            Node::Class(c) => {
                self.emit(Inst::Class(Box::new(c.clone())));
            }
            Node::Start | Node::End | Node::WordB | Node::NotWordB | Node::WordStart | Node::WordEnd => {
                self.emit(Inst::Assert(n.clone()));
            }
            Node::Group(inner, g) => {
                self.emit(Inst::Save(2 * g));
                self.compile(inner)?;
                self.emit(Inst::Save(2 * g + 1));
            }
            Node::Concat(v) => {
                for x in v {
                    self.compile(x)?;
                }
            }
            Node::Alt(v) => {
                let mut jumps = Vec::new();
                for (k, x) in v.iter().enumerate() {
                    if k + 1 < v.len() {
                        let split = self.emit(Inst::Split(0, 0));
                        self.compile(x)?;
                        jumps.push(self.emit(Inst::Jmp(0)));
                        let next = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, next);
                    } else {
                        self.compile(x)?;
                    }
                }
                let end = self.prog.len();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat(x, min, max) => {
                for _ in 0..*min {
                    self.compile(x)?;
                }
                match max {
                    None => {
                        // x*: L: split(body, out); body; jmp L
                        let split = self.emit(Inst::Split(0, 0));
                        self.compile(x)?;
                        self.emit(Inst::Jmp(split));
                        let out = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, out);
                    }
                    Some(m) => {
                        let mut splits = Vec::new();
                        for _ in *min..*m {
                            splits.push(self.emit(Inst::Split(0, 0)));
                            self.compile(x)?;
                        }
                        let out = self.prog.len();
                        for s in splits {
                            self.prog[s] = Inst::Split(s + 1, out);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Regex {
    prog: Vec<Inst>,
    pub groups: usize,
    icase: bool,
    /// A literal the match must start with, for a fast scan (empty if none).
    prefix: Vec<u8>,
    anchored: bool,
}

/// Captures: `caps[0]` is the whole match, `caps[n]` group n, as byte ranges.
pub type Captures = Vec<Option<(usize, usize)>>;

fn literal_prefix(n: &Node) -> (String, bool) {
    // (prefix, anchored at start)
    match n {
        Node::Char(c) => (c.to_string(), false),
        Node::Start => (String::new(), true),
        Node::Concat(v) => {
            let mut s = String::new();
            let mut anchored = false;
            for (k, x) in v.iter().enumerate() {
                match x {
                    Node::Start if k == 0 => anchored = true,
                    Node::Char(c) => s.push(*c),
                    _ => break,
                }
            }
            (s, anchored)
        }
        _ => (String::new(), false),
    }
}

impl Regex {
    pub fn new(pattern: &str, syntax: Syntax, icase: bool) -> Result<Regex, Error> {
        Self::from_bytes(pattern.as_bytes(), syntax, icase)
    }

    pub fn from_bytes(pattern: &[u8], syntax: Syntax, icase: bool) -> Result<Regex, Error> {
        let mut p = Parser::new(pattern, syntax);
        let ast = p.parse_alt()?;
        if p.i < pattern.len() {
            return err("unmatched ) or \\)");
        }
        let (prefix, anchored) = literal_prefix(&ast);
        let mut c = Compiler { prog: Vec::new() };
        c.emit(Inst::Save(0));
        c.compile(&ast)?;
        c.emit(Inst::Save(1));
        c.emit(Inst::Match);
        Ok(Regex { prog: c.prog, groups: p.groups, icase, prefix: if icase { Vec::new() } else { prefix.into_bytes() }, anchored })
    }

    /// A literal string (grep -F), as a regex.
    pub fn literal(s: &str, icase: bool) -> Regex {
        let mut c = Compiler { prog: Vec::new() };
        c.emit(Inst::Save(0));
        for ch in s.chars() {
            c.emit(Inst::Char(ch));
        }
        c.emit(Inst::Save(1));
        c.emit(Inst::Match);
        Regex { prog: c.prog, groups: 0, icase, prefix: if icase { Vec::new() } else { s.as_bytes().to_vec() }, anchored: false }
    }

    pub fn is_match(&self, text: &[u8]) -> bool {
        self.find_at(text, 0, true).is_some()
    }

    /// The leftmost-longest match at or after `start`.
    pub fn find(&self, text: &[u8], start: usize) -> Option<(usize, usize)> {
        self.captures_at(text, start).and_then(|c| c[0])
    }

    pub fn captures_at(&self, text: &[u8], start: usize) -> Option<Captures> {
        self.find_at(text, start, false)
    }

    fn assert_ok(&self, a: &Node, text: &[u8], i: usize) -> bool {
        let before = if i == 0 { None } else { Some(prev_char(text, i)) };
        let after = if i >= text.len() { None } else { Some(char_at(text, i).0) };
        let wb = before.is_some_and(is_word);
        let wa = after.is_some_and(is_word);
        match a {
            Node::Start => i == 0,
            Node::End => i == text.len(),
            Node::WordB => wb != wa,
            Node::NotWordB => wb == wa,
            Node::WordStart => !wb && wa,
            Node::WordEnd => wb && !wa,
            _ => false,
        }
    }

    fn find_at(&self, text: &[u8], start: usize, any: bool) -> Option<Captures> {
        let slots = 2 * (self.groups + 1);
        let n = self.prog.len();
        let mut clist: Vec<(usize, Vec<Option<usize>>)> = Vec::new();
        let mut nlist: Vec<(usize, Vec<Option<usize>>)> = Vec::new();
        // on[pc] == step: pc is already on the list for this step.
        let mut on = vec![usize::MAX; n];
        let mut step = 0usize;
        let mut best: Option<Vec<Option<usize>>> = None;
        if start > text.len() {
            return None;
        }
        let mut i = start;
        // Skip ahead to the first place the literal prefix occurs.
        if !self.prefix.is_empty() && !self.anchored {
            i += memfind(&text[i..], &self.prefix)?;
        }
        loop {
            // A new thread starting here, unless a match (which starts earlier) was found.
            if best.is_none() && (!self.anchored || i == 0) {
                self.add(&mut clist, &mut on, step, 0, vec![None; slots], text, i);
            }
            if clist.is_empty() {
                if best.is_some() || i >= text.len() || self.anchored {
                    break;
                }
                // Nothing alive: jump to the next possible start.
                i += char_at(text, i).1;
                if !self.prefix.is_empty() {
                    i += memfind(&text[i..], &self.prefix)?;
                }
                step += 1;
                continue;
            }
            let (ch, len) = if i < text.len() {
                let (c, l) = char_at(text, i);
                (Some(c), l)
            } else {
                (None, 0)
            };
            step += 1;
            for (pc, caps) in std::mem::take(&mut clist) {
                let advance = match &self.prog[pc] {
                    Inst::Match => {
                        let s = caps[0].unwrap_or(i);
                        let better = match &best {
                            None => true,
                            Some(b) => {
                                let bs = b[0].unwrap_or(0);
                                s < bs || (s == bs && i > b[1].unwrap_or(0))
                            }
                        };
                        if better {
                            let mut c = caps;
                            c[1] = Some(i);
                            if any {
                                return Some(to_caps(&c));
                            }
                            best = Some(c);
                        }
                        continue;
                    }
                    Inst::Char(want) => ch.is_some_and(|c| c == *want || (self.icase && fold(c) == fold(*want))),
                    Inst::Any => ch.is_some(),
                    Inst::Class(cl) => ch.is_some_and(|c| cl.matches(c, self.icase)),
                    _ => false,
                };
                if advance {
                    self.add(&mut nlist, &mut on, step, pc + 1, caps, text, i + len);
                }
            }
            // Threads that started after the best match's start can never win.
            if let Some(b) = &best {
                let bs = b[0];
                nlist.retain(|(_, c)| c[0] <= bs);
            }
            if ch.is_none() {
                break;
            }
            i += len;
            std::mem::swap(&mut clist, &mut nlist);
        }
        best.map(|b| to_caps(&b))
    }

    /// Follow the empty transitions from `pc` at position `i`, adding (in priority order)
    /// the threads that stop at an instruction which consumes input or matches.
    #[allow(clippy::too_many_arguments)]
    fn add(&self, list: &mut Vec<(usize, Vec<Option<usize>>)>, on: &mut [usize], step: usize, pc: usize, caps: Vec<Option<usize>>, text: &[u8], i: usize) {
        let mut stack = vec![(pc, caps)];
        while let Some((pc, mut caps)) = stack.pop() {
            if on[pc] == step {
                continue;
            }
            on[pc] = step;
            match &self.prog[pc] {
                Inst::Jmp(t) => stack.push((*t, caps)),
                Inst::Split(a, b) => {
                    stack.push((*b, caps.clone()));
                    stack.push((*a, caps));
                }
                Inst::Save(s) => {
                    caps[*s] = Some(i);
                    stack.push((pc + 1, caps));
                }
                Inst::Assert(a) => {
                    if self.assert_ok(a, text, i) {
                        stack.push((pc + 1, caps));
                    }
                }
                _ => list.push((pc, caps)),
            }
        }
    }
}

fn to_caps(b: &[Option<usize>]) -> Captures {
    b.chunks(2).map(|p| match (p[0], p[1]) {
        (Some(s), Some(e)) if s <= e => Some((s, e)),
        _ => None,
    }).collect()
}

fn prev_char(s: &[u8], i: usize) -> char {
    let mut j = i - 1;
    while j > 0 && i - j < 4 && s[j] & 0xC0 == 0x80 {
        j -= 1;
    }
    let (c, len) = char_at(s, j);
    if j + len == i {
        c
    } else {
        char_at(s, i - 1).0
    }
}

pub fn memfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    let first = needle[0];
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        match hay[i..hay.len() - needle.len() + 1].iter().position(|&b| b == first) {
            Some(k) => {
                i += k;
                if &hay[i..i + needle.len()] == needle {
                    return Some(i);
                }
                i += 1;
            }
            None => return None,
        }
    }
    None
}

/// Substitute matches of `re` in `text` (sed's s command and awk's sub/gsub use their own
/// replacement syntax and call this with a callback).
pub fn replace(re: &Regex, text: &[u8], which: Which, mut repl: impl FnMut(&Captures, &[u8]) -> Vec<u8>) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(text.len());
    let mut at = 0;
    let mut count = 0;
    let mut nth = 0;
    let mut last_empty_end: Option<usize> = None;
    while at <= text.len() {
        let Some(caps) = re.captures_at(text, at) else { break };
        let (s, e) = caps[0].unwrap();
        // An empty match right after a previous match is skipped (as POSIX and GNU do).
        if s == e && last_empty_end == Some(s) {
            if s >= text.len() {
                break;
            }
            let (_, len) = char_at(text, s);
            out.extend_from_slice(&text[at..s + len]);
            at = s + len;
            continue;
        }
        nth += 1;
        let take = match which {
            Which::All => true,
            Which::Nth(n) => nth == n,
            Which::From(n) => nth >= n,
        };
        out.extend_from_slice(&text[at..s]);
        if take {
            out.extend(repl(&caps, text));
            count += 1;
        } else {
            out.extend_from_slice(&text[s..e]);
        }
        last_empty_end = Some(e);
        if s == e {
            if e >= text.len() {
                at = e;
                break;
            }
            let (_, len) = char_at(text, e);
            out.extend_from_slice(&text[e..e + len]);
            at = e + len;
        } else {
            at = e;
        }
        if matches!(which, Which::Nth(n) if nth >= n) {
            break;
        }
    }
    if at <= text.len() {
        out.extend_from_slice(&text[at.min(text.len())..]);
    }
    (out, count)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    All,
    Nth(usize),
    /// GNU sed's `Ng`: the Nth and every later match.
    From(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, s: Syntax, t: &str) -> Option<(usize, usize)> {
        Regex::new(p, s, false).unwrap().find(t.as_bytes(), 0)
    }

    use Syntax::{Basic as B, Extended as E};

    #[test]
    fn basics() {
        assert_eq!(m("abc", B, "xxabcxx"), Some((2, 5)));
        assert_eq!(m("a.c", B, "abc"), Some((0, 3)));
        assert_eq!(m("^ab", B, "xab"), None);
        assert_eq!(m("ab$", B, "xab"), Some((1, 3)));
        assert_eq!(m("a$b", B, "a$b"), Some((0, 3)), "BRE $ in the middle is literal");
        assert_eq!(m("a*", B, "aaab"), Some((0, 3)));
        assert_eq!(m("*a", B, "x*a"), Some((1, 3)), "leading * is literal in BRE");
        assert_eq!(m("a+", B, "aa+"), Some((1, 3)), "+ is literal in BRE");
        assert_eq!(m("a\\+", B, "baa"), Some((1, 3)));
        assert_eq!(m("a+", E, "baa"), Some((1, 3)));
        assert_eq!(m("colou?r", E, "color"), Some((0, 5)));
        assert_eq!(m("x{2,3}", E, "xxxx"), Some((0, 3)));
        assert_eq!(m("x\\{2\\}", B, "xxx"), Some((0, 2)));
        assert_eq!(m("a{,2}b", E, "aaab"), Some((1, 4)));
        assert_eq!(m("a{x", E, "a{x"), Some((0, 3)), "a brace that is no interval is literal");
    }

    #[test]
    fn leftmost_longest() {
        assert_eq!(m("a|ab", E, "abc"), Some((0, 2)), "longest, not first alternative");
        assert_eq!(m("(a|ab)(c|bcd)", E, "abcd"), Some((0, 4)));
        assert_eq!(m("b|abc", E, "abc"), Some((0, 3)), "leftmost wins over shorter later");
        assert_eq!(m("x*", E, "abc"), Some((0, 0)), "empty match at the start");
    }

    #[test]
    fn classes_and_escapes() {
        assert_eq!(m("[0-9]+", E, "ab123c"), Some((2, 5)));
        assert_eq!(m("[^a-z]", E, "abC"), Some((2, 3)));
        assert_eq!(m("[[:digit:]][[:alpha:]]", E, "1 2b"), Some((2, 4)));
        assert_eq!(m("[]a]", E, "x]"), Some((1, 2)));
        assert_eq!(m("[a-]", E, "-"), Some((0, 1)));
        assert_eq!(m("\\bfoo\\b", E, "afoo foo"), Some((5, 8)));
        assert_eq!(m("\\<is\\>", B, "this is"), Some((5, 7)));
        assert_eq!(m("\\w+", E, "  héllo!"), Some((2, 8)), "UTF-8 letters are word characters");
        assert_eq!(m("h.llo", E, "héllo"), Some((0, 6)), ". is one character, not one byte");
        assert_eq!(m("\\.", E, "a.b"), Some((1, 2)));
        assert!(Regex::new("HeLLo", E, true).unwrap().is_match(b"say hello"));
        assert!(Regex::new("[a-c]x", E, true).unwrap().is_match(b"BX"));
    }

    #[test]
    fn groups() {
        let re = Regex::new("\\(a*\\)\\(b*\\)", B, false).unwrap();
        let c = re.captures_at(b"xaabbby", 0).unwrap();
        assert_eq!(c[0], Some((0, 0)), "leftmost: the empty match at 0");
        let c = re.captures_at(b"aabbby", 0).unwrap();
        assert_eq!((c[0], c[1], c[2]), (Some((0, 5)), Some((0, 2)), Some((2, 5))));
        let re = Regex::new("(foo|bar)+", E, false).unwrap();
        let c = re.captures_at(b"xfoobarfoo!", 0).unwrap();
        assert_eq!(c[0], Some((1, 10)));
        assert_eq!(c[1], Some((7, 10)), "the last iteration");
        let re = Regex::new("(a)|b", E, false).unwrap();
        assert_eq!(re.captures_at(b"b", 0).unwrap()[1], None);
    }

    #[test]
    fn errors() {
        for (p, s) in [("a\\(", B), ("(a", E), ("a)", E), ("[a", E), ("+a", E), ("\\(a\\)\\1", B), ("a{5,2}", E), ("x\\", B), ("[[:bogus:]]", E)] {
            assert!(Regex::new(p, s, false).is_err(), "{p}");
        }
        assert!(Regex::new("a)", B, false).is_ok(), ") is literal in BRE");
    }

    #[test]
    fn no_blowup() {
        // (a*)*b against many a's: exponential for a backtracker, linear here.
        let re = Regex::new("(a*)*b", E, false).unwrap();
        let text = vec![b'a'; 20_000];
        let t = std::time::Instant::now();
        assert!(!re.is_match(&text));
        assert!(t.elapsed().as_secs() < 5);
    }

    #[test]
    fn substitution() {
        let re = Regex::new("o", E, false).unwrap();
        let r = |w| String::from_utf8(replace(&re, b"foo boo", w, |_, _| b"0".to_vec()).0).unwrap();
        assert_eq!(r(Which::All), "f00 b00");
        assert_eq!(r(Which::Nth(3)), "foo b0o");
        assert_eq!(r(Which::From(2)), "f00 b00".replacen('0', "o", 1));
        let empty = Regex::new("x*", E, false).unwrap();
        assert_eq!(String::from_utf8(replace(&empty, b"abc", Which::All, |_, _| b"-".to_vec()).0).unwrap(), "-a-b-c-");
        let re = Regex::new("b*", E, false).unwrap();
        assert_eq!(String::from_utf8(replace(&re, b"abc", Which::All, |_, _| b"X".to_vec()).0).unwrap(), "XaXcX");
    }
}
