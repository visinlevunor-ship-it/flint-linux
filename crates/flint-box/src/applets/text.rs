//! Text tools: grep, sed, sort, uniq, tr, cut, tee, xargs.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::Command;

use crate::regex::{self, Regex, Syntax, Which};
use crate::util::{error, finish_output, parse};

/// Read all of a file (or stdin for "-").
pub fn read_input(name: &str) -> io::Result<Vec<u8>> {
    if name == "-" {
        let mut v = Vec::new();
        io::stdin().lock().read_to_end(&mut v)?;
        Ok(v)
    } else {
        fs::read(name)
    }
}

/// Split into lines without the newline; a final line without a newline still counts.
pub fn lines(data: &[u8]) -> Vec<&[u8]> {
    let mut v: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    if data.ends_with(b"\n") || data.is_empty() {
        v.pop();
    }
    v
}

// ---------------------------------------------------------------------------------------
// grep

struct GrepOpts {
    invert: bool,
    count: bool,
    files_with: bool,
    files_without: bool,
    number: bool,
    names: bool,
    quiet: bool,
    only: bool,
    word: bool,
    line: bool,
    silent: bool,
    max: Option<usize>,
}

const GREP_USAGE: &str = "usage: grep [-EFivclLnhHrRqsowx] [-m NUM] [-e PATTERN]... [-f FILE] [PATTERN] [FILE...]";

fn grep_file(prog: &str, name: &str, data: &[u8], res: &[Regex], o: &GrepOpts, out: &mut impl Write) -> io::Result<bool> {
    let mut count = 0usize;
    for (n, line) in lines(data).iter().enumerate() {
        if o.max.is_some_and(|m| count >= m) {
            break;
        }
        let hit_at = |start: usize| -> Option<(usize, usize)> {
            let mut best: Option<(usize, usize)> = None;
            for re in res {
                let mut at = start;
                while let Some((s, e)) = re.find(line, at) {
                    let ok = (!o.word || word_bounded(line, s, e)) && (!o.line || (s == 0 && e == line.len()));
                    if ok {
                        if best.is_none_or_worse(s, e) {
                            best = Some((s, e));
                        }
                        break;
                    }
                    if s >= line.len() {
                        break;
                    }
                    at = s + regex::char_at(line, s).1;
                }
            }
            best
        };
        let matched = hit_at(0).is_some();
        if matched == o.invert {
            continue;
        }
        count += 1;
        if o.quiet {
            return Ok(true);
        }
        if o.files_with || o.files_without || o.count {
            continue;
        }
        let prefix = |out: &mut dyn Write| -> io::Result<()> {
            if o.names {
                write!(out, "{name}:")?;
            }
            if o.number {
                write!(out, "{}:", n + 1)?;
            }
            Ok(())
        };
        if o.only && !o.invert {
            let mut at = 0;
            while let Some((s, e)) = hit_at(at) {
                if e > s {
                    prefix(out)?;
                    out.write_all(&line[s..e])?;
                    out.write_all(b"\n")?;
                }
                if e >= line.len() {
                    break;
                }
                at = if e > s { e } else { e + regex::char_at(line, e).1 };
            }
        } else {
            prefix(out)?;
            out.write_all(line)?;
            out.write_all(b"\n")?;
        }
    }
    if o.count {
        if o.names {
            write!(out, "{name}:")?;
        }
        writeln!(out, "{count}")?;
    }
    if o.files_with && count > 0 {
        writeln!(out, "{name}")?;
    }
    if o.files_without && count == 0 {
        writeln!(out, "{name}")?;
    }
    let _ = prog;
    Ok(count > 0)
}

trait Best {
    fn is_none_or_worse(&self, s: usize, e: usize) -> bool;
}

impl Best for Option<(usize, usize)> {
    fn is_none_or_worse(&self, s: usize, e: usize) -> bool {
        match self {
            None => true,
            Some((bs, be)) => s < *bs || (s == *bs && e > *be),
        }
    }
}

fn word_bounded(line: &[u8], s: usize, e: usize) -> bool {
    let wc = |c: char| c.is_alphanumeric() || c == '_';
    let before = s == 0 || !wc(prev(line, s));
    let after = e >= line.len() || !wc(regex::char_at(line, e).0);
    before && after
}

fn prev(line: &[u8], i: usize) -> char {
    let mut j = i - 1;
    while j > 0 && line[j] & 0xC0 == 0x80 {
        j -= 1;
    }
    regex::char_at(line, j).0
}

fn walk_files(path: &Path, out: &mut Vec<String>) {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {
            let Ok(rd) = fs::read_dir(path) else { return };
            let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for e in entries {
                walk_files(&e, out);
            }
        }
        Ok(m) if m.is_file() => out.push(path.to_string_lossy().into_owned()),
        _ => {}
    }
}

pub fn grep(args: &[String]) -> i32 {
    let p = match parse(args, "EFGivclLnhHrRqsowxy", "efm") {
        Ok(p) => p,
        Err(e) => {
            error("grep", format!("{e}\n{GREP_USAGE}"));
            return 2;
        }
    };
    let mut patterns: Vec<String> = Vec::new();
    let mut operands = p.operands.clone();
    // -e and -f may be repeated: collect them in order from the raw arguments.
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        if let Some(rest) = a.strip_prefix("-e") {
            if a.starts_with("-e") && !a.starts_with("--") {
                patterns.push(if rest.is_empty() { it.next().cloned().unwrap_or_default() } else { rest.to_string() });
            }
        } else if let Some(rest) = a.strip_prefix("-f") {
            let file = if rest.is_empty() { it.next().cloned().unwrap_or_default() } else { rest.to_string() };
            match read_input(&file) {
                Ok(d) => patterns.extend(lines(&d).iter().map(|l| String::from_utf8_lossy(l).into_owned())),
                Err(e) => {
                    error("grep", format!("{file}: {e}"));
                    return 2;
                }
            }
        } else if a.starts_with('-') && a.len() > 1 && !a.starts_with("--") {
            // combined flags ending in e or f, like -ie PATTERN
            let flags = &a[1..];
            if let Some(pos) = flags.find(['e', 'f']) {
                if pos > 0 && !flags[..pos].contains('m') {
                    let rest = &flags[pos + 1..];
                    let val = if rest.is_empty() { it.next().cloned().unwrap_or_default() } else { rest.to_string() };
                    if flags.as_bytes()[pos] == b'e' {
                        patterns.push(val);
                    } else if let Ok(d) = read_input(&val) {
                        patterns.extend(lines(&d).iter().map(|l| String::from_utf8_lossy(l).into_owned()));
                    }
                }
            }
        }
    }
    if patterns.is_empty() {
        if operands.is_empty() {
            error("grep", GREP_USAGE);
            return 2;
        }
        let pat = operands.remove(0);
        patterns.extend(pat.split('\n').map(String::from));
    }
    let icase = p.has('i') || p.has('y');
    let syntax = if p.has('E') { Syntax::Extended } else { Syntax::Basic };
    let mut res = Vec::new();
    for pat in &patterns {
        let re = if p.has('F') { Ok(Regex::literal(pat, icase)) } else { Regex::new(pat, syntax, icase) };
        match re {
            Ok(r) => res.push(r),
            Err(e) => {
                error("grep", format!("{pat}: {e}"));
                return 2;
            }
        }
    }
    let recursive = p.has('r') || p.has('R');
    let mut files = operands;
    if files.is_empty() {
        files.push(if recursive { ".".into() } else { "-".into() });
    }
    if recursive {
        let mut all = Vec::new();
        for f in &files {
            if f == "-" {
                all.push(f.clone());
            } else {
                walk_files(Path::new(f), &mut all);
            }
        }
        files = all;
    }
    let max = match p.value('m').map(str::parse::<usize>) {
        Some(Ok(m)) => Some(m),
        Some(Err(_)) => {
            error("grep", "-m needs a number");
            return 2;
        }
        None => None,
    };
    let o = GrepOpts {
        invert: p.has('v'),
        count: p.has('c'),
        files_with: p.has('l'),
        files_without: p.has('L'),
        number: p.has('n'),
        names: !p.has('h') && (p.has('H') || files.len() > 1 || recursive),
        quiet: p.has('q'),
        only: p.has('o'),
        word: p.has('w'),
        line: p.has('x'),
        silent: p.has('s'),
        max,
    };
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut any = false;
    let mut trouble = false;
    for f in &files {
        let data = match read_input(f) {
            Ok(d) => d,
            Err(e) => {
                if !o.silent {
                    error("grep", format!("{f}: {e}"));
                }
                trouble = true;
                continue;
            }
        };
        let shown = if f == "-" { "(standard input)" } else { f };
        match grep_file("grep", shown, &data, &res, &o, &mut out) {
            Ok(true) => {
                any = true;
                if o.quiet {
                    return 0;
                }
            }
            Ok(false) => {}
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return if any { 0 } else { 1 },
            Err(e) => {
                error("grep", format!("write error: {e}"));
                return 2;
            }
        }
    }
    if finish_output("grep", out.flush()) != 0 {
        return 2;
    }
    if trouble && !(o.quiet && any) {
        2
    } else if any {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------------------
// sed

#[derive(Debug, Clone)]
enum Addr {
    Line(usize),
    Last,
    Re(Option<Box<Regex>>), // None: the last regex used
    Step(usize, usize),
}

#[derive(Debug, Clone)]
enum Cmd {
    Block(usize), // index of the matching '}'
    EndBlock,
    Subst { re: Option<Box<Regex>>, repl: Vec<u8>, which: Which, print: bool, write: Option<String> },
    Trans(Vec<(char, char)>),
    Delete,
    DeleteFirst,
    Print,
    PrintFirst,
    Next,
    NextAppend,
    Quit(i32, bool),
    Append(Vec<u8>),
    Insert(Vec<u8>),
    Change(Vec<u8>),
    LineNo,
    Hold,
    HoldAppend,
    Get,
    GetAppend,
    Exchange,
    Branch(String),
    BranchIf(String, bool),
    Label(String),
    List,
    ReadFile(String),
    WriteFile(String),
    Zap,
    Nop,
}

#[derive(Debug, Clone)]
struct Inst {
    a1: Option<Addr>,
    a2: Option<Addr>,
    /// a2 is "+N" (GNU): N lines after the start of the range
    a2_rel: Option<usize>,
    negate: bool,
    cmd: Cmd,
}

struct SedParser<'a> {
    s: &'a [u8],
    i: usize,
    syntax: Syntax,
}

