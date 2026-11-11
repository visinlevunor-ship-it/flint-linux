//! less: a pager. Reads files (or standard input), shows them a screen at a time, scrolls
//! both ways and searches. Keys come from /dev/tty, so `cmd | less` works. When the output
//! is not a terminal it just copies its input, like cat.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;

use flint_core::term::{self, Key, Raw, Screen};

use crate::regex::{Regex, Syntax};
use crate::util::error;

const HELP: &str = "\
  less keys

  q  Q            quit
  SPACE  f  ^F    forward one screen          b  ^B       back one screen
  j  ENTER  ^N    forward one line            k  y  ^P    back one line
  d  ^D           forward half a screen       u  ^U       back half a screen
  g  <  HOME      first line                  G  >  END   last line
  /PATTERN        search forward              ?PATTERN    search backward
  n               next match                  N           previous match
  :n  :p          next / previous file        -N          toggle line numbers
  -S              toggle chopping long lines  r  ^L       redraw
  h  H            this help

  (press q to leave the help)";

struct Doc {
    name: String,
    lines: Vec<String>,
}

/// One screen row: (line index, start column in chars) when lines wrap.
type Row = (usize, usize);

struct Pager {
    docs: Vec<Doc>,
    cur: usize,
    top: Row,
    rows: usize,
    cols: usize,
    numbers: bool,
    chop: bool,
    raw_ctrl: bool,
    search: Option<(Regex, bool)>, // regex, forward
    message: Option<String>,
    quit_at_end: bool,
}

/// A display cell: the character shown, whether it belongs to a passed-through escape
/// sequence (-R), and the byte offset in the line it came from.
type Cell = (char, bool, usize);

/// Expand tabs and show control characters as ^X (or keep ANSI SGR sequences with -R).
fn render_line(line: &str, raw_ctrl: bool) -> Vec<Cell> {
    let mut out = Vec::new();
    let mut col = 0;
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            '\t' => {
                let n = 8 - col % 8;
                for _ in 0..n {
                    out.push((' ', false, at));
                }
                col += n;
            }
            '\x1b' if raw_ctrl && chars.peek().map(|x| x.1) == Some('[') => {
                out.push((c, true, at));
                for (k, d) in chars.by_ref() {
                    out.push((d, true, k));
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            c if (c as u32) < 0x20 || c == '\x7f' => {
                out.push(('^', false, at));
                out.push((if c == '\x7f' { '?' } else { (c as u8 + b'@') as char }, false, at));
                col += 2;
            }
            c => {
                out.push((c, false, at));
                col += term::char_width(c);
            }
        }
    }
    out
}

impl Pager {
    fn doc(&self) -> &Doc {
        &self.docs[self.cur]
    }

    fn text_cols(&self) -> usize {
        if self.numbers {
            self.cols.saturating_sub(8).max(10)
        } else {
            self.cols.max(10)
        }
    }

    /// Screen rows a line takes.
    fn line_rows(&self, i: usize) -> usize {
        if self.chop {
            return 1;
        }
        let w: usize = render_line(&self.doc().lines[i], self.raw_ctrl).iter().filter(|x| !x.1).map(|x| term::char_width(x.0)).sum();
        w.div_ceil(self.text_cols()).max(1)
    }

    fn page(&self) -> usize {
        self.rows.saturating_sub(1).max(1)
    }

    fn next_row(&self, r: Row) -> Option<Row> {
        if r.1 + 1 < self.line_rows(r.0) {
            Some((r.0, r.1 + 1))
        } else if r.0 + 1 < self.doc().lines.len() {
            Some((r.0 + 1, 0))
        } else {
            None
        }
    }

    fn prev_row(&self, r: Row) -> Option<Row> {
        if r.1 > 0 {
            Some((r.0, r.1 - 1))
        } else if r.0 > 0 {
            Some((r.0 - 1, self.line_rows(r.0 - 1) - 1))
        } else {
            None
        }
    }

    /// The top row that shows the last page.
    fn last_top(&self) -> Row {
        let n = self.doc().lines.len();
        if n == 0 {
            return (0, 0);
        }
        let mut r = (n - 1, self.line_rows(n - 1) - 1);
        for _ in 1..self.page() {
            match self.prev_row(r) {
                Some(p) => r = p,
                None => break,
            }
        }
        r
    }

