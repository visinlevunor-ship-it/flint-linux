//! POSIX regular expressions: regcomp, regexec, regerror, regfree, with glibc's syntax for
//! basic and extended expressions (GNU extensions: \| \+ \? in BRE, back-references,
//! \w \W \s \S \b \B \< \> \` \').
//!
//! Patterns compile to a small instruction set. Matching finds the leftmost-longest match,
//! as POSIX requires, with a Pike VM (linear time); patterns with back-references, which a
//! Pike VM cannot express, use an exhaustive backtracker with a step budget instead.

use super::pattern::{decode, fold, in_class};
use crate::fl;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ptr;

type c_char = u8;
type c_int = i32;
type size_t = usize;

const REG_EXTENDED: c_int = 1;
const REG_ICASE: c_int = 2;
const REG_NEWLINE: c_int = 4;
const REG_NOSUB: c_int = 8;
const REG_NOTBOL: c_int = 1;
const REG_NOTEOL: c_int = 2;
const REG_STARTEND: c_int = 4;

const REG_NOMATCH: c_int = 1;
const REG_BADPAT: c_int = 2;
const REG_ECOLLATE: c_int = 3;
const REG_ECTYPE: c_int = 4;
const REG_EESCAPE: c_int = 5;
const REG_ESUBREG: c_int = 6;
const REG_EBRACK: c_int = 7;
const REG_EPAREN: c_int = 8;
const REG_EBRACE: c_int = 9;
const REG_BADBR: c_int = 10;
const REG_ERANGE: c_int = 11;
const REG_ESPACE: c_int = 12;
const REG_BADRPT: c_int = 13;
const REG_ESIZE: c_int = 15;

const DUP_MAX: u32 = 0x7fff;
const MAX_INSTS: usize = 1 << 20;

// --- syntax tree ---------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Assert {
    Bol,
    Eol,
    WordB,
    NotWordB,
    WordStart,
    WordEnd,
    BufStart,
    BufEnd,
}

struct Set {
    negate: bool,
    ranges: Vec<(u32, u32)>,
    classes: Vec<Vec<u8>>,
}

enum Node {
    Empty,
    Char(u32),
    Any,
    Set(usize),
    Assert(Assert),
    Group(Box<Node>, usize),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, u32, Option<u32>),
    Backref(usize),
}