impl SedParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|c| c == b' ' || c == b'\t') {
            self.i += 1;
        }
    }

    fn number(&mut self) -> Option<usize> {
        let st = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[st..self.i]).ok()?.parse().ok()
    }

    /// Read up to the unescaped delimiter; `\delim` becomes `delim`, `\n` a newline.
    fn delimited(&mut self, d: u8, is_regex: bool) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else { return Err("unterminated command".into()) };
            self.i += 1;
            if c == d {
                return Ok(out);
            }
            if c == b'\\' {
                let Some(n) = self.peek() else { return Err("unterminated command".into()) };
                self.i += 1;
                if n == d {
                    out.push(d);
                } else if n == b'n' && is_regex {
                    out.extend_from_slice(b"\\n");
                } else if n == b'\n' {
                    out.push(b'\n');
                } else {
                    out.push(b'\\');
                    out.push(n);
                }
                continue;
            }
            // Brackets in a regex can hold the delimiter unescaped.
            if is_regex && c == b'[' {
                out.push(c);
                let start = self.i;
                if self.peek() == Some(b'^') {
                    out.push(b'^');
                    self.i += 1;
                }
                if self.peek() == Some(b']') {
                    out.push(b']');
                    self.i += 1;
                }
                while let Some(x) = self.peek() {
                    self.i += 1;
                    out.push(x);
                    if x == b'[' && matches!(self.peek(), Some(b':' | b'=' | b'.')) {
                        let close = self.peek().unwrap();
                        out.push(close);
                        self.i += 1;
                        while let Some(y) = self.peek() {
                            self.i += 1;
                            out.push(y);
                            if y == close && self.peek() == Some(b']') {
                                out.push(b']');
                                self.i += 1;
                                break;
                            }
                        }
                        continue;
                    }
                    if x == b']' {
                        break;
                    }
                }
                let _ = start;
                continue;
            }
            out.push(c);
        }
    }

    fn regex(&mut self, d: u8) -> Result<Option<Box<Regex>>, String> {
        let pat = self.delimited(d, true)?;
        let icase = if self.peek() == Some(b'I') {
            self.i += 1;
            true
        } else {
            false
        };
        if pat.is_empty() {
            return Ok(None);
        }
        Regex::from_bytes(&pat, self.syntax, icase).map(|r| Some(Box::new(r))).map_err(|e| e.0)
    }

    fn address(&mut self) -> Result<Option<Addr>, String> {
        match self.peek() {
            Some(b'0'..=b'9') => {
                let n = self.number().unwrap_or(0);
                if self.peek() == Some(b'~') {
                    self.i += 1;
                    let step = self.number().unwrap_or(0);
                    return Ok(Some(Addr::Step(n, step)));
                }
                Ok(Some(Addr::Line(n)))
            }
            Some(b'$') => {
                self.i += 1;
                Ok(Some(Addr::Last))
            }
            Some(b'/') => {
                self.i += 1;
                Ok(Some(Addr::Re(self.regex(b'/')?)))
            }
            Some(b'\\') => {
                self.i += 1;
                let Some(d) = self.peek() else { return Err("missing delimiter".into()) };
                self.i += 1;
                Ok(Some(Addr::Re(self.regex(d)?)))
            }
            _ => Ok(None),
        }
    }

    fn label(&mut self) -> String {
        self.skip_ws();
        let st = self.i;
        while self.peek().is_some_and(|c| c != b'\n' && c != b';' && c != b'}') {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[st..self.i]).trim().to_string()
    }

    fn filename(&mut self) -> String {
        self.skip_ws();
        let st = self.i;
        while self.peek().is_some_and(|c| c != b'\n') {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[st..self.i]).into_owned()
    }

    /// Text for a, i, c: `a\` newline text, or GNU one-liner `a text`.
    fn text(&mut self) -> Vec<u8> {
        self.skip_ws();
        if self.peek() == Some(b'\\') {
            self.i += 1;
            if self.peek() == Some(b'\n') {
                self.i += 1;
            }
        }
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == b'\\' {
                match self.peek() {
                    Some(n) => {
                        self.i += 1;
                        out.push(if n == b'n' { b'\n' } else if n == b't' { b'\t' } else { n });
                    }
                    None => break,
                }
                continue;
            }
            if c == b'\n' {
                break;
            }
            out.push(c);
        }
        out
    }

    fn parse(&mut self, prog: &mut Vec<Inst>, blocks: &mut Vec<usize>) -> Result<(), String> {
        loop {
            while self.peek().is_some_and(|c| c == b' ' || c == b'\t' || c == b'\n' || c == b';') {
                self.i += 1;
            }
            let Some(c) = self.peek() else { return Ok(()) };
            if c == b'#' {
                while self.peek().is_some_and(|c| c != b'\n') {
                    self.i += 1;
                }
                continue;
            }
            let a1 = self.address()?;
            let mut a2 = None;
            let mut a2_rel = None;
            if a1.is_some() && self.peek() == Some(b',') {
                self.i += 1;
                self.skip_ws();
                if self.peek() == Some(b'+') {
                    self.i += 1;
                    a2_rel = Some(self.number().ok_or("expected a number after +")?);
                } else {
                    a2 = self.address()?;
                    if a2.is_none() {
                        return Err("unexpected ','".into());
                    }
                }
            }
            self.skip_ws();
            let mut negate = false;
            while self.peek() == Some(b'!') {
                negate = true;
                self.i += 1;
                self.skip_ws();
            }
            let Some(c) = self.peek() else { return Err("missing command".into()) };
            self.i += 1;
            let cmd = match c {
                b'{' => {
                    blocks.push(prog.len());
                    Cmd::Block(0)
                }
                b'}' => {
                    let open = blocks.pop().ok_or("unexpected '}'")?;
                    let here = prog.len();
                    if let Cmd::Block(ref mut t) = prog[open].cmd {
                        *t = here;
                    }
                    Cmd::EndBlock
                }
                b's' => {
                    let d = self.peek().ok_or("unterminated s command")?;
                    if d == b'\n' || d == b'\\' {
                        return Err("bad delimiter for s".into());
                    }
                    self.i += 1;
                    let re = self.regex_no_flag(d)?;
                    let repl = self.delimited(d, false)?;
                    let mut which = Which::Nth(1);
                    let mut print = false;
                    let mut icase = false;
                    let mut write = None;
                    let mut global = false;
                    let mut nth = None;
                    loop {
                        match self.peek() {
                            Some(b'g') => global = true,
                            Some(b'p') => print = true,
                            Some(b'i' | b'I') => icase = true,
                            Some(b'0'..=b'9') => {
                                nth = self.number();
                                continue;
                            }
                            Some(b'w') => {
                                self.i += 1;
                                write = Some(self.filename());
                                break;
                            }
                            _ => break,
                        }
                        self.i += 1;
                    }
                    if global {
                        which = match nth {
                            Some(n) if n > 1 => Which::From(n),
                            _ => Which::All,
                        };
                    } else if let Some(n) = nth {
                        if n == 0 {
                            return Err("number option to s cannot be zero".into());
                        }
                        which = Which::Nth(n);
                    }
                    let re = match re {
                        Some(p) => Some(Box::new(Regex::from_bytes(&p, self.syntax, icase).map_err(|e| e.0)?)),
                        None => None,
                    };
                    Cmd::Subst { re, repl, which, print, write }
                }
                b'y' => {
                    let d = self.peek().ok_or("unterminated y command")?;
                    self.i += 1;
                    let a = String::from_utf8_lossy(&self.delimited(d, false)?).replace("\\\\", "\\");
                    let b = String::from_utf8_lossy(&self.delimited(d, false)?).replace("\\\\", "\\");
                    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
                    if a.len() != b.len() {
                        return Err("strings for y command are different lengths".into());
                    }
                    Cmd::Trans(a.into_iter().zip(b).collect())
                }
                b'd' => Cmd::Delete,
                b'D' => Cmd::DeleteFirst,
                b'p' => Cmd::Print,
                b'P' => Cmd::PrintFirst,
                b'n' => Cmd::Next,
                b'N' => Cmd::NextAppend,
                b'q' | b'Q' => {
                    self.skip_ws();
                    let code = self.number().unwrap_or(0) as i32;
                    Cmd::Quit(code, c == b'q')
                }
                b'a' => Cmd::Append(self.text()),
                b'i' => Cmd::Insert(self.text()),
                b'c' => Cmd::Change(self.text()),
                b'=' => Cmd::LineNo,
                b'h' => Cmd::Hold,
                b'H' => Cmd::HoldAppend,
                b'g' => Cmd::Get,
                b'G' => Cmd::GetAppend,
                b'x' => Cmd::Exchange,
                b'b' => Cmd::Branch(self.label()),
                b't' => Cmd::BranchIf(self.label(), true),
                b'T' => Cmd::BranchIf(self.label(), false),
                b':' => {
                    if a1.is_some() {
                        return Err(": doesn't want any addresses".into());
                    }
                    let l = self.label();
                    if l.is_empty() {
                        return Err("\":\" lacks a label".into());
                    }
                    Cmd::Label(l)
                }
                b'l' => Cmd::List,
                b'r' => Cmd::ReadFile(self.filename()),
                b'w' => Cmd::WriteFile(self.filename()),
                b'z' => Cmd::Zap,
                b'#' => Cmd::Nop,
                other => return Err(format!("unknown command: '{}'", other as char)),
            };
            prog.push(Inst { a1, a2, a2_rel, negate, cmd });
        }
    }

    fn regex_no_flag(&mut self, d: u8) -> Result<Option<Vec<u8>>, String> {
        let pat = self.delimited(d, true)?;
        Ok(if pat.is_empty() { None } else { Some(pat) })
    }
}