    fn at_end(&self) -> bool {
        self.top >= self.last_top()
    }

    fn scroll(&mut self, n: isize) {
        for _ in 0..n.unsigned_abs() {
            let next = if n > 0 {
                if self.at_end() {
                    None
                } else {
                    self.next_row(self.top)
                }
            } else {
                self.prev_row(self.top)
            };
            match next {
                Some(r) => self.top = r,
                None => break,
            }
        }
    }

    fn draw(&mut self, out: &mut impl Write) -> io::Result<()> {
        let mut s = Screen::new();
        s.hide_cursor(true);
        s.goto(0, 0);
        let tc = self.text_cols();
        let mut row = self.top;
        let mut drawn = 0;
        let mut done = self.doc().lines.is_empty();
        while drawn < self.page() {
            s.goto(drawn, 0);
            s.clear_line();
            if !done {
                let line = &self.doc().lines[row.0];
                let cells = render_line(line, self.raw_ctrl);
                if self.numbers {
                    if row.1 == 0 {
                        s.push(&format!("{:>6}  ", row.0 + 1));
                    } else {
                        s.push("        ");
                    }
                }
                // The cells of this screen row: columns [start, start + width).
                let start_col = row.1 * tc;
                let marks = self.search.as_ref().map(|(re, _)| matches(re, line)).unwrap_or_default();
                let mut col = 0;
                let mut highlighted = false;
                for &(c, esc, at) in &cells {
                    if esc {
                        if col >= start_col && col < start_col + tc {
                            s.buf.push(c);
                        }
                        continue;
                    }
                    let w = term::char_width(c);
                    if col >= start_col + tc {
                        break;
                    }
                    if col >= start_col {
                        let hit = marks.iter().any(|&(a, b)| at >= a && at < b);
                        if hit != highlighted {
                            s.reverse(hit);
                            highlighted = hit;
                        }
                        s.buf.push(c);
                    }
                    col += w;
                }
                if highlighted || self.raw_ctrl {
                    s.reverse(false);
                }
                match self.next_row(row) {
                    Some(r) => row = r,
                    None => done = true,
                }
            } else {
                s.push("~");
            }
            drawn += 1;
        }
        // Status line.
        s.goto(self.rows - 1, 0);
        s.clear_line();
        s.reverse(true);
        let status = if let Some(m) = self.message.take() {
            m
        } else if self.at_end() {
            if self.docs.len() > 1 && self.cur + 1 < self.docs.len() {
                format!("(END) - Next: {}", self.docs[self.cur + 1].name)
            } else {
                "(END)".into()
            }
        } else {
            let total = self.doc().lines.len().max(1);
            let pct = (self.top.0 + self.page()).min(total) * 100 / total;
            format!("{} {pct}%", self.doc().name)
        };
        s.push(&status.chars().take(self.cols.saturating_sub(1)).collect::<String>());
        s.reverse(false);
        s.hide_cursor(false);
        s.flush(out)
    }

    fn find(&mut self, forward: bool) -> bool {
        let Some((re, dir)) = self.search.clone() else {
            self.message = Some("No previous search".into());
            return false;
        };
        let fwd = if forward { dir } else { !dir };
        let n = self.doc().lines.len();
        let mut i = self.top.0;
        for _ in 0..n {
            i = if fwd {
                if i + 1 >= n {
                    break;
                }
                i + 1
            } else {
                if i == 0 {
                    break;
                }
                i - 1
            };
            if re.is_match(self.doc().lines[i].as_bytes()) {
                self.top = (i, 0);
                if self.top > self.last_top() {
                    self.top = self.last_top();
                }
                return true;
            }
        }
        self.message = Some("Pattern not found".into());
        false
    }
}

fn matches(re: &Regex, line: &str) -> Vec<(usize, usize)> {
    let b = line.as_bytes();
    let mut v = Vec::new();
    let mut at = 0;
    while let Some((s, e)) = re.find(b, at) {
        if e > s {
            v.push((s, e));
            at = e;
        } else {
            at = s + 1;
        }
        if at > b.len() {
            break;
        }
    }
    v
}