struct Parser<'a> {
    p: &'a [u8],
    i: usize,
    ere: bool,
    icase: bool,
    sets: Vec<Set>,
    ngroups: usize,
    open: Vec<usize>,
    closed: Vec<bool>,
    has_backref: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.p.get(self.i).copied()
    }
    fn at(&self, s: &[u8]) -> bool {
        self.p[self.i..].starts_with(s)
    }

    /// Does the alternation end here (end, ')' / '\)', '|' / '\|')?
    fn at_alt_end(&self) -> bool {
        if self.i >= self.p.len() {
            return true;
        }
        if self.ere {
            (self.p[self.i] == b')' && !self.open.is_empty()) || self.p[self.i] == b'|'
        } else {
            self.at(b"\\)") || self.at(b"\\|")
        }
    }

    fn parse_alt(&mut self) -> Result<Node, c_int> {
        let mut alts = vec![self.parse_concat()?];
        loop {
            if self.ere && self.peek() == Some(b'|') {
                self.i += 1;
            } else if !self.ere && self.at(b"\\|") {
                self.i += 2;
            } else {
                break;
            }
            alts.push(self.parse_concat()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn parse_concat(&mut self) -> Result<Node, c_int> {
        let mut items: Vec<Node> = Vec::new();
        let start = self.i;
        while !self.at_alt_end() {
            let at_start = self.i == start;
            let atom = self.parse_atom(at_start, items.last().map_or(false, |n| matches!(n, Node::Assert(Assert::Bol))) && items.len() == 1)?;
            let atom = self.parse_postfix(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Concat(items),
        })
    }

    fn parse_postfix(&mut self, mut atom: Node) -> Result<Node, c_int> {
        let mut repeated = false;
        loop {
            let (min, max) = match self.peek() {
                // in a BRE, '*' right after a leading '^' is an ordinary character
                Some(b'*') if !self.ere && matches!(atom, Node::Assert(Assert::Bol)) => return Ok(atom),
                Some(b'*') => {
                    self.i += 1;
                    (0, None)
                }
                Some(b'+') if self.ere => {
                    self.i += 1;
                    (1, None)
                }
                Some(b'?') if self.ere => {
                    self.i += 1;
                    (0, Some(1))
                }
                Some(b'{') if self.ere => {
                    self.i += 1;
                    self.interval()?
                }
                Some(b'\\') if !self.ere => match self.p.get(self.i + 1) {
                    Some(b'+') => {
                        self.i += 2;
                        (1, None)
                    }
                    Some(b'?') => {
                        self.i += 2;
                        (0, Some(1))
                    }
                    Some(b'{') => {
                        self.i += 2;
                        self.interval()?
                    }
                    _ => return Ok(atom),
                },
                _ => return Ok(atom),
            };
            if self.ere && matches!(atom, Node::Assert(Assert::Bol | Assert::Eol)) {
                return Err(REG_BADRPT);
            }
            // "a**" is an error in a BRE (an ERE allows stacked repetitions)
            if repeated && !self.ere && min == 0 && max.is_none() {
                return Err(REG_BADRPT);
            }
            repeated = true;
            atom = Node::Repeat(Box::new(atom), min, max);
        }
    }

    /// After '{' (or '\{'): "m}", "m,}", "m,n}" (closing with '\}' in a BRE).
    fn interval(&mut self) -> Result<(u32, Option<u32>), c_int> {
        let num = |s: &mut Self| -> Option<u32> {
            let st = s.i;
            let mut v: u32 = 0;
            while let Some(c) = s.peek() {
                if !c.is_ascii_digit() {
                    break;
                }
                v = v.saturating_mul(10).saturating_add((c - b'0') as u32);
                s.i += 1;
            }
            if s.i == st { None } else { Some(v) }
        };
        let min = num(self);
        let comma = self.peek() == Some(b',');
        let max = if comma {
            self.i += 1;
            num(self)
        } else {
            Some(min.unwrap_or(0))
        };
        let min = match min {
            Some(m) => m,
            // "{,n}" is "{0,n}"
            None if comma => 0,
            None => {
                if self.i >= self.p.len() {
                    return Err(REG_EBRACE);
                }
                return Err(REG_BADBR);
            }
        };
        let close: &[u8] = if self.ere { b"}" } else { b"\\}" };
        if !self.at(close) {
            return Err(if self.i >= self.p.len() { REG_EBRACE } else { REG_BADBR });
        }
        self.i += close.len();
        if let Some(mx) = max {
            if mx < min {
                return Err(REG_BADBR);
            }
        }
        if min > DUP_MAX || max.map_or(false, |m| m > DUP_MAX) {
            return Err(REG_ESIZE);
        }
        Ok((min, max))
    }

    fn parse_atom(&mut self, at_start: bool, after_bol: bool) -> Result<Node, c_int> {
        let c = self.p[self.i];
        if self.ere {
            match c {
                b'(' => {
                    self.i += 1;
                    return self.group();
                }
                b'*' | b'+' | b'?' => {
                    return Err(REG_BADRPT);
                }
                b'{' => {
                    // a '{' that cannot start an interval is literal
                    if at_start {
                        return Err(REG_BADRPT);
                    }
                    self.i += 1;
                    return Ok(Node::Char(b'{' as u32));
                }
                b'^' => {
                    self.i += 1;
                    return Ok(Node::Assert(Assert::Bol));
                }
                b'$' => {
                    self.i += 1;
                    return Ok(Node::Assert(Assert::Eol));
                }
                _ => {}
            }
        } else {
            match c {
                b'*' if at_start || after_bol => {
                    self.i += 1;
                    return Ok(Node::Char(b'*' as u32));
                }
                b'^' if at_start => {
                    self.i += 1;
                    return Ok(Node::Assert(Assert::Bol));
                }
                b'$' => {
                    // an anchor only at the end of the expression or before \) or \|
                    self.i += 1;
                    if self.at_alt_end() {
                        return Ok(Node::Assert(Assert::Eol));
                    }
                    return Ok(Node::Char(b'$' as u32));
                }
                b'\\' if self.p.get(self.i + 1) == Some(&b'(') => {
                    self.i += 2;
                    return self.group();
                }
                b'\\' if self.p.get(self.i + 1) == Some(&b'{') && at_start => {
                    return Err(REG_BADRPT);
                }
                _ => {}
            }
        }
        match c {
            b'.' => {
                self.i += 1;
                Ok(Node::Any)
            }
            b'[' => {
                self.i += 1;
                self.bracket()
            }
            b'\\' => {
                self.i += 1;
                let Some(e) = self.peek() else { return Err(REG_EESCAPE) };
                self.i += 1;
                Ok(match e {
                    b'1'..=b'9' => {
                        let n = (e - b'0') as usize;
                        if n > self.ngroups || !self.closed.get(n).copied().unwrap_or(false) {
                            return Err(REG_ESUBREG);
                        }
                        self.has_backref = true;
                        Node::Backref(n)
                    }
                    b'w' | b'W' | b's' | b'S' => {
                        let class: &[u8] = if e == b'w' || e == b'W' { b"alnum" } else { b"space" };
                        let mut set = Set { negate: e.is_ascii_uppercase(), ranges: Vec::new(), classes: vec![class.to_vec()] };
                        if e == b'w' || e == b'W' {
                            set.ranges.push((b'_' as u32, b'_' as u32));
                        }
                        self.sets.push(set);
                        Node::Set(self.sets.len() - 1)
                    }
                    b'b' => Node::Assert(Assert::WordB),
                    b'B' => Node::Assert(Assert::NotWordB),
                    b'<' => Node::Assert(Assert::WordStart),
                    b'>' => Node::Assert(Assert::WordEnd),
                    b'`' => Node::Assert(Assert::BufStart),
                    b'\'' => Node::Assert(Assert::BufEnd),
                    b')' if !self.ere => return Err(REG_EPAREN),
                    b'}' if !self.ere => return Err(REG_EBRACE),
                    _ => {
                        self.i -= 1;
                        let (ch, l) = decode(self.p, self.i);
                        self.i += l;
                        Node::Char(self.lit(ch))
                    }
                })
            }
            _ => {
                let (ch, l) = decode(self.p, self.i);
                self.i += l;
                Ok(Node::Char(self.lit(ch)))
            }
        }
    }

    fn lit(&self, c: u32) -> u32 {
        if self.icase { fold(c) } else { c }
    }

    fn group(&mut self) -> Result<Node, c_int> {
        self.ngroups += 1;
        let idx = self.ngroups;
        if self.closed.len() <= idx {
            self.closed.resize(idx + 1, false);
        }
        self.open.push(idx);
        let body = self.parse_alt()?;
        let close: &[u8] = if self.ere { b")" } else { b"\\)" };
        if !self.at(close) {
            return Err(REG_EPAREN);
        }
        self.i += close.len();
        self.open.pop();
        self.closed[idx] = true;
        Ok(Node::Group(Box::new(body), idx))
    }

    fn bracket(&mut self) -> Result<Node, c_int> {
        let mut set = Set { negate: false, ranges: Vec::new(), classes: Vec::new() };
        if self.peek() == Some(b'^') {
            set.negate = true;
            self.i += 1;
        }
        let mut first = true;
        loop {
            let Some(c) = self.peek() else { return Err(REG_EBRACK) };
            if c == b']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            let lo = if c == b'[' && matches!(self.p.get(self.i + 1), Some(b':' | b'=' | b'.')) {
                let kind = self.p[self.i + 1];
                let st = self.i + 2;
                let mut e = st;
                while e + 1 < self.p.len() && !(self.p[e] == kind && self.p[e + 1] == b']') {
                    e += 1;
                }
                if e + 1 >= self.p.len() {
                    return Err(REG_EBRACK);
                }
                let body = self.p[st..e].to_vec();
                self.i = e + 2;
                match kind {
                    b':' => {
                        if in_class(&body, b'a' as u32).is_none() {
                            return Err(REG_ECTYPE);
                        }
                        set.classes.push(body);
                        continue;
                    }
                    _ => {
                        if body.is_empty() {
                            return Err(REG_ECOLLATE);
                        }
                        let (ch, l) = decode(&body, 0);
                        if l != body.len() {
                            return Err(REG_ECOLLATE);
                        }
                        if kind == b'=' {
                            set.ranges.push((ch, ch));
                            continue;
                        }
                        ch
                    }
                }
            } else {
                let (ch, l) = decode(self.p, self.i);
                self.i += l;
                ch
            };
            // a range: lo-hi (a '-' before ']' is literal)
            if self.peek() == Some(b'-') && self.p.get(self.i + 1).map_or(false, |&x| x != b']') {
                self.i += 1;
                let hi = if self.at(b"[.") {
                    let st = self.i + 2;
                    let mut e = st;
                    while e + 1 < self.p.len() && !(self.p[e] == b'.' && self.p[e + 1] == b']') {
                        e += 1;
                    }
                    if e + 1 >= self.p.len() {
                        return Err(REG_EBRACK);
                    }
                    let (ch, _) = decode(&self.p[st..e], 0);
                    self.i = e + 2;
                    ch
                } else {
                    let (ch, l) = decode(self.p, self.i);
                    self.i += l;
                    ch
                };
                if hi < lo {
                    return Err(REG_ERANGE);
                }
                set.ranges.push((lo, hi));
            } else {
                set.ranges.push((lo, lo));
            }
        }
        self.sets.push(set);
        Ok(Node::Set(self.sets.len() - 1))
    }
}

// --- program --------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Inst {
    Char(u32),
    Any,
    Set(usize),
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    Assert(Assert),
    Backref(usize),
    /// Loop guards for the backtracker: remember the position / fail if it has not moved.
    Mark(usize),
    Check(usize),
    Match,
}

pub struct Prog {
    insts: Vec<Inst>,
    sets: Vec<Set>,
    ngroups: usize,
    nregs: usize,
    icase: bool,
    newline: bool,
    backrefs: bool,
}

fn nullable(n: &Node) -> bool {
    match n {
        Node::Empty | Node::Assert(_) | Node::Backref(_) => true,
        Node::Char(_) | Node::Any | Node::Set(_) => false,
        Node::Group(b, _) => nullable(b),
        Node::Concat(v) => v.iter().all(nullable),
        Node::Alt(v) => v.iter().any(nullable),
        Node::Repeat(b, min, _) => *min == 0 || nullable(b),
    }
}

struct Compiler {
    insts: Vec<Inst>,
    loops: usize,
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> Result<usize, c_int> {
        if self.insts.len() >= MAX_INSTS {
            return Err(REG_ESPACE);
        }
        self.insts.push(i);
        Ok(self.insts.len() - 1)
    }

    fn node(&mut self, n: &Node) -> Result<(), c_int> {
        match n {
            Node::Empty => {}
            Node::Char(c) => {
                self.emit(Inst::Char(*c))?;
            }
            Node::Any => {
                self.emit(Inst::Any)?;
            }
            Node::Set(s) => {
                self.emit(Inst::Set(*s))?;
            }
            Node::Assert(a) => {
                self.emit(Inst::Assert(*a))?;
            }
            Node::Backref(k) => {
                self.emit(Inst::Backref(*k))?;
            }
            Node::Group(b, k) => {
                self.emit(Inst::Save(2 * k))?;
                self.node(b)?;
                self.emit(Inst::Save(2 * k + 1))?;
            }
            Node::Concat(v) => {
                for x in v {
                    self.node(x)?;
                }
            }
            Node::Alt(v) => {
                let mut jumps = Vec::new();
                for (i, x) in v.iter().enumerate() {
                    if i + 1 < v.len() {
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.node(x)?;
                        jumps.push(self.emit(Inst::Jmp(0))?);
                        let next = self.insts.len();
                        self.insts[split] = Inst::Split(split + 1, next);
                    } else {
                        self.node(x)?;
                    }
                }
                let end = self.insts.len();
                for j in jumps {
                    self.insts[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat(b, min, max) => {
                for _ in 0..*min {
                    self.node(b)?;
                }
                match max {
                    None => {
                        // L: split body, out; [mark] body [check]; jmp L
                        let guard = nullable(b);
                        let reg = self.loops;
                        if guard {
                            self.loops += 1;
                        }
                        let l = self.emit(Inst::Split(0, 0))?;
                        if guard {
                            self.emit(Inst::Mark(reg))?;
                        }
                        self.node(b)?;
                        if guard {
                            self.emit(Inst::Check(reg))?;
                        }
                        self.emit(Inst::Jmp(l))?;
                        let out = self.insts.len();
                        self.insts[l] = Inst::Split(l + 1, out);
                    }
                    Some(mx) => {
                        // nested optionals: (b(b(b)?)?)?
                        let mut splits = Vec::new();
                        for _ in *min..*mx {
                            splits.push(self.emit(Inst::Split(0, 0))?);
                            self.node(b)?;
                        }
                        let out = self.insts.len();
                        for s in splits {
                            self.insts[s] = Inst::Split(s + 1, out);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

unsafe fn compile(p: &[u8], cflags: c_int) -> Result<Prog, c_int> {
    let ere = cflags & REG_EXTENDED != 0;
    let mut ps = Parser {
        p,
        i: 0,
        ere,
        icase: cflags & REG_ICASE != 0,
        sets: Vec::new(),
        ngroups: 0,
        open: Vec::new(),
        closed: vec![false],
        has_backref: false,
    };
    let tree = ps.parse_alt()?;
    if ps.i < p.len() {
        // a stray ')' / '\)'
        return Err(if ere { REG_EPAREN } else { REG_EPAREN });
    }
    let mut c = Compiler { insts: Vec::new(), loops: 0 };
    c.emit(Inst::Save(0))?;
    c.node(&tree)?;
    c.emit(Inst::Save(1))?;
    c.emit(Inst::Match)?;
    let ngroups = ps.ngroups;
    Ok(Prog {
        insts: c.insts,
        sets: ps.sets,
        ngroups,
        nregs: 2 * (ngroups + 1) + c.loops,
        icase: cflags & REG_ICASE != 0,
        newline: cflags & REG_NEWLINE != 0,
        backrefs: ps.has_backref,
    })
}

// --- matching -------------------------------------------------------------------------------------

struct Input<'a> {
    s: &'a [u8],
    /// the region being searched within s
    start: usize,
    end: usize,
    notbol: bool,
    noteol: bool,
}

fn is_word(c: u32) -> bool {
    c == b'_' as u32 || in_class(b"alnum", c) == Some(true)
}

impl Prog {
    fn set_match(&self, k: usize, c: u32) -> bool {
        let set = &self.sets[k];
        if set.negate && self.newline && c == b'\n' as u32 {
            return false;
        }
        let test = |x: u32| set.ranges.iter().any(|&(lo, hi)| lo <= x && x <= hi) || set.classes.iter().any(|cl| in_class(cl, x) == Some(true));
        let mut m = test(c);
        if !m && self.icase {
            let l = fold(c);
            let u = super::locale::towupper(c as _) as u32;
            m = test(l) || test(u) || set.classes.iter().any(|cl| (cl == b"upper" || cl == b"lower") && in_class(b"alpha", c) == Some(true));
        }
        m != set.negate
    }

    fn char_at(&self, inp: &Input, pos: usize) -> Option<(u32, usize)> {
        if pos >= inp.end {
            return None;
        }
        Some(decode(&inp.s[..inp.end], pos))
    }

    fn prev_char(&self, inp: &Input, pos: usize) -> Option<u32> {
        if pos <= inp.start {
            return None;
        }
        // step back over UTF-8 continuation bytes
        let mut p = pos - 1;
        while p > inp.start && inp.s[p] & 0xc0 == 0x80 && pos - p < 4 {
            p -= 1;
        }
        let (c, l) = decode(inp.s, p);
        if p + l == pos {
            Some(c)
        } else {
            Some(inp.s[pos - 1] as u32)
        }
    }

    fn assert(&self, a: Assert, inp: &Input, pos: usize) -> bool {
        let prev = self.prev_char(inp, pos);
        let next = self.char_at(inp, pos).map(|x| x.0);
        match a {
            Assert::Bol => (pos == inp.start && !inp.notbol) || (self.newline && prev == Some(b'\n' as u32)),
            Assert::Eol => (pos == inp.end && !inp.noteol) || (self.newline && next == Some(b'\n' as u32)),
            Assert::BufStart => pos == inp.start,
            Assert::BufEnd => pos == inp.end,
            _ => {
                let pw = prev.map_or(false, is_word);
                let nw = next.map_or(false, is_word);
                match a {
                    Assert::WordB => pw != nw,
                    Assert::NotWordB => pw == nw,
                    Assert::WordStart => !pw && nw,
                    _ => pw && !nw,
                }
            }
        }
    }

    /// Does instruction pc consume the character at pos? Returns the new position.
    fn step(&self, pc: usize, inp: &Input, pos: usize) -> Option<usize> {
        let (c, l) = self.char_at(inp, pos)?;
        let ok = match self.insts[pc] {
            Inst::Char(x) => {
                if self.icase {
                    fold(c) == x
                } else {
                    c == x
                }
            }
            Inst::Any => !(self.newline && c == b'\n' as u32),
            Inst::Set(k) => self.set_match(k, c),
            _ => false,
        };
        if ok { Some(pos + l) } else { None }
    }

    // --- Pike VM ---

    fn add_thread(&self, list: &mut Vec<(usize, Vec<isize>)>, seen: &mut [u32], gen: u32, pc: usize, caps: Vec<isize>, inp: &Input, pos: usize) {
        // iterative epsilon closure, keeping priority order
        let mut stack: Vec<(usize, Vec<isize>)> = vec![(pc, caps)];
        while let Some((pc, mut caps)) = stack.pop() {
            if seen[pc] == gen {
                continue;
            }
            seen[pc] = gen;
            match self.insts[pc] {
                Inst::Jmp(t) => stack.push((t, caps)),
                Inst::Split(a, b) => {
                    stack.push((b, caps.clone()));
                    stack.push((a, caps));
                }
                Inst::Save(k) => {
                    caps[k] = pos as isize;
                    stack.push((pc + 1, caps));
                }
                Inst::Mark(_) | Inst::Check(_) => stack.push((pc + 1, caps)),
                Inst::Assert(a) => {
                    if self.assert(a, inp, pos) {
                        stack.push((pc + 1, caps));
                    }
                }
                _ => list.push((pc, caps)),
            }
        }
    }

    fn pike(&self, inp: &Input) -> Option<Vec<isize>> {
        let n = self.insts.len();
        let mut seen = vec![0u32; n];
        let mut gen = 1u32;
        let mut cur_gen = gen;
        let mut clist: Vec<(usize, Vec<isize>)> = Vec::new();
        let mut best: Option<Vec<isize>> = None;
        let mut pos = inp.start;
        loop {
            // until something matched, a new thread starts here (lowest priority)
            if best.is_none() {
                self.add_thread(&mut clist, &mut seen, cur_gen, 0, vec![-1; self.nregs], inp, pos);
            }
            gen += 1;
            let ngen = gen;
            let mut nlist: Vec<(usize, Vec<isize>)> = Vec::new();
            for (pc, caps) in clist.drain(..) {
                if let Some(b) = &best {
                    if caps[0] > b[0] {
                        continue;
                    }
                }
                match self.insts[pc] {
                    Inst::Match => {
                        let better = match &best {
                            None => true,
                            Some(b) => caps[0] < b[0] || (caps[0] == b[0] && caps[1] > b[1]),
                        };
                        if better {
                            best = Some(caps);
                        }
                    }
                    _ => {
                        if let Some(np) = self.step(pc, inp, pos) {
                            self.add_thread(&mut nlist, &mut seen, ngen, pc + 1, caps, inp, np);
                        }
                    }
                }
            }
            if pos >= inp.end || (nlist.is_empty() && best.is_some()) {
                break;
            }
            let (_, l) = self.char_at(inp, pos).unwrap_or((0, 1));
            pos += l;
            clist = nlist;
            gen += 1;
            cur_gen = ngen;
        }
        best
    }

    // --- backtracker (back-references) ---

    fn backtrack(&self, inp: &Input) -> Result<Option<Vec<isize>>, c_int> {
        let mut budget: u64 = 20_000_000;
        let mut start = inp.start;
        loop {
            let mut best: Option<Vec<isize>> = None;
            let mut stack: Vec<(usize, usize, Vec<isize>)> = vec![(0, start, vec![-1; self.nregs])];
            while let Some((mut pc, mut pos, mut caps)) = stack.pop() {
                loop {
                    budget -= 1;
                    if budget == 0 {
                        return Err(REG_ESPACE);
                    }
                    match self.insts[pc] {
                        Inst::Match => {
                            if best.as_ref().map_or(true, |b| caps[1] > b[1]) {
                                best = Some(caps.clone());
                            }
                            break;
                        }
                        Inst::Jmp(t) => pc = t,
                        Inst::Split(a, b) => {
                            stack.push((b, pos, caps.clone()));
                            pc = a;
                        }
                        Inst::Save(k) => {
                            caps[k] = pos as isize;
                            pc += 1;
                        }
                        Inst::Mark(r) => {
                            caps[2 * (self.ngroups + 1) + r] = pos as isize;
                            pc += 1;
                        }
                        Inst::Check(r) => {
                            if caps[2 * (self.ngroups + 1) + r] == pos as isize {
                                break;
                            }
                            pc += 1;
                        }
                        Inst::Assert(a) => {
                            if !self.assert(a, inp, pos) {
                                break;
                            }
                            pc += 1;
                        }
                        Inst::Backref(k) => {
                            let (s, e) = (caps[2 * k], caps[2 * k + 1]);
                            if s < 0 || e < 0 {
                                break;
                            }
                            let len = (e - s) as usize;
                            if pos + len > inp.end {
                                break;
                            }
                            let a = &inp.s[s as usize..e as usize];
                            let b = &inp.s[pos..pos + len];
                            let eq = if self.icase { a.iter().zip(b).all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase()) } else { a == b };
                            if !eq {
                                break;
                            }
                            pos += len;
                            pc += 1;
                        }
                        _ => match self.step(pc, inp, pos) {
                            Some(np) => {
                                pos = np;
                                pc += 1;
                            }
                            None => break,
                        },
                    }
                }
            }
            if best.is_some() {
                return Ok(best);
            }
            if start >= inp.end {
                return Ok(None);
            }
            let (_, l) = self.char_at(inp, start).unwrap_or((0, 1));
            start += l;
        }
    }
}

// --- the C interface --------------------------------------------------------------------------------

/// glibc's regex_t (struct re_pattern_buffer).
#[repr(C)]
pub struct RegexT {
    buffer: *mut Prog,
    allocated: usize,
    used: usize,
    syntax: usize,
    fastmap: *mut c_char,
    translate: *mut u8,
    re_nsub: usize,
    bits: u32,
}

#[repr(C)]
pub struct RegMatch {
    rm_so: c_int,
    rm_eo: c_int,
}

#[no_mangle]
pub unsafe extern "C" fn regcomp(re: *mut RegexT, pattern: *const c_char, cflags: c_int) -> c_int {
    ptr::write_bytes(re as *mut u8, 0, core::mem::size_of::<RegexT>());
    match compile(fl::mem::cstr(pattern), cflags) {
        Ok(p) => {
            (*re).re_nsub = p.ngroups;
            (*re).allocated = 1;
            (*re).used = p.insts.len();
            // no_sub (bit 4) and newline_anchor (bit 7), as glibc records them
            (*re).bits = if cflags & REG_NOSUB != 0 { 1 << 4 } else { 0 } | if cflags & REG_NEWLINE != 0 { 1 << 7 } else { 0 };
            (*re).buffer = Box::into_raw(Box::new(p));
            0
        }
        Err(e) => e,
    }
}

#[no_mangle]
pub unsafe extern "C" fn regexec(re: *const RegexT, s: *const c_char, nmatch: size_t, pm: *mut RegMatch, eflags: c_int) -> c_int {
    let prog = (*re).buffer;
    if prog.is_null() {
        return REG_BADPAT;
    }
    let prog = &*prog;
    let nosub = (*re).bits & (1 << 4) != 0;
    let (start, end, bytes) = if eflags & REG_STARTEND != 0 && !pm.is_null() {
        let so = (*pm).rm_so.max(0) as usize;
        let eo = (*pm).rm_eo.max(0) as usize;
        (so, eo, core::slice::from_raw_parts(s, eo))
    } else {
        let b = fl::mem::cstr(s);
        (0, b.len(), b)
    };
    let inp = Input { s: bytes, start, end, notbol: eflags & REG_NOTBOL != 0, noteol: eflags & REG_NOTEOL != 0 };
    let caps = if prog.backrefs {
        match prog.backtrack(&inp) {
            Ok(c) => c,
            Err(e) => return e,
        }
    } else {
        prog.pike(&inp)
    };
    let Some(caps) = caps else { return REG_NOMATCH };
    if !nosub && !pm.is_null() {
        for i in 0..nmatch {
            let (so, eo) = if i <= prog.ngroups { (caps[2 * i], caps[2 * i + 1]) } else { (-1, -1) };
            let (so, eo) = if so < 0 || eo < 0 { (-1, -1) } else { (so, eo) };
            *pm.add(i) = RegMatch { rm_so: so as c_int, rm_eo: eo as c_int };
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn regfree(re: *mut RegexT) {
    if !(*re).buffer.is_null() {
        drop(Box::from_raw((*re).buffer));
        (*re).buffer = ptr::null_mut();
    }
    (*re).allocated = 0;
    (*re).used = 0;
}

const MESSAGES: [&[u8]; 17] = [
    b"Success",
    b"No match",
    b"Invalid regular expression",
    b"Invalid collation character",
    b"Invalid character class name",
    b"Trailing backslash",
    b"Invalid back reference",
    b"Unmatched [, [^, [:, [., or [=",
    b"Unmatched ( or \\(",
    b"Unmatched \\{",
    b"Invalid content of \\{\\}",
    b"Invalid range end",
    b"Memory exhausted",
    b"Invalid preceding regular expression",
    b"Premature end of regular expression",
    b"Regular expression too big",
    b"Unmatched ) or \\)",
];

#[no_mangle]
pub unsafe extern "C" fn regerror(code: c_int, _re: *const RegexT, buf: *mut c_char, size: size_t) -> size_t {
    let m = MESSAGES.get(code as usize).copied().unwrap_or(b"Invalid regular expression");
    if size > 0 && !buf.is_null() {
        let n = m.len().min(size - 1);
        ptr::copy_nonoverlapping(m.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    m.len() + 1
}