struct Sed {
    prog: Vec<Inst>,
    quiet: bool,
    in_range: Vec<bool>,
    range_start: Vec<usize>,
    last_re: Option<Box<Regex>>,
    files: HashMap<String, fs::File>,
}

enum Flow {
    /// End the cycle; print unless -n (and unless deleted)
    EndCycle { print: bool },
    /// D with a newline: restart without reading
    Restart,
    Quit(i32, bool),
}

struct Input {
    lines: Vec<Vec<u8>>,
    pos: usize,
    /// Did the very last line end without a newline?
    missing_newline: bool,
}

impl Input {
    fn next(&mut self) -> Option<Vec<u8>> {
        let l = self.lines.get(self.pos).cloned();
        if l.is_some() {
            self.pos += 1;
        }
        l
    }

    fn is_last(&self) -> bool {
        self.pos >= self.lines.len()
    }
}

/// sed's replacement: `&` is the match, `\1`..`\9` groups, `\n` newline, `\\` and `\&` literal.
fn expand_repl(repl: &[u8], caps: &regex::Captures, text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    let group = |n: usize, out: &mut Vec<u8>| {
        if let Some(Some((s, e))) = caps.get(n) {
            out.extend_from_slice(&text[*s..*e]);
        }
    };
    while i < repl.len() {
        let c = repl[i];
        i += 1;
        match c {
            b'&' => group(0, &mut out),
            b'\\' if i < repl.len() => {
                let n = repl[i];
                i += 1;
                match n {
                    b'0'..=b'9' => group(usize::from(n - b'0'), &mut out),
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    _ => out.push(n),
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn escape_l(line: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut col = 0;
    for &b in line {
        let piece: Vec<u8> = match b {
            b'\\' => b"\\\\".to_vec(),
            0x07 => b"\\a".to_vec(),
            0x08 => b"\\b".to_vec(),
            0x0C => b"\\f".to_vec(),
            b'\n' => b"\\n".to_vec(),
            b'\r' => b"\\r".to_vec(),
            b'\t' => b"\\t".to_vec(),
            0x0B => b"\\v".to_vec(),
            32..=126 => vec![b],
            _ => format!("\\{b:03o}").into_bytes(),
        };
        if col + piece.len() > 69 {
            out.extend_from_slice(b"\\\n");
            col = 0;
        }
        col += piece.len();
        out.extend(piece);
    }
    out.extend_from_slice(b"$\n");
    out
}

impl Sed {
    fn matches(&mut self, a: &Addr, ps: &[u8], line_no: usize, last: bool) -> bool {
        match a {
            Addr::Line(n) => line_no == *n,
            Addr::Last => last,
            Addr::Step(first, step) => {
                if *step == 0 {
                    line_no == *first
                } else {
                    line_no >= *first && (line_no - first) % step == 0
                }
            }
            Addr::Re(r) => {
                let re = match r {
                    Some(r) => {
                        self.last_re = Some(r.clone());
                        r
                    }
                    None => match &self.last_re {
                        Some(r) => r,
                        None => return false,
                    },
                };
                re.is_match(ps)
            }
        }
    }

    fn selected(&mut self, k: usize, ps: &[u8], line_no: usize, last: bool) -> bool {
        let inst = self.prog[k].clone();
        let hit = match (&inst.a1, &inst.a2, inst.a2_rel) {
            (None, _, _) => true,
            (Some(a1), None, None) => self.matches(a1, ps, line_no, last),
            (Some(a1), a2, rel) => {
                if self.in_range[k] {
                    let end = match (a2, rel) {
                        (Some(Addr::Line(n)), _) => line_no >= *n,
                        (Some(a), _) => self.matches(a, ps, line_no, last),
                        (None, Some(r)) => line_no >= self.range_start[k] + r,
                        (None, None) => true,
                    };
                    if end {
                        self.in_range[k] = false;
                    }
                    true
                } else if self.matches(a1, ps, line_no, last) {
                    self.range_start[k] = line_no;
                    // A line-number end that is already behind ends the range at once.
                    let ends_now = match (a2, rel) {
                        (Some(Addr::Line(n)), _) => *n <= line_no,
                        (None, Some(0)) => true,
                        _ => false,
                    };
                    self.in_range[k] = !ends_now;
                    true
                } else {
                    false
                }
            }
        };
        hit != inst.negate
    }

    #[allow(clippy::too_many_arguments)]
    fn run_cycle(&mut self, ps: &mut Vec<u8>, hold: &mut Vec<u8>, input: &mut Input, line_no: &mut usize, out: &mut dyn Write, append_queue: &mut Vec<Vec<u8>>, subst_flag: &mut bool) -> io::Result<Flow> {
        let mut pc = 0;
        while pc < self.prog.len() {
            let last = input.is_last();
            if !self.selected(pc, ps, *line_no, last) {
                if let Cmd::Block(end) = self.prog[pc].cmd {
                    pc = end + 1;
                } else {
                    pc += 1;
                }
                continue;
            }
            let cmd = self.prog[pc].cmd.clone();
            pc += 1;
            match cmd {
                Cmd::Block(_) | Cmd::EndBlock | Cmd::Label(_) | Cmd::Nop => {}
                Cmd::Subst { re, repl, which, print, write } => {
                    let re = match re {
                        Some(r) => {
                            self.last_re = Some(r.clone());
                            r
                        }
                        None => match &self.last_re {
                            Some(r) => r.clone(),
                            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "no previous regular expression")),
                        },
                    };
                    let (new, n) = regex::replace(&re, ps, which, |caps, text| expand_repl(&repl, caps, text));
                    if n > 0 {
                        *ps = new;
                        *subst_flag = true;
                        if print {
                            out.write_all(ps)?;
                            out.write_all(b"\n")?;
                        }
                        if let Some(w) = write {
                            self.write_to(&w, ps)?;
                        }
                    }
                }
                Cmd::Trans(map) => {
                    let s = String::from_utf8_lossy(ps).into_owned();
                    *ps = s.chars().map(|c| map.iter().find(|(a, _)| *a == c).map_or(c, |(_, b)| *b)).collect::<String>().into_bytes();
                }
                Cmd::Delete => return Ok(Flow::EndCycle { print: false }),
                Cmd::DeleteFirst => match ps.iter().position(|&b| b == b'\n') {
                    Some(k) => {
                        ps.drain(..=k);
                        return Ok(Flow::Restart);
                    }
                    None => return Ok(Flow::EndCycle { print: false }),
                },
                Cmd::Print => {
                    out.write_all(ps)?;
                    out.write_all(b"\n")?;
                }
                Cmd::PrintFirst => {
                    let end = ps.iter().position(|&b| b == b'\n').unwrap_or(ps.len());
                    out.write_all(&ps[..end])?;
                    out.write_all(b"\n")?;
                }
                Cmd::Next => {
                    if !self.quiet {
                        out.write_all(ps)?;
                        out.write_all(b"\n")?;
                    }
                    flush_appends(out, append_queue)?;
                    match input.next() {
                        Some(l) => {
                            *ps = l;
                            *line_no += 1;
                        }
                        None => return Ok(Flow::Quit(0, false)),
                    }
                }
                Cmd::NextAppend => match input.next() {
                    Some(l) => {
                        flush_appends(out, append_queue)?;
                        ps.push(b'\n');
                        ps.extend(l);
                        *line_no += 1;
                    }
                    // GNU prints the pattern space when N runs out of input.
                    None => return Ok(Flow::Quit(0, true)),
                },
                Cmd::Quit(code, print) => return Ok(Flow::Quit(code, print)),
                Cmd::Append(t) => append_queue.push(t),
                Cmd::Insert(t) => {
                    out.write_all(&t)?;
                    out.write_all(b"\n")?;
                }
                Cmd::Change(t) => {
                    let k = pc - 1;
                    let in_range = self.prog[k].a2.is_some() || self.prog[k].a2_rel.is_some();
                    if !in_range || !self.in_range[k] {
                        out.write_all(&t)?;
                        out.write_all(b"\n")?;
                    }
                    return Ok(Flow::EndCycle { print: false });
                }
                Cmd::LineNo => writeln!(out, "{line_no}")?,
                Cmd::Hold => *hold = ps.clone(),
                Cmd::HoldAppend => {
                    hold.push(b'\n');
                    hold.extend_from_slice(ps);
                }
                Cmd::Get => *ps = hold.clone(),
                Cmd::GetAppend => {
                    ps.push(b'\n');
                    ps.extend_from_slice(hold);
                }
                Cmd::Exchange => std::mem::swap(ps, hold),
                Cmd::Branch(l) => pc = self.jump(&l)?,
                Cmd::BranchIf(l, on_success) => {
                    if *subst_flag == on_success {
                        *subst_flag = false;
                        pc = self.jump(&l)?;
                    } else if on_success {
                        // t without a substitution does not reset the flag
                    } else {
                        *subst_flag = false;
                    }
                }
                Cmd::List => out.write_all(&escape_l(ps))?,
                Cmd::ReadFile(f) => {
                    if let Ok(d) = fs::read(&f) {
                        let mut d = d;
                        if d.ends_with(b"\n") {
                            d.pop();
                        }
                        append_queue.push(d);
                    }
                }
                Cmd::WriteFile(f) => {
                    let data = ps.clone();
                    self.write_to(&f, &data)?;
                }
                Cmd::Zap => ps.clear(),
            }
        }
        Ok(Flow::EndCycle { print: true })
    }

    fn jump(&self, label: &str) -> io::Result<usize> {
        if label.is_empty() {
            return Ok(self.prog.len());
        }
        self.prog
            .iter()
            .position(|i| matches!(&i.cmd, Cmd::Label(l) if l == label))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("can't find label for jump to `{label}'")))
    }

    fn write_to(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        if name == "/dev/stdout" {
            let mut o = io::stdout();
            o.write_all(data)?;
            return o.write_all(b"\n");
        }
        if !self.files.contains_key(name) {
            self.files.insert(name.to_string(), fs::File::create(name)?);
        }
        let f = self.files.get_mut(name).unwrap();
        f.write_all(data)?;
        f.write_all(b"\n")
    }

    fn run(&mut self, input: &mut Input, out: &mut dyn Write) -> io::Result<i32> {
        let mut hold = Vec::new();
        let mut line_no = 0usize;
        let mut ps: Vec<u8> = Vec::new();
        let mut have = false;
        loop {
            if !have {
                match input.next() {
                    Some(l) => {
                        ps = l;
                        line_no += 1;
                    }
                    None => return Ok(0),
                }
            }
            have = false;
            let mut appends = Vec::new();
            let mut subst = false;
            let flow = self.run_cycle(&mut ps, &mut hold, input, &mut line_no, out, &mut appends, &mut subst)?;
            let last_line = input.is_last();
            let end_line = |out: &mut dyn Write, ps: &[u8]| -> io::Result<()> {
                out.write_all(ps)?;
                if !(last_line && input.missing_newline) {
                    out.write_all(b"\n")?;
                }
                Ok(())
            };
            match flow {
                Flow::EndCycle { print } => {
                    if print && !self.quiet {
                        end_line(out, &ps)?;
                    }
                }
                Flow::Restart => {
                    flush_appends(out, &mut appends)?;
                    have = true;
                    continue;
                }
                Flow::Quit(code, print) => {
                    if print && !self.quiet {
                        end_line(out, &ps)?;
                    }
                    flush_appends(out, &mut appends)?;
                    return Ok(code);
                }
            }
            flush_appends(out, &mut appends)?;
        }
    }
}

fn flush_appends(out: &mut dyn Write, q: &mut Vec<Vec<u8>>) -> io::Result<()> {
    for t in q.drain(..) {
        out.write_all(&t)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

const SED_USAGE: &str = "usage: sed [-nEr] [-i[SUFFIX]] [-s] SCRIPT [FILE...]\n       sed [-nEr] -e SCRIPT... [-f FILE]... [FILE...]";

pub fn sed(args: &[String]) -> i32 {
    let mut scripts: Vec<u8> = Vec::new();
    let mut have_script = false;
    let mut quiet = false;
    let mut syntax = Syntax::Basic;
    let mut in_place: Option<String> = None;
    let mut separate = false;
    let mut files = Vec::new();
    let mut i = 0;
    let mut only_files = false;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if only_files || !a.starts_with('-') || a == "-" {
            files.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => only_files = true,
            "-n" | "--quiet" | "--silent" => quiet = true,
            "-E" | "-r" | "--regexp-extended" => syntax = Syntax::Extended,
            "-s" | "--separate" => separate = true,
            "-e" | "-f" => {
                let Some(v) = args.get(i) else {
                    error("sed", format!("option requires an argument -- '{}'", &a[1..]));
                    return 1;
                };
                i += 1;
                let text = if a == "-e" {
                    v.as_bytes().to_vec()
                } else {
                    match read_input(v) {
                        Ok(d) => d,
                        Err(e) => {
                            error("sed", format!("couldn't open file {v}: {e}"));
                            return 1;
                        }
                    }
                };
                if have_script {
                    scripts.push(b'\n');
                }
                scripts.extend(text);
                have_script = true;
            }
            s if s.starts_with("-i") => {
                in_place = Some(s[2..].to_string());
                separate = true;
            }
            s if s.starts_with("--in-place") => {
                in_place = Some(s.strip_prefix("--in-place=").unwrap_or("").to_string());
                separate = true;
            }
            s if s.len() > 2 && !s.starts_with("--") => {
                // combined like -ne
                let mut consumed_next = false;
                for (k, ch) in s[1..].chars().enumerate() {
                    match ch {
                        'n' => quiet = true,
                        'E' | 'r' => syntax = Syntax::Extended,
                        's' => separate = true,
                        'e' if k + 2 == s.len() => {
                            if let Some(v) = args.get(i) {
                                if have_script {
                                    scripts.push(b'\n');
                                }
                                scripts.extend(v.as_bytes());
                                have_script = true;
                                consumed_next = true;
                            }
                        }
                        _ => {
                            error("sed", format!("invalid option -- '{ch}'\n{SED_USAGE}"));
                            return 1;
                        }
                    }
                }
                if consumed_next {
                    i += 1;
                }
            }
            _ => {
                error("sed", format!("unknown option {a}\n{SED_USAGE}"));
                return 1;
            }
        }
    }
    if !have_script {
        if files.is_empty() {
            error("sed", SED_USAGE);
            return 1;
        }
        scripts = files.remove(0).into_bytes();
    }
    let mut prog = Vec::new();
    let mut blocks = Vec::new();
    let mut parser = SedParser { s: &scripts, i: 0, syntax };
    if let Err(e) = parser.parse(&mut prog, &mut blocks) {
        error("sed", format!("-e expression #1, char {}: {e}", parser.i));
        return 1;
    }
    if !blocks.is_empty() {
        error("sed", "-e expression #1: unmatched `{'");
        return 1;
    }
    let n = prog.len();
    let mut sed = Sed { prog, quiet, in_range: vec![false; n], range_start: vec![0; n], last_re: None, files: HashMap::new() };
    if let Some(i) = sed.prog.iter().find_map(|i| match &i.cmd {
        Cmd::Branch(l) | Cmd::BranchIf(l, _) if !l.is_empty() => Some(l.clone()),
        _ => None,
    }) {
        if sed.jump(&i).is_err() {
            error("sed", format!("can't find label for jump to `{i}'"));
            return 1;
        }
    }
    if files.is_empty() {
        if in_place.is_some() {
            error("sed", "no input files");
            return 1;
        }
        files.push("-".into());
    }
    let read = |f: &str| -> Result<Input, String> {
        let d = read_input(f).map_err(|e| format!("can't read {f}: {e}"))?;
        let missing_newline = !d.is_empty() && !d.ends_with(b"\n");
        Ok(Input { lines: lines(&d).into_iter().map(<[u8]>::to_vec).collect(), pos: 0, missing_newline })
    };
    let mut status = 0;
    if separate {
        for f in &files {
            let mut input = match read(f) {
                Ok(i) => i,
                Err(e) => {
                    error("sed", e);
                    status = 2;
                    continue;
                }
            };
            sed.in_range.iter_mut().for_each(|r| *r = false);
            if let Some(suffix) = &in_place {
                let mut buf = Vec::new();
                let r = sed.run(&mut input, &mut buf);
                match r {
                    Ok(code) => {
                        if !suffix.is_empty() {
                            let _ = fs::copy(f, format!("{f}{suffix}"));
                        }
                        let tmp = format!("{f}.sed-tmp-{}", std::process::id());
                        let mode = fs::metadata(f).map(|m| m.permissions());
                        if let Err(e) = fs::write(&tmp, &buf).and_then(|_| {
                            if let Ok(p) = mode {
                                fs::set_permissions(&tmp, p)?;
                            }
                            fs::rename(&tmp, f)
                        }) {
                            let _ = fs::remove_file(&tmp);
                            error("sed", format!("couldn't write {f}: {e}"));
                            status = 4;
                        }
                        if code != 0 {
                            return code;
                        }
                    }
                    Err(e) => {
                        error("sed", e);
                        return 4;
                    }
                }
            } else {
                let stdout = io::stdout();
                let mut out = io::BufWriter::new(stdout.lock());
                match sed.run(&mut input, &mut out) {
                    Ok(code) if code != 0 => {
                        let _ = out.flush();
                        return code;
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return 0,
                    Err(e) => {
                        error("sed", e);
                        return 4;
                    }
                }
                if finish_output("sed", out.flush()) != 0 {
                    return 4;
                }
            }
        }
        return status;
    }
    // One stream across all files.
    let mut all = Input { lines: Vec::new(), pos: 0, missing_newline: false };
    for f in &files {
        match read(f) {
            Ok(i) => {
                all.missing_newline = i.missing_newline;
                all.lines.extend(i.lines);
            }
            Err(e) => {
                error("sed", e);
                status = 2;
            }
        }
    }
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    match sed.run(&mut all, &mut out) {
        Ok(code) => {
            if finish_output("sed", out.flush()) != 0 {
                return 4;
            }
            if code != 0 {
                code
            } else {
                status
            }
        }
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            error("sed", e);
            4
        }
    }
}

// ---------------------------------------------------------------------------------------
// sort

#[derive(Clone, Debug, Default)]
struct KeySpec {
    start_field: usize,
    start_char: usize,
    end_field: Option<usize>,
    end_char: usize,
    numeric: bool,
    reverse: bool,
    fold: bool,
    blanks: bool,
    human: bool,
    general: bool,
}

fn parse_key(spec: &str, global: &KeySpec) -> Result<KeySpec, String> {
    let mut k = KeySpec { numeric: global.numeric, reverse: global.reverse, fold: global.fold, blanks: global.blanks, human: global.human, general: global.general, ..Default::default() };
    let (start, end) = match spec.split_once(',') {
        Some((a, b)) => (a, Some(b)),
        None => (spec, None),
    };
    let mut own_flags = false;
    let mut take = |part: &str, k: &mut KeySpec| -> Result<(usize, usize), String> {
        let digits_end = part.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(part.len());
        let (pos, flags) = part.split_at(digits_end);
        for f in flags.chars() {
            if !own_flags {
                own_flags = true;
                k.numeric = false;
                k.reverse = false;
                k.fold = false;
                k.blanks = false;
                k.human = false;
                k.general = false;
            }
            match f {
                'n' => k.numeric = true,
                'r' => k.reverse = true,
                'f' => k.fold = true,
                'b' => k.blanks = true,
                'h' => k.human = true,
                'g' => k.general = true,
                _ => return Err(format!("invalid key option '{f}'")),
            }
        }
        let (f, c) = match pos.split_once('.') {
            Some((f, c)) => (f, c),
            None => (pos, ""),
        };
        let f: usize = f.parse().map_err(|_| format!("invalid key '{spec}'"))?;
        let c: usize = if c.is_empty() { 0 } else { c.parse().map_err(|_| format!("invalid key '{spec}'"))? };
        Ok((f, c))
    };
    let (sf, sc) = take(start, &mut k)?;
    if sf == 0 {
        return Err("fields count from 1".into());
    }
    k.start_field = sf;
    k.start_char = sc.max(1);
    if let Some(e) = end {
        let (ef, ec) = take(e, &mut k)?;
        k.end_field = Some(ef);
        k.end_char = ec;
    }
    Ok(k)
}

/// The fields of a line: with a separator, split on it; otherwise each field starts with
/// its leading blanks (as POSIX sort does).
fn fields(line: &[u8], sep: Option<u8>) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    match sep {
        Some(s) => {
            let mut st = 0;
            for (i, &b) in line.iter().enumerate() {
                if b == s {
                    v.push((st, i));
                    st = i + 1;
                }
            }
            v.push((st, line.len()));
        }
        None => {
            let mut i = 0;
            while i < line.len() {
                let st = i;
                while i < line.len() && (line[i] == b' ' || line[i] == b'\t') {
                    i += 1;
                }
                while i < line.len() && line[i] != b' ' && line[i] != b'\t' {
                    i += 1;
                }
                v.push((st, i));
            }
        }
    }
    v
}

fn key_slice<'a>(line: &'a [u8], k: &KeySpec, sep: Option<u8>) -> &'a [u8] {
    let f = fields(line, sep);
    let skip_blanks = |s: &[u8]| -> usize { s.iter().take_while(|&&b| b == b' ' || b == b'\t').count() };
    let start = match f.get(k.start_field - 1) {
        Some(&(s, e)) => {
            let s = if k.blanks { s + skip_blanks(&line[s..e]) } else { s };
            (s + k.start_char - 1).min(e.max(s))
        }
        None => return &[],
    };
    let end = match k.end_field {
        None => line.len(),
        Some(ef) => match f.get(ef - 1) {
            Some(&(s, e)) => {
                if k.end_char == 0 {
                    e
                } else {
                    let s = if k.blanks { s + skip_blanks(&line[s..e]) } else { s };
                    (s + k.end_char).min(e)
                }
            }
            None => line.len(),
        },
    };
    if end <= start {
        &[]
    } else {
        &line[start..end]
    }
}