fn prompt(tty: &mut fs::File, rows: usize, lead: &str) -> io::Result<Option<String>> {
    let mut text = String::new();
    loop {
        let mut s = Screen::new();
        s.goto(rows - 1, 0);
        s.clear_line();
        s.push(lead);
        s.push(&text);
        s.flush(tty)?;
        match term::read_key(tty.as_raw_fd(), None)? {
            Key::Enter => return Ok(Some(text)),
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('g') => return Ok(None),
            Key::Backspace => {
                if text.pop().is_none() {
                    return Ok(None);
                }
            }
            Key::Ctrl('u') => text.clear(),
            Key::Char(c) => text.push(c),
            Key::Eof => return Ok(None),
            _ => {}
        }
    }
}

fn load(name: &str) -> io::Result<Doc> {
    let mut data = Vec::new();
    if name == "-" {
        io::stdin().lock().read_to_end(&mut data)?;
    } else {
        let m = fs::metadata(name)?;
        if m.is_dir() {
            return Err(io::Error::new(io::ErrorKind::Other, "is a directory"));
        }
        data = fs::read(name)?;
    }
    let text = String::from_utf8_lossy(&data);
    let mut lines: Vec<String> = text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    Ok(Doc { name: if name == "-" { "(standard input)".into() } else { name.to_string() }, lines })
}

pub fn less(args: &[String]) -> i32 {
    let mut files = Vec::new();
    let (mut numbers, mut chop, mut raw_ctrl, mut quit_at_end, mut quit_if_one) = (false, false, false, false, false);
    let mut initial_search: Option<String> = None;
    for a in args {
        if let Some(p) = a.strip_prefix("+/") {
            initial_search = Some(p.to_string());
            continue;
        }
        match a.as_str() {
            "-N" | "--LINE-NUMBERS" => numbers = true,
            "-S" | "--chop-long-lines" => chop = true,
            "-R" | "-r" | "--RAW-CONTROL-CHARS" => raw_ctrl = true,
            "-E" | "-e" => quit_at_end = true,
            "-F" | "--quit-if-one-screen" => quit_if_one = true,
            "-X" | "-K" | "-i" | "-I" | "-M" | "-m" => {}
            s if s.starts_with('-') && s.len() > 1 => {
                for c in s[1..].chars() {
                    match c {
                        'N' => numbers = true,
                        'S' => chop = true,
                        'R' | 'r' => raw_ctrl = true,
                        'E' | 'e' => quit_at_end = true,
                        'F' => quit_if_one = true,
                        'X' | 'K' | 'i' | 'I' | 'M' | 'm' => {}
                        _ => {
                            error("less", format!("unknown option -{c}\nusage: less [-NSRFE] [+/PATTERN] [FILE...]"));
                            return 1;
                        }
                    }
                }
            }
            _ => files.push(a.clone()),
        }
    }
    if files.is_empty() {
        if term::is_tty(0) {
            error("less", "missing filename (\"less --help\" for help)");
            return 1;
        }
        files.push("-".into());
    }
    if files.iter().any(|f| f == "--help") {
        println!("{HELP}");
        return 0;
    }
    // Not a terminal: behave like cat.
    if !term::is_tty(1) {
        let mut out = io::stdout().lock();
        let mut status = 0;
        for f in &files {
            match load(f) {
                Ok(d) => {
                    for l in d.lines {
                        if writeln!(out, "{l}").is_err() {
                            return 0;
                        }
                    }
                }
                Err(e) => {
                    error("less", format!("{f}: {e}"));
                    status = 1;
                }
            }
        }
        return status;
    }
    let mut docs = Vec::new();
    for f in &files {
        match load(f) {
            Ok(d) => docs.push(d),
            Err(e) => error("less", format!("{f}: {e}")),
        }
    }
    if docs.is_empty() {
        return 1;
    }
    let mut tty = match fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
        Ok(t) => t,
        Err(e) => {
            error("less", format!("cannot open the terminal: {e}"));
            return 1;
        }
    };
    let (rows, cols) = term::size(tty.as_raw_fd());
    let mut p = Pager { docs, cur: 0, top: (0, 0), rows, cols, numbers, chop, raw_ctrl, search: None, message: None, quit_at_end };
    if quit_if_one && p.docs.len() == 1 && p.last_top() == (0, 0) {
        let mut out = io::stdout().lock();
        for l in &p.doc().lines {
            let _ = writeln!(out, "{l}");
        }
        return 0;
    }
    if let Some(pat) = initial_search {
        if let Ok(re) = Regex::new(&pat, Syntax::Extended, false) {
            p.search = Some((re, true));
            p.top = (0, 0);
            if !p.doc().lines.first().is_some_and(|l| p.search.as_ref().unwrap().0.is_match(l.as_bytes())) {
                p.find(true);
            }
        }
    }
    let raw = match Raw::enter(tty.as_raw_fd(), false) {
        Ok(r) => r,
        Err(e) => {
            error("less", format!("cannot set up the terminal: {e}"));
            return 1;
        }
    };
    term::watch_resize();
    let _ = term::alt_screen(&mut tty, true);
    let r = run(&mut p, &mut tty);
    let _ = term::alt_screen(&mut tty, false);
    drop(raw);
    match r {
        Ok(()) => 0,
        Err(e) => {
            error("less", e);
            1
        }
    }
}