fn num_prefix(s: &[u8], human: bool) -> f64 {
    let t = String::from_utf8_lossy(s);
    let t = t.trim_start();
    let mut end = 0;
    for (i, c) in t.char_indices() {
        if c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+')) {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    let mut n: f64 = t[..end].parse().unwrap_or(0.0);
    if human {
        let mult = match t[end..].chars().next() {
            Some('K' | 'k') => 1e3,
            Some('M') => 1e6,
            Some('G') => 1e9,
            Some('T') => 1e12,
            Some('P') => 1e15,
            _ => 1.0,
        };
        n *= mult;
    }
    n
}

fn compare_keys(a: &[u8], b: &[u8], k: &KeySpec) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ord = if k.numeric || k.human || k.general {
        num_prefix(a, k.human).partial_cmp(&num_prefix(b, k.human)).unwrap_or(Ordering::Equal)
    } else {
        let trim = |s: &[u8]| -> Vec<u8> {
            let s = if k.blanks { &s[s.iter().take_while(|&&c| c == b' ' || c == b'\t').count()..] } else { s };
            if k.fold {
                s.to_ascii_lowercase()
            } else {
                s.to_vec()
            }
        };
        trim(a).cmp(&trim(b))
    };
    if k.reverse {
        ord.reverse()
    } else {
        ord
    }
}

pub fn sort(args: &[String]) -> i32 {
    let p = match parse(args, "nrufbsmhgcC", "kto") {
        Ok(p) => p,
        Err(e) => {
            error("sort", format!("{e}\nusage: sort [-bcfghmnrsu] [-k KEY]... [-t SEP] [-o FILE] [FILE...]"));
            return 2;
        }
    };
    let global = KeySpec { numeric: p.has('n'), reverse: p.has('r'), fold: p.has('f'), blanks: p.has('b'), human: p.has('h'), general: p.has('g'), start_field: 1, start_char: 1, ..Default::default() };
    let sep = match p.value('t') {
        Some(t) if t.len() == 1 => Some(t.as_bytes()[0]),
        Some("\\t") => Some(b'\t'),
        Some(t) if t.is_empty() => None,
        Some(_) => {
            error("sort", "the separator must be one character");
            return 2;
        }
        None => None,
    };
    // All -k options, in order.
    let mut keys = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let spec = if a == "-k" {
            it.next().cloned()
        } else if let Some(r) = a.strip_prefix("-k") {
            Some(r.to_string())
        } else {
            None
        };
        if let Some(s) = spec {
            match parse_key(&s, &global) {
                Ok(k) => keys.push(k),
                Err(e) => {
                    error("sort", e);
                    return 2;
                }
            }
        }
    }
    let mut files = p.operands.clone();
    if files.is_empty() {
        files.push("-".into());
    }
    let mut data = Vec::new();
    for f in &files {
        match read_input(f) {
            Ok(d) => {
                data.extend_from_slice(&d);
                if !d.is_empty() && !d.ends_with(b"\n") {
                    data.push(b'\n');
                }
            }
            Err(e) => {
                error("sort", format!("{f}: {e}"));
                return 2;
            }
        }
    }
    let mut ls: Vec<&[u8]> = lines(&data);
    let cmp = |a: &&[u8], b: &&[u8]| -> std::cmp::Ordering {
        if keys.is_empty() {
            let o = compare_keys(a, b, &global);
            if o != std::cmp::Ordering::Equal || p.has('s') || p.has('u') {
                return o;
            }
        } else {
            for k in &keys {
                let o = compare_keys(key_slice(a, k, sep), key_slice(b, k, sep), k);
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            if p.has('s') || p.has('u') {
                return std::cmp::Ordering::Equal;
            }
        }
        // Last resort: the whole line, byte by byte.
        let o = a.cmp(b);
        if global.reverse {
            o.reverse()
        } else {
            o
        }
    };
    if p.has('c') || p.has('C') {
        for w in ls.windows(2) {
            let o = cmp(&w[0], &w[1]);
            if o == std::cmp::Ordering::Greater || (p.has('u') && o == std::cmp::Ordering::Equal) {
                if p.has('c') {
                    error("sort", format!("disorder: {}", String::from_utf8_lossy(w[1])));
                }
                return 1;
            }
        }
        return 0;
    }
    ls.sort_by(cmp);
    if p.has('u') {
        ls.dedup_by(|a, b| cmp(a, b) == std::cmp::Ordering::Equal);
    }
    let mut out_data = Vec::with_capacity(data.len());
    for l in ls {
        out_data.extend_from_slice(l);
        out_data.push(b'\n');
    }
    if let Some(o) = p.value('o') {
        if let Err(e) = fs::write(o, &out_data) {
            error("sort", format!("{o}: {e}"));
            return 2;
        }
        return 0;
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    finish_output("sort", out.write_all(&out_data).and_then(|_| out.flush()))
}

// ---------------------------------------------------------------------------------------
// uniq

pub fn uniq(args: &[String]) -> i32 {
    let p = match parse(args, "cdui", "fs") {
        Ok(p) => p,
        Err(e) => {
            error("uniq", format!("{e}\nusage: uniq [-cdui] [-f FIELDS] [-s CHARS] [INPUT [OUTPUT]]"));
            return 1;
        }
    };
    let skip_fields: usize = p.value('f').and_then(|v| v.parse().ok()).unwrap_or(0);
    let skip_chars: usize = p.value('s').and_then(|v| v.parse().ok()).unwrap_or(0);
    let input = p.operands.first().map_or("-", String::as_str);
    let data = match read_input(input) {
        Ok(d) => d,
        Err(e) => {
            error("uniq", format!("{input}: {e}"));
            return 1;
        }
    };
    let key = |l: &[u8]| -> Vec<u8> {
        let f = fields(l, None);
        let mut start = f.get(skip_fields).map_or(l.len(), |x| x.0);
        if skip_fields == 0 {
            start = 0;
        }
        let s = &l[start.min(l.len())..];
        let s = &s[skip_chars.min(s.len())..];
        if p.has('i') {
            s.to_ascii_lowercase()
        } else {
            s.to_vec()
        }
    };
    let mut out_buf = Vec::new();
    let ls = lines(&data);
    let mut i = 0;
    while i < ls.len() {
        let k = key(ls[i]);
        let mut j = i + 1;
        while j < ls.len() && key(ls[j]) == k {
            j += 1;
        }
        let n = j - i;
        let show = (!p.has('d') || n > 1) && (!p.has('u') || n == 1);
        if show {
            if p.has('c') {
                out_buf.extend(format!("{n:7} ").into_bytes());
            }
            out_buf.extend_from_slice(ls[i]);
            out_buf.push(b'\n');
        }
        i = j;
    }
    if let Some(o) = p.operands.get(1) {
        if let Err(e) = fs::write(o, &out_buf) {
            error("uniq", format!("{o}: {e}"));
            return 1;
        }
        return 0;
    }
    let mut out = io::stdout().lock();
    finish_output("uniq", out.write_all(&out_buf).and_then(|_| out.flush()))
}

// ---------------------------------------------------------------------------------------
// tr

/// Expand a tr set: ranges, \escapes, [:class:], [c*n].
fn tr_set(s: &str) -> Result<Vec<char>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let esc = |c: &[char], i: &mut usize| -> char {
        let ch = c[*i];
        *i += 1;
        if ch != '\\' || *i >= c.len() {
            return ch;
        }
        let n = c[*i];
        *i += 1;
        match n {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '\\' => '\\',
            'a' => '\x07',
            'b' => '\x08',
            'f' => '\x0C',
            'v' => '\x0B',
            '0'..='7' => {
                let mut v = n.to_digit(8).unwrap();
                for _ in 0..2 {
                    if *i < c.len() && c[*i].is_digit(8) {
                        v = v * 8 + c[*i].to_digit(8).unwrap();
                        *i += 1;
                    }
                }
                char::from_u32(v).unwrap_or('?')
            }
            other => other,
        }
    };
    while i < c.len() {
        if c[i] == '[' && i + 1 < c.len() && c[i + 1] == ':' {
            if let Some(end) = s[s.char_indices().nth(i).map_or(0, |x| x.0)..].find(":]") {
                let start_b = s.char_indices().nth(i).map_or(0, |x| x.0);
                let name = &s[start_b + 2..start_b + end];
                let set: Vec<char> = (0u8..=127).map(char::from).filter(|&ch| match name {
                    "alpha" => ch.is_ascii_alphabetic(),
                    "digit" => ch.is_ascii_digit(),
                    "alnum" => ch.is_ascii_alphanumeric(),
                    "upper" => ch.is_ascii_uppercase(),
                    "lower" => ch.is_ascii_lowercase(),
                    "space" => ch.is_ascii_whitespace() || ch == '\x0B',
                    "blank" => ch == ' ' || ch == '\t',
                    "punct" => ch.is_ascii_punctuation(),
                    "print" => (' '..='~').contains(&ch),
                    "graph" => ('!'..='~').contains(&ch),
                    "cntrl" => ch.is_ascii_control(),
                    "xdigit" => ch.is_ascii_hexdigit(),
                    _ => false,
                }).collect();
                if set.is_empty() {
                    return Err(format!("invalid character class '{name}'"));
                }
                out.extend(set);
                i += name.chars().count() + 4;
                continue;
            }
        }
        if c[i] == '[' && i + 2 < c.len() && c[i + 2] == '*' {
            // [c*n]
            if let Some(close) = c[i + 3..].iter().position(|&x| x == ']') {
                let n: String = c[i + 3..i + 3 + close].iter().collect();
                let count = if n.is_empty() { 0 } else { n.parse::<usize>().map_err(|_| format!("invalid repeat count '{n}'"))? };
                // count 0 means "as many as needed": marked with a sentinel, resolved by the caller
                out.extend(std::iter::repeat(c[i + 1]).take(if count == 0 { 1 } else { count }));
                if count == 0 {
                    out.push('\u{F8FF}');
                }
                i += 4 + close;
                continue;
            }
        }
        let lo = esc(&c, &mut i);
        if i + 1 < c.len() && c[i] == '-' {
            i += 1;
            let hi = esc(&c, &mut i);
            if hi < lo {
                return Err(format!("range-endpoints of '{lo}-{hi}' are in reverse collating sequence order"));
            }
            out.extend((lo as u32..=hi as u32).filter_map(char::from_u32));
        } else {
            out.push(lo);
        }
    }
    Ok(out)
}