fn run(p: &mut Pager, tty: &mut fs::File) -> io::Result<()> {
    let mut count: Option<usize> = None;
    let mut showing_help = false;
    let mut saved: Option<(Vec<Doc>, usize, Row)> = None;
    loop {
        if term::RESIZED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let (r, c) = term::size(tty.as_raw_fd());
            p.rows = r;
            p.cols = c;
            if p.top > p.last_top() {
                p.top = p.last_top();
            }
        }
        p.draw(tty)?;
        let key = term::read_key(tty.as_raw_fd(), None)?;
        let n = count.take().unwrap_or(1);
        let page = p.page() as isize;
        match key {
            Key::Char(c @ '0'..='9') => {
                count = Some(n.saturating_mul(if count.is_some() { 10 } else { 0 }) + (c as usize - '0' as usize));
                if count == Some(0) {
                    count = None;
                }
                continue;
            }
            Key::Char('q') | Key::Char('Q') | Key::Ctrl('c') | Key::Eof => {
                if showing_help {
                    let (d, cur, top) = saved.take().unwrap();
                    p.docs = d;
                    p.cur = cur;
                    p.top = top;
                    showing_help = false;
                    continue;
                }
                return Ok(());
            }
            Key::Char(' ') | Key::Char('f') | Key::Ctrl('f') | Key::Ctrl('v') | Key::PageDown => {
                if p.at_end() {
                    if p.quit_at_end {
                        return Ok(());
                    }
                    if p.cur + 1 < p.docs.len() {
                        p.cur += 1;
                        p.top = (0, 0);
                        continue;
                    }
                }
                p.scroll(page * n as isize);
            }
            Key::Char('b') | Key::Ctrl('b') | Key::Alt('v') | Key::PageUp => p.scroll(-page * n as isize),
            Key::Char('j') | Key::Char('e') | Key::Enter | Key::Ctrl('n') | Key::Ctrl('e') | Key::Down => p.scroll(n as isize),
            Key::Char('k') | Key::Char('y') | Key::Ctrl('p') | Key::Ctrl('y') | Key::Ctrl('k') | Key::Up => p.scroll(-(n as isize)),
            Key::Char('d') | Key::Ctrl('d') => p.scroll(page / 2 * n as isize),
            Key::Char('u') | Key::Ctrl('u') => p.scroll(-(page / 2) * n as isize),
            Key::Char('g') | Key::Char('<') | Key::Home => {
                p.top = (0, 0);
                if count.is_none() && n > 1 {
                    p.top = ((n - 1).min(p.doc().lines.len().saturating_sub(1)), 0);
                }
            }
            Key::Char('G') | Key::Char('>') | Key::End => p.top = p.last_top(),
            Key::Char('r') | Key::Ctrl('l') | Key::Char('R') => {
                tty.write_all(b"\x1b[2J")?;
            }
            Key::Char('h') | Key::Char('H') => {
                if !showing_help {
                    let docs = std::mem::take(&mut p.docs);
                    saved = Some((docs, p.cur, p.top));
                    p.docs = vec![Doc { name: "HELP -- press q when done".into(), lines: HELP.lines().map(String::from).collect() }];
                    p.cur = 0;
                    p.top = (0, 0);
                    showing_help = true;
                }
            }
            Key::Char('/') | Key::Char('?') => {
                let fwd = key == Key::Char('/');
                if let Some(pat) = prompt(tty, p.rows, if fwd { "/" } else { "?" })? {
                    if !pat.is_empty() {
                        let icase = !pat.chars().any(char::is_uppercase);
                        match Regex::new(&pat, Syntax::Extended, icase) {
                            Ok(re) => p.search = Some((re, fwd)),
                            Err(e) => {
                                p.message = Some(format!("Invalid pattern: {e}"));
                                continue;
                            }
                        }
                    }
                    for _ in 0..n {
                        if !p.find(true) {
                            break;
                        }
                    }
                }
            }
            Key::Char('n') => {
                for _ in 0..n {
                    if !p.find(true) {
                        break;
                    }
                }
            }
            Key::Char('N') => {
                for _ in 0..n {
                    if !p.find(false) {
                        break;
                    }
                }
            }
            Key::Char(':') => {
                if let Some(cmd) = prompt(tty, p.rows, ":")? {
                    match cmd.as_str() {
                        "n" if p.cur + 1 < p.docs.len() => {
                            p.cur += 1;
                            p.top = (0, 0);
                        }
                        "p" if p.cur > 0 => {
                            p.cur -= 1;
                            p.top = (0, 0);
                        }
                        "q" => return Ok(()),
                        "n" | "p" => p.message = Some("No more files".into()),
                        _ => p.message = Some(format!("Unknown command :{cmd}")),
                    }
                }
            }
            Key::Char('-') => {
                if let Key::Char(c) = term::read_key(tty.as_raw_fd(), None)? {
                    match c {
                        'N' => {
                            p.numbers = !p.numbers;
                            p.message = Some(format!("Line numbers {}", if p.numbers { "on" } else { "off" }));
                        }
                        'S' => {
                            p.chop = !p.chop;
                            p.top = (p.top.0, 0);
                            p.message = Some(format!("Chop long lines {}", if p.chop { "on" } else { "off" }));
                        }
                        _ => p.message = Some(format!("No option -{c} to toggle")),
                    }
                }
            }
            Key::Char('=') | Key::Ctrl('g') => {
                let total = p.doc().lines.len();
                p.message = Some(format!("{} lines {}-{}/{total}", p.doc().name, p.top.0 + 1, (p.top.0 + p.page()).min(total)));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering() {
        let r: String = render_line("a\tb\x01", false).iter().map(|x| x.0).collect();
        assert_eq!(render_line("a\tb", false)[8].2, 2, "cells remember their source byte");
        assert_eq!(r, "a       b^A");
        let r: Vec<bool> = render_line("\x1b[1mX", true).iter().map(|x| x.1).collect();
        assert_eq!(r, [true, true, true, true, false]);
    }

    #[test]
    fn wrapping_and_scrolling() {
        let lines: Vec<String> = (0..10).map(|i| if i == 3 { "x".repeat(25) } else { format!("line {i}") }).collect();
        let mut p = Pager { docs: vec![Doc { name: "t".into(), lines }], cur: 0, top: (0, 0), rows: 5, cols: 10, numbers: false, chop: false, raw_ctrl: false, search: None, message: None, quit_at_end: false };
        assert_eq!(p.line_rows(3), 3);
        p.scroll(4);
        assert_eq!(p.top, (3, 1));
        p.scroll(100);
        assert_eq!(p.top, p.last_top());
        assert_eq!(p.top, (6, 0));
        p.scroll(-100);
        assert_eq!(p.top, (0, 0));
        p.search = Some((Regex::new("line 8", Syntax::Extended, false).unwrap(), true));
        assert!(p.find(true));
        assert_eq!(p.top, (6, 0), "clamped to the last page");
        p.chop = true;
        assert_eq!(p.line_rows(3), 1);
    }
}