pub fn tr(args: &[String]) -> i32 {
    let p = match parse(args, "cCdst", "") {
        Ok(p) => p,
        Err(e) => {
            error("tr", format!("{e}\nusage: tr [-cdst] SET1 [SET2]"));
            return 1;
        }
    };
    let (complement, delete, squeeze) = (p.has('c') || p.has('C'), p.has('d'), p.has('s'));
    let ops = &p.operands;
    let need = if delete && !squeeze { 1 } else if delete || !squeeze { 2 } else { 1 };
    if ops.len() < need || ops.len() > 2 {
        error("tr", "usage: tr [-cdst] SET1 [SET2]");
        return 1;
    }
    let set1 = match tr_set(&ops[0]) {
        Ok(s) => s,
        Err(e) => {
            error("tr", e);
            return 1;
        }
    };
    let mut set2 = match ops.get(1).map(|s| tr_set(s)) {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            error("tr", e);
            return 1;
        }
        None => Vec::new(),
    };
    let in_set1 = |c: char| set1.contains(&c) != complement;
    let mut data = String::new();
    let mut raw = Vec::new();
    if let Err(e) = io::stdin().lock().read_to_end(&mut raw) {
        error("tr", e);
        return 1;
    }
    data.push_str(&String::from_utf8_lossy(&raw));
    // Translation map (not with -d unless -s uses set2 for squeezing).
    let translating = !delete && ops.len() == 2;
    if translating {
        if let Some(pos) = set2.iter().position(|&c| c == '\u{F8FF}') {
            // [c*] fills SET2 up to SET1's length
            let fill = set2[pos - 1];
            set2.remove(pos);
            while set2.len() < set1.len() {
                set2.insert(pos, fill);
            }
        }
        if let Some(&last) = set2.last() {
            while set2.len() < set1.len() {
                set2.push(last);
            }
        }
        if p.has('t') {
            set2.truncate(set1.len());
        }
    }
    let map = |c: char| -> char {
        if complement {
            if set1.contains(&c) {
                c
            } else {
                *set2.last().unwrap_or(&c)
            }
        } else {
            set1.iter().rposition(|&x| x == c).and_then(|k| set2.get(k).copied()).unwrap_or(c)
        }
    };
    let squeeze_set: Vec<char> = if squeeze { if ops.len() == 2 { set2.clone() } else { set1.clone() } } else { Vec::new() };
    let squeeze_hit = |c: char| -> bool {
        if ops.len() == 2 || !complement {
            squeeze_set.contains(&c)
        } else {
            !squeeze_set.contains(&c)
        }
    };
    let mut out = String::with_capacity(data.len());
    let mut last: Option<char> = None;
    for c in data.chars() {
        if delete && in_set1(c) {
            continue;
        }
        let c = if translating && in_set1(c) { map(c) } else { c };
        if squeeze && last == Some(c) && squeeze_hit(c) {
            continue;
        }
        out.push(c);
        last = Some(c);
    }
    let mut o = io::stdout().lock();
    finish_output("tr", o.write_all(out.as_bytes()).and_then(|_| o.flush()))
}

// ---------------------------------------------------------------------------------------
// cut

fn parse_list(s: &str) -> Result<Vec<(usize, usize)>, String> {
    let mut v = Vec::new();
    for part in s.split(',') {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (if a.is_empty() { 1 } else { a.parse().map_err(|_| format!("invalid list '{s}'"))? }, if b.is_empty() { usize::MAX } else { b.parse().map_err(|_| format!("invalid list '{s}'"))? }),
            None => {
                let n: usize = part.parse().map_err(|_| format!("invalid list '{s}'"))?;
                (n, n)
            }
        };
        if a == 0 {
            return Err("fields and positions are numbered from 1".into());
        }
        v.push((a, b));
    }
    Ok(v)
}

pub fn cut(args: &[String]) -> i32 {
    let p = match parse(args, "sn", "bcfd") {
        Ok(p) => p,
        Err(e) => {
            error("cut", format!("{e}\nusage: cut -b LIST | -c LIST | -f LIST [-d DELIM] [-s] [FILE...]"));
            return 1;
        }
    };
    let (mode, list) = match (p.value('b'), p.value('c'), p.value('f')) {
        (Some(l), None, None) => ('b', l),
        (None, Some(l), None) => ('c', l),
        (None, None, Some(l)) => ('f', l),
        _ => {
            error("cut", "give exactly one of -b, -c, -f");
            return 1;
        }
    };
    let ranges = match parse_list(list) {
        Ok(r) => r,
        Err(e) => {
            error("cut", e);
            return 1;
        }
    };
    let want = |i: usize| ranges.iter().any(|&(a, b)| i >= a && i <= b);
    let delim = match p.value('d') {
        Some(d) if d.chars().count() == 1 => d.to_string(),
        Some(_) => {
            error("cut", "the delimiter must be a single character");
            return 1;
        }
        None => "\t".into(),
    };
    let mut files = p.operands.clone();
    if files.is_empty() {
        files.push("-".into());
    }
    let mut out = Vec::new();
    let mut status = 0;
    for f in &files {
        let data = match read_input(f) {
            Ok(d) => d,
            Err(e) => {
                error("cut", format!("{f}: {e}"));
                status = 1;
                continue;
            }
        };
        for l in lines(&data) {
            match mode {
                'b' => {
                    for (i, b) in l.iter().enumerate() {
                        if want(i + 1) {
                            out.push(*b);
                        }
                    }
                }
                'c' => {
                    let s = String::from_utf8_lossy(l);
                    out.extend(s.chars().enumerate().filter(|(i, _)| want(i + 1)).map(|(_, c)| c).collect::<String>().into_bytes());
                }
                _ => {
                    let s = String::from_utf8_lossy(l);
                    if !s.contains(delim.as_str()) {
                        if !p.has('s') {
                            out.extend_from_slice(l);
                            out.push(b'\n');
                        }
                        continue;
                    }
                    let picked: Vec<&str> = s.split(delim.as_str()).enumerate().filter(|(i, _)| want(i + 1)).map(|(_, x)| x).collect();
                    out.extend(picked.join(&delim).into_bytes());
                }
            }
            out.push(b'\n');
        }
    }
    let mut o = io::stdout().lock();
    let r = finish_output("cut", o.write_all(&out).and_then(|_| o.flush()));
    if r != 0 {
        r
    } else {
        status
    }
}

// ---------------------------------------------------------------------------------------
// tee

pub fn tee(args: &[String]) -> i32 {
    let p = match parse(args, "ai", "") {
        Ok(p) => p,
        Err(e) => {
            error("tee", format!("{e}\nusage: tee [-ai] [FILE...]"));
            return 1;
        }
    };
    if p.has('i') {
        // SAFETY: ignoring SIGINT has no preconditions.
        unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    }
    let mut status = 0;
    let mut outs: Vec<(String, fs::File)> = Vec::new();
    for f in &p.operands {
        let r = if p.has('a') { fs::OpenOptions::new().append(true).create(true).open(f) } else { fs::File::create(f) };
        match r {
            Ok(h) => outs.push((f.clone(), h)),
            Err(e) => {
                error("tee", format!("{f}: {e}"));
                status = 1;
            }
        }
    }
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut stdout_ok = true;
    let mut buf = [0u8; 65536];
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                error("tee", e);
                return 1;
            }
        };
        if stdout_ok && stdout.write_all(&buf[..n]).and_then(|_| stdout.flush()).is_err() {
            stdout_ok = false;
        }
        outs.retain_mut(|(name, f)| match f.write_all(&buf[..n]) {
            Ok(()) => true,
            Err(e) => {
                error("tee", format!("{name}: {e}"));
                status = 1;
                false
            }
        });
    }
    status
}

// ---------------------------------------------------------------------------------------
// xargs

pub fn xargs(args: &[String]) -> i32 {
    // Options end at the command: everything from its name on belongs to it.
    let first_cmd = {
        let mut k = 0;
        while k < args.len() {
            let a = &args[k];
            if a == "--" {
                k += 1;
                break;
            }
            if !a.starts_with('-') || a == "-" {
                break;
            }
            // options that take a separate value: -n N, -I R, -d D, -L N, -s N
            if matches!(a.as_str(), "-n" | "-I" | "-d" | "-L" | "-s") {
                k += 1;
            }
            k += 1;
        }
        k
    };
    let opts: Vec<String> = args[..first_cmd.min(args.len())].iter().filter(|a| *a != "--").cloned().collect();
    let p = match parse(&opts, "0rtpx", "nIdLs") {
        Ok(p) => p,
        Err(e) => {
            error("xargs", format!("{e}\nusage: xargs [-0rt] [-n MAX] [-I REPLACE] [-d DELIM] [COMMAND [ARGS...]]"));
            return 1;
        }
    };
    let cmd: Vec<String> = if first_cmd < args.len() { args[first_cmd..].to_vec() } else { vec!["echo".into()] };
    let mut input = Vec::new();
    if let Err(e) = io::stdin().lock().read_to_end(&mut input) {
        error("xargs", e);
        return 1;
    }
    let items: Vec<String> = if p.has('0') {
        input.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
    } else if let Some(d) = p.value('d') {
        let d = match d {
            "\\n" => '\n',
            "\\t" => '\t',
            _ => d.chars().next().unwrap_or('\n'),
        };
        String::from_utf8_lossy(&input).split(d).filter(|s| !s.is_empty()).map(String::from).collect()
    } else if p.value('I').is_some() || p.value('L').is_some() {
        String::from_utf8_lossy(&input).lines().map(|l| l.trim_start().to_string()).filter(|l| !l.is_empty()).collect()
    } else {
        match split_xargs(&String::from_utf8_lossy(&input)) {
            Ok(v) => v,
            Err(e) => {
                error("xargs", e);
                return 1;
            }
        }
    };
    if items.is_empty() && (p.has('r') || p.value('I').is_some()) {
        return 0;
    }
    let max: usize = match p.value('n').or(p.value('L')) {
        Some(n) => match n.parse() {
            Ok(n) if n > 0 => n,
            _ => {
                error("xargs", "-n needs a positive number");
                return 1;
            }
        },
        None => if p.value('I').is_some() { 1 } else { 5000 },
    };
    let batches: Vec<Vec<String>> = if items.is_empty() { vec![Vec::new()] } else { items.chunks(max).map(<[String]>::to_vec).collect() };
    let mut status = 0;
    for batch in batches {
        let argv: Vec<String> = match p.value('I') {
            Some(rep) => cmd.iter().map(|a| a.replace(rep, &batch.join(" "))).collect(),
            None => cmd.iter().cloned().chain(batch).collect(),
        };
        if p.has('t') || p.has('p') {
            eprintln!("{}", argv.join(" "));
        }
        if p.has('p') {
            eprint!("?...");
            let mut ans = String::new();
            let tty = fs::File::open("/dev/tty");
            if let Ok(t) = tty {
                let _ = BufReader::new(t).read_line(&mut ans);
            }
            if !ans.trim_start().starts_with(['y', 'Y']) {
                continue;
            }
        }
        match Command::new(&argv[0]).args(&argv[1..]).status() {
            Ok(s) => match s.code() {
                Some(255) => {
                    error("xargs", format!("{}: exited with status 255; aborting", argv[0]));
                    return 124;
                }
                Some(0) => {}
                Some(_) => status = 123,
                None => {
                    error("xargs", format!("{}: terminated by a signal", argv[0]));
                    return 125;
                }
            },
            Err(e) => {
                error("xargs", format!("{}: {e}", argv[0]));
                return if e.kind() == io::ErrorKind::NotFound { 127 } else { 126 };
            }
        }
    }
    status
}

/// xargs input: blank-separated, with '...' "..." and backslash quoting.
fn split_xargs(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            '\'' | '"' => {
                have = true;
                loop {
                    match chars.next() {
                        Some(x) if x == c => break,
                        Some('\n') | None => return Err(format!("unmatched {} quote", if c == '\'' { "single" } else { "double" })),
                        Some(x) => cur.push(x),
                    }
                }
            }
            '\\' => {
                have = true;
                if let Some(x) = chars.next() {
                    cur.push(x);
                }
            }
            _ => {
                have = true;
                cur.push(c);
            }
        }
    }
    if have {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sed_replacement_text() {
        let re = Regex::new("\\(a\\)\\(b*\\)", Syntax::Basic, false).unwrap();
        let caps = re.captures_at(b"xabbz", 0).unwrap();
        assert_eq!(expand_repl(b"<&|\\2\\1\\n\\&>", &caps, b"xabbz"), b"<abb|bba\n&>");
    }

    #[test]
    fn tr_sets() {
        assert_eq!(tr_set("a-e").unwrap(), vec!['a', 'b', 'c', 'd', 'e']);
        assert_eq!(tr_set("\\n\\t").unwrap(), vec!['\n', '\t']);
        assert_eq!(tr_set("[:digit:]").unwrap().len(), 10);
        assert!(tr_set("z-a").is_err());
    }

    #[test]
    fn sort_keys() {
        let g = KeySpec { start_field: 1, start_char: 1, ..Default::default() };
        let k = parse_key("2,2n", &g).unwrap();
        assert!(k.numeric);
        assert_eq!(key_slice(b"b 10 x", &k, None), b" 10");
        let k = parse_key("2", &g).unwrap();
        assert_eq!(key_slice(b"a:b:c", &k, Some(b':')), b"b:c");
        let k = parse_key("1.2,1.3", &g).unwrap();
        assert_eq!(key_slice(b"abcd", &k, None), b"bc");
        assert_eq!(num_prefix(b" -3.5x", false), -3.5);
        assert_eq!(num_prefix(b"2K", true), 2000.0);
    }

    #[test]
    fn xargs_quoting() {
        assert_eq!(split_xargs("a 'b c' \"d\\e\" f\\ g\n").unwrap(), ["a", "b c", "d\\e", "f g"]);
        assert!(split_xargs("'open").is_err());
    }

    #[test]
    fn cut_lists() {
        assert_eq!(parse_list("1,3-4,6-").unwrap(), [(1, 1), (3, 4), (6, usize::MAX)]);
        assert!(parse_list("0").is_err());
    }
}
