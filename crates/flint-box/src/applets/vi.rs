//! vi: a screen editor with the classic commands.
//!
//! Normal mode: counts, motions (h j k l w b e W B E 0 ^ $ gg G H M L f F t T ; , % { }
//! n N * #), operators (d c y > <) with any motion or doubled (dd, cc, yy, >>, <<), and
//! x X D C s S p P J r R ~ u ^R . i a I A o O ZZ ZQ ^G ^F ^B ^D ^U.
//! Ex commands: :w [file] :q :q! :wq :x :e[!] file :N :$ :[range]s/re/repl/[g] :[range]d
//! :set nu|nonu|ic|noic :r file. Ranges: N, $, ., %, N,M.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

use flint_core::term::{self, Key, Raw, Screen};

use crate::regex::{self, Regex, Syntax, Which};
use crate::util::error;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Normal,
    Insert,
    Replace,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
struct Pos {
    r: usize,
    c: usize,
}

#[derive(Clone)]
struct Register {
    text: Vec<String>,
    linewise: bool,
}

struct Editor {
    lines: Vec<String>,
    cur: Pos,
    want_col: usize,
    top: usize,
    rows: usize,
    cols: usize,
    mode: Mode,
    file: Option<String>,
    dirty: bool,
    message: String,
    reg: Option<Register>,
    undo: Vec<(Vec<String>, Pos)>,
    redo: Vec<(Vec<String>, Pos)>,
    search: Option<(String, bool)>,
    last_find: Option<(char, char)>, // (command f/F/t/T, char)
    pending: VecDeque<Key>,
    recording: Option<Vec<Key>>,
    last_change: Vec<Key>,
    numbers: bool,
    icase: bool,
    quit: bool,
    insert_start: Option<Pos>,
    /// A count was typed before the command (G and gg use it as a line number).
    count_given: bool,
}

fn clen(s: &str) -> usize {
    s.chars().count()
}

fn byte_at(s: &str, c: usize) -> usize {
    s.char_indices().nth(c).map_or(s.len(), |(b, _)| b)
}

fn char_at(s: &str, c: usize) -> Option<char> {
    s.chars().nth(c)
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// What a motion covers: target position, whether whole lines, whether the target char is included.
struct Motion {
    to: Pos,
    linewise: bool,
    inclusive: bool,
}

impl Editor {
    fn line(&self) -> &str {
        &self.lines[self.cur.r]
    }

    fn line_len(&self, r: usize) -> usize {
        clen(&self.lines[r])
    }

    fn clamp(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cur.r = self.cur.r.min(self.lines.len() - 1);
        let len = self.line_len(self.cur.r);
        let max = if self.mode == Mode::Normal { len.saturating_sub(1) } else { len };
        self.cur.c = self.cur.c.min(max);
    }

    fn first_nonblank(&self, r: usize) -> usize {
        self.lines[r].chars().take_while(|c| *c == ' ' || *c == '\t').count()
    }

    fn snapshot(&mut self) {
        self.undo.push((self.lines.clone(), self.cur));
        if self.undo.len() > 500 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn next_key(&mut self, tty: &fs::File) -> io::Result<Key> {
        let k = match self.pending.pop_front() {
            Some(k) => k,
            None => loop {
                let k = term::read_key(tty.as_raw_fd(), None)?;
                if k != Key::None {
                    break k;
                }
                if term::RESIZED.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let (r, c) = term::size(tty.as_raw_fd());
                    self.rows = r;
                    self.cols = c;
                    return Ok(Key::Ctrl('l'));
                }
            },
        };
        if let Some(rec) = self.recording.as_mut() {
            rec.push(k);
        }
        Ok(k)
    }

    // ---- display

    fn text_cols(&self) -> usize {
        if self.numbers {
            self.cols.saturating_sub(8).max(8)
        } else {
            self.cols.max(8)
        }
    }

    fn display_width(&self, s: &str) -> usize {
        let mut w = 0;
        for c in s.chars() {
            w += if c == '\t' { 8 - w % 8 } else { cell_width(c) };
        }
        w
    }

    fn rows_of(&self, r: usize) -> usize {
        self.display_width(&self.lines[r]).div_ceil(self.text_cols()).max(1)
    }

    fn scroll_to_cursor(&mut self) {
        let text_rows = self.rows.saturating_sub(1).max(1);
        if self.cur.r < self.top {
            self.top = self.cur.r;
        }
        // Rows from top through the cursor's row must fit.
        loop {
            let prefix: String = self.lines[self.cur.r].chars().take(self.cur.c).collect();
            let cursor_row_in_line = self.display_width(&prefix) / self.text_cols();
            let mut used = cursor_row_in_line + 1;
            for r in self.top..self.cur.r {
                used += self.rows_of(r);
            }
            if used <= text_rows || self.top >= self.cur.r {
                break;
            }
            self.top += 1;
        }
    }

    fn draw(&mut self, tty: &mut fs::File, cmdline: Option<&str>) -> io::Result<()> {
        self.scroll_to_cursor();
        let tc = self.text_cols();
        let text_rows = self.rows.saturating_sub(1).max(1);
        let mut s = Screen::new();
        s.hide_cursor(true);
        let mut row = 0;
        let mut r = self.top;
        let mut cursor_screen = (0, 0);
        while row < text_rows {
            if r >= self.lines.len() {
                s.goto(row, 0);
                s.clear_line();
                s.push("~");
                row += 1;
                continue;
            }
            let line = &self.lines[r];
            let mut col = 0usize;
            let mut screen_row = row;
            s.goto(screen_row, 0);
            s.clear_line();
            if self.numbers {
                s.push(&format!("{:>6}  ", r + 1));
            }
            let base = if self.numbers { 8 } else { 0 };
            for (ci, ch) in line.chars().enumerate() {
                if r == self.cur.r && ci == self.cur.c {
                    cursor_screen = (screen_row, base + col % tc);
                }
                let w = if ch == '\t' { 8 - col % 8 } else { cell_width(ch) };
                if col / tc != (col + w.max(1) - 1) / tc && w > 1 && ch != '\t' {
                    col = (col / tc + 1) * tc; // a wide char never straddles rows
                }
                if col > 0 && col % tc == 0 && col / tc > (screen_row - row) {
                    screen_row += 1;
                    if screen_row >= text_rows {
                        break;
                    }
                    s.goto(screen_row, 0);
                    s.clear_line();
                    if self.numbers {
                        s.push("        ");
                    }
                }
                if ch == '\t' {
                    for _ in 0..w {
                        s.buf.push(' ');
                    }
                } else if (ch as u32) < 0x20 || ch == '\x7f' {
                    s.buf.push('?');
                } else {
                    s.buf.push(ch);
                }
                col += w;
            }
            if r == self.cur.r && self.cur.c >= clen(line) {
                cursor_screen = (screen_row.min(text_rows - 1), base + (col % tc));
                if col > 0 && col % tc == 0 && self.mode != Mode::Normal {
                    cursor_screen = ((screen_row + 1).min(text_rows - 1), base);
                }
            }
            row = screen_row + 1;
            r += 1;
        }
        // Status line.
        s.goto(self.rows - 1, 0);
        s.clear_line();
        match cmdline {
            Some(c) => s.push(c),
            None => {
                let left = if !self.message.is_empty() {
                    self.message.clone()
                } else {
                    match self.mode {
                        Mode::Insert => "-- INSERT --".into(),
                        Mode::Replace => "-- REPLACE --".into(),
                        Mode::Normal => String::new(),
                    }
                };
                let right = format!("{},{}", self.cur.r + 1, self.cur.c + 1);
                let room = self.cols.saturating_sub(right.len() + 2);
                let left: String = left.chars().take(room).collect();
                s.push(&left);
                s.goto(self.rows - 1, self.cols.saturating_sub(right.len() + 1));
                s.push(&right);
            }
        }
        match cmdline {
            Some(c) => s.goto(self.rows - 1, term::str_width(c)),
            None => s.goto(cursor_screen.0, cursor_screen.1),
        }
        s.hide_cursor(false);
        s.flush(tty)
    }

    // ---- motions

    fn word_fwd(&self, mut p: Pos, big: bool, end: bool) -> Pos {
        let at = |p: Pos| -> Option<char> { char_at(&self.lines[p.r], p.c) };
        let adv = |p: Pos| -> Option<Pos> {
            if p.c + 1 < self.line_len(p.r) {
                Some(Pos { r: p.r, c: p.c + 1 })
            } else if p.r + 1 < self.lines.len() {
                Some(Pos { r: p.r + 1, c: 0 })
            } else {
                None
            }
        };
        if end {
            // to the end of this or the next word
            let Some(mut q) = adv(p) else { return p };
            while at(q).map_or(true, |c| c.is_whitespace()) {
                match adv(q) {
                    Some(n) => q = n,
                    None => return q,
                }
            }
            let k = class(at(q).unwrap(), big);
            loop {
                match adv(q) {
                    Some(n) if n.r == q.r && at(n).is_some_and(|c| class(c, big) == k) => q = n,
                    _ => return q,
                }
            }
        }
        let start_class = at(p).map(|c| class(c, big));
        let start_row = p.r;
        // skip the current word
        if let Some(k) = start_class {
            if k != Class::Space {
                loop {
                    match adv(p) {
                        Some(n) if n.r == p.r && at(n).is_some_and(|c| class(c, big) == k) => p = n,
                        Some(n) => {
                            p = n;
                            break;
                        }
                        None => return Pos { r: p.r, c: self.line_len(p.r) },
                    }
                }
            }
        }
        // skip blanks (an empty line stops it)
        loop {
            if p.r != start_row && self.lines[p.r].is_empty() {
                return p;
            }
            match at(p) {
                Some(c) if c.is_whitespace() => {}
                Some(_) => return p,
                None => {}
            }
            match adv(p) {
                Some(n) => p = n,
                None => return Pos { r: p.r, c: self.line_len(p.r) },
            }
        }
    }

    fn word_back(&self, mut p: Pos, big: bool) -> Pos {
        let at = |p: Pos| -> Option<char> { char_at(&self.lines[p.r], p.c) };
        let back = |p: Pos| -> Option<Pos> {
            if p.c > 0 {
                Some(Pos { r: p.r, c: p.c - 1 })
            } else if p.r > 0 {
                let l = self.line_len(p.r - 1);
                Some(Pos { r: p.r - 1, c: l.saturating_sub(1) })
            } else {
                None
            }
        };
        let Some(mut q) = back(p) else { return p };
        while at(q).map_or(!self.lines[q.r].is_empty(), |c| c.is_whitespace()) {
            match back(q) {
                Some(n) => q = n,
                None => return q,
            }
        }
        if self.lines[q.r].is_empty() {
            return q;
        }
        let k = class(at(q).unwrap(), big);
        p = q;
        while let Some(n) = back(p) {
            if n.r == p.r && at(n).is_some_and(|c| class(c, big) == k) {
                p = n;
            } else {
                break;
            }
        }
        p
    }

    fn find_char(&self, cmd: char, ch: char, count: usize) -> Option<Pos> {
        let line: Vec<char> = self.line().chars().collect();
        let mut c = self.cur.c;
        for k in 0..count {
            match cmd {
                'f' | 't' => {
                    let skip = if cmd == 't' && k == 0 { 2 } else { 1 };
                    let from = c + skip.min(1);
                    let start = if cmd == 't' && k == 0 && line.get(c + 1) == Some(&ch) { c + 2 } else { from };
                    let i = (start..line.len()).find(|&i| line[i] == ch)?;
                    c = i;
                }
                _ => {
                    let end = if cmd == 'T' && k == 0 && c >= 1 && line.get(c - 1) == Some(&ch) { c.saturating_sub(1) } else { c };
                    let i = (0..end).rev().find(|&i| line[i] == ch)?;
                    c = i;
                }
            }
        }
        let c = match cmd {
            't' => c - 1,
            'T' => c + 1,
            _ => c,
        };
        Some(Pos { r: self.cur.r, c })
    }

    fn match_pair(&self) -> Option<Pos> {
        let line: Vec<char> = self.line().chars().collect();
        let pairs = [('(', ')'), ('[', ']'), ('{', '}')];
        let start = (self.cur.c..line.len()).find(|&i| pairs.iter().any(|(a, b)| line[i] == *a || line[i] == *b))?;
        let ch = line[start];
        let (open, close, fwd) = pairs.iter().find_map(|&(a, b)| if ch == a { Some((a, b, true)) } else if ch == b { Some((a, b, false)) } else { None })?;
        let mut depth = 0i32;
        let mut p = Pos { r: self.cur.r, c: start };
        loop {
            let c = char_at(&self.lines[p.r], p.c);
            if c == Some(open) {
                depth += if fwd { 1 } else { -1 };
            } else if c == Some(close) {
                depth += if fwd { -1 } else { 1 };
            }
            if depth == 0 {
                return Some(p);
            }
            if fwd {
                if p.c + 1 < self.line_len(p.r) {
                    p.c += 1;
                } else if p.r + 1 < self.lines.len() {
                    p = Pos { r: p.r + 1, c: 0 };
                } else {
                    return None;
                }
            } else if p.c > 0 {
                p.c -= 1;
            } else if p.r > 0 {
                p.r -= 1;
                p.c = self.line_len(p.r);
                if p.c == 0 {
                    continue;
                }
                p.c -= 1;
            } else {
                return None;
            }
        }
    }

    fn regex(&self, pat: &str) -> Result<Regex, String> {
        let icase = self.icase && !pat.chars().any(char::is_uppercase);
        Regex::new(pat, Syntax::Basic, icase).map_err(|e| e.0)
    }

    fn search_from(&mut self, from: Pos, forward: bool) -> Option<Pos> {
        let (pat, dir) = self.search.clone()?;
        let fwd = forward == dir;
        let re = match self.regex(&pat) {
            Ok(r) => r,
            Err(e) => {
                self.message = format!("bad pattern: {e}");
                return None;
            }
        };
        let n = self.lines.len();
        for k in 0..=n {
            let r = if fwd { (from.r + k) % n } else { (from.r + n * 2 - k) % n };
            let line = &self.lines[r];
            let b = line.as_bytes();
            let mut hits = Vec::new();
            let mut at = 0;
            while let Some((s, e)) = re.find(b, at) {
                hits.push(clen(&line[..s]));
                at = if e > s { e } else { s + 1 };
                if at > b.len() {
                    break;
                }
            }
            let pick = if fwd {
                hits.into_iter().find(|&c| k > 0 || c > from.c)
            } else {
                hits.into_iter().rev().find(|&c| k > 0 || c < from.c)
            };
            if let Some(c) = pick {
                if (fwd && (r < from.r || (r == from.r && k > 0))) || (!fwd && (r > from.r || (r == from.r && k > 0))) {
                    self.message = if fwd { "search hit BOTTOM, continuing at TOP".into() } else { "search hit TOP, continuing at BOTTOM".into() };
                }
                return Some(Pos { r, c });
            }
        }
        self.message = format!("Pattern not found: {pat}");
        None
    }

    /// Read a motion key (after an operator or alone). None if the key is not a motion.
    fn motion(&mut self, key: Key, count: usize, tty: &fs::File, op: Option<char>) -> io::Result<Option<Motion>> {
        let cur = self.cur;
        let ch = |p: Pos| Motion { to: p, linewise: false, inclusive: false };
        let line_to = |r: usize| Motion { to: Pos { r, c: 0 }, linewise: true, inclusive: false };
        let last = self.lines.len() - 1;
        Ok(Some(match key {
            Key::Char('h') | Key::Left | Key::Backspace | Key::Ctrl('h') => ch(Pos { r: cur.r, c: cur.c.saturating_sub(count) }),
            Key::Char('l') | Key::Right | Key::Char(' ') => {
                let len = self.line_len(cur.r);
                let max = if op.is_some() { len } else { len.saturating_sub(1) };
                ch(Pos { r: cur.r, c: (cur.c + count).min(max) })
            }
            Key::Char('j') | Key::Down | Key::Ctrl('n') | Key::Ctrl('j') => {
                let mut m = line_to((cur.r + count).min(last));
                m.to.c = self.want_col;
                m
            }
            Key::Char('k') | Key::Up | Key::Ctrl('p') => {
                let mut m = line_to(cur.r.saturating_sub(count));
                m.to.c = self.want_col;
                m
            }
            Key::Char('+') | Key::Enter => {
                let r = (cur.r + count).min(last);
                Motion { to: Pos { r, c: self.first_nonblank(r) }, linewise: true, inclusive: false }
            }
            Key::Char('-') => {
                let r = cur.r.saturating_sub(count);
                Motion { to: Pos { r, c: self.first_nonblank(r) }, linewise: true, inclusive: false }
            }
            Key::Char('0') | Key::Home => ch(Pos { r: cur.r, c: 0 }),
            Key::Char('^') => ch(Pos { r: cur.r, c: self.first_nonblank(cur.r) }),
            Key::Char('$') | Key::End => {
                let r = (cur.r + count - 1).min(last);
                Motion { to: Pos { r, c: self.line_len(r).saturating_sub(1) }, linewise: false, inclusive: true }
            }
            Key::Char('|') => ch(Pos { r: cur.r, c: (count - 1).min(self.line_len(cur.r).saturating_sub(1)) }),
            Key::Char(c @ ('w' | 'W')) => {
                let mut p = cur;
                for _ in 0..count {
                    p = self.word_fwd(p, c == 'W', false);
                }
                // cw acts like ce; dw stops at the end of the line
                if op == Some('c') && char_at(self.line(), cur.c).is_some_and(|x| !x.is_whitespace()) {
                    let mut q = Pos { r: cur.r, c: cur.c.saturating_sub(1) };
                    if cur.c == 0 {
                        q = cur;
                        let e = self.word_fwd_end_from(q, c == 'W', count);
                        return Ok(Some(Motion { to: e, linewise: false, inclusive: true }));
                    }
                    for _ in 0..count {
                        q = self.word_fwd(q, c == 'W', true);
                    }
                    return Ok(Some(Motion { to: q, linewise: false, inclusive: true }));
                }
                if op.is_some() && p.r > cur.r {
                    p = Pos { r: cur.r, c: self.line_len(cur.r) };
                }
                ch(p)
            }
            Key::Char(c @ ('e' | 'E')) => {
                let mut p = cur;
                for _ in 0..count {
                    p = self.word_fwd(p, c == 'E', true);
                }
                Motion { to: p, linewise: false, inclusive: true }
            }
            Key::Char('b' | 'B') | Key::CtrlLeft => {
                let big = matches!(key, Key::Char('B'));
                let mut p = cur;
                for _ in 0..count {
                    p = self.word_back(p, big);
                }
                ch(p)
            }
            Key::Char('G') => {
                let r = if self.count_given { (count - 1).min(last) } else { last };
                Motion { to: Pos { r, c: self.first_nonblank(r) }, linewise: true, inclusive: false }
            }
            Key::Char('g') => match self.next_key(tty)? {
                Key::Char('g') => {
                    let r = if self.count_given { (count - 1).min(last) } else { 0 };
                    Motion { to: Pos { r, c: self.first_nonblank(r) }, linewise: true, inclusive: false }
                }
                Key::Char('_') => Motion { to: Pos { r: cur.r, c: self.line_len(cur.r).saturating_sub(1) }, linewise: false, inclusive: true },
                _ => return Ok(None),
            },
            Key::Char('H') | Key::Char('M') | Key::Char('L') => {
                let text_rows = self.rows.saturating_sub(1);
                let bottom = (self.top + text_rows).min(self.lines.len()) - 1;
                let r = match key {
                    Key::Char('H') => (self.top + count - 1).min(bottom),
                    Key::Char('L') => bottom.saturating_sub(count - 1).max(self.top),
                    _ => (self.top + bottom) / 2,
                };
                Motion { to: Pos { r, c: self.first_nonblank(r) }, linewise: true, inclusive: false }
            }
            Key::Char(f @ ('f' | 'F' | 't' | 'T')) => {
                let Key::Char(c) = self.next_key(tty)? else { return Ok(None) };
                self.last_find = Some((f, c));
                match self.find_char(f, c, count) {
                    Some(p) => Motion { to: p, linewise: false, inclusive: f == 'f' || f == 't' },
                    None => return Ok(None),
                }
            }
            Key::Char(k @ (';' | ',')) => {
                let Some((f, c)) = self.last_find else { return Ok(None) };
                let f = if k == ',' {
                    match f {
                        'f' => 'F',
                        'F' => 'f',
                        't' => 'T',
                        _ => 't',
                    }
                } else {
                    f
                };
                match self.find_char(f, c, count) {
                    Some(p) => Motion { to: p, linewise: false, inclusive: f == 'f' || f == 't' },
                    None => return Ok(None),
                }
            }
            Key::Char('%') => match self.match_pair() {
                Some(p) => Motion { to: p, linewise: false, inclusive: true },
                None => return Ok(None),
            },
            Key::Char('}') | Key::Char('{') => {
                let fwd = key == Key::Char('}');
                let mut r = cur.r;
                for _ in 0..count {
                    // skip blank lines, then to the next blank line
                    loop {
                        if fwd {
                            if r >= last {
                                break;
                            }
                            r += 1;
                        } else {
                            if r == 0 {
                                break;
                            }
                            r -= 1;
                        }
                        if self.lines[r].trim().is_empty() && !self.lines[if fwd { r - 1 } else { r + 1 }].trim().is_empty() {
                            break;
                        }
                    }
                }
                ch(Pos { r, c: if fwd && r == last { self.line_len(r).saturating_sub(1) } else { 0 } })
            }
            Key::Char(k @ ('n' | 'N')) => {
                let mut p = cur;
                for _ in 0..count {
                    match self.search_from(p, k == 'n') {
                        Some(q) => p = q,
                        None => return Ok(None),
                    }
                }
                ch(p)
            }
            Key::Char(k @ ('*' | '#')) => {
                let line: Vec<char> = self.line().chars().collect();
                let is_w = |c: char| c.is_alphanumeric() || c == '_';
                let mut s = cur.c;
                while s < line.len() && !is_w(line[s]) {
                    s += 1;
                }
                if s >= line.len() {
                    return Ok(None);
                }
                let mut a = s;
                while a > 0 && is_w(line[a - 1]) {
                    a -= 1;
                }
                let mut b = s;
                while b < line.len() && is_w(line[b]) {
                    b += 1;
                }
                let word: String = line[a..b].iter().collect();
                self.search = Some((format!("\\<{word}\\>"), k == '*'));
                match self.search_from(Pos { r: cur.r, c: a }, true) {
                    Some(p) => ch(p),
                    None => return Ok(None),
                }
            }
            Key::Char(k @ ('/' | '?')) => {
                let Some(pat) = self.read_cmdline(tty, if k == '/' { "/" } else { "?" })? else { return Ok(None) };
                if !pat.is_empty() {
                    self.search = Some((pat, k == '/'));
                } else if let Some((p, _)) = self.search.clone() {
                    self.search = Some((p, k == '/'));
                }
                match self.search_from(cur, true) {
                    Some(p) => ch(p),
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        }))
    }

    fn word_fwd_end_from(&self, p: Pos, big: bool, count: usize) -> Pos {
        // `cw` at column 0 on a word: the end of the count-th word starting here
        let line: Vec<char> = self.line().chars().collect();
        let k = class(line[p.c], big);
        let mut c = p.c;
        while c + 1 < line.len() && class(line[c + 1], big) == k {
            c += 1;
        }
        let mut q = Pos { r: p.r, c };
        for _ in 1..count {
            q = self.word_fwd(q, big, true);
        }
        q
    }

    // ---- editing primitives

    /// Text between two positions (charwise, end exclusive), possibly across lines.
    fn take_range(&mut self, a: Pos, b: Pos, delete: bool) -> Register {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let mut text = Vec::new();
        if a.r == b.r {
            let l = &self.lines[a.r];
            let (s, e) = (byte_at(l, a.c), byte_at(l, b.c));
            text.push(l[s..e].to_string());
            if delete {
                let mut l = self.lines[a.r].clone();
                l.replace_range(s..e, "");
                self.lines[a.r] = l;
            }
        } else {
            let first = &self.lines[a.r];
            text.push(first[byte_at(first, a.c)..].to_string());
            for r in a.r + 1..b.r {
                text.push(self.lines[r].clone());
            }
            let last = &self.lines[b.r];
            text.push(last[..byte_at(last, b.c)].to_string());
            if delete {
                let head = self.lines[a.r][..byte_at(&self.lines[a.r], a.c)].to_string();
                let tail = self.lines[b.r][byte_at(&self.lines[b.r], b.c)..].to_string();
                self.lines.splice(a.r..=b.r, [head + &tail]);
            }
        }
        Register { text, linewise: false }
    }

    fn take_lines(&mut self, a: usize, b: usize, delete: bool) -> Register {
        let (a, b) = (a.min(b), a.max(b).min(self.lines.len() - 1));
        let text: Vec<String> = self.lines[a..=b].to_vec();
        if delete {
            self.lines.drain(a..=b);
            if self.lines.is_empty() {
                self.lines.push(String::new());
            }
        }
        Register { text, linewise: true }
    }

    fn put(&mut self, after: bool, count: usize) {
        let Some(reg) = self.reg.clone() else {
            self.message = "Nothing in register".into();
            return;
        };
        self.snapshot();
        if reg.linewise {
            let at = if after { self.cur.r + 1 } else { self.cur.r };
            let mut block = Vec::new();
            for _ in 0..count {
                block.extend(reg.text.iter().cloned());
            }
            self.lines.splice(at..at, block);
            self.cur = Pos { r: at, c: self.first_nonblank(at) };
        } else {
            let mut text = reg.text.clone();
            if count > 1 {
                let joined = text.join("\n").repeat(count);
                text = joined.split('\n').map(String::from).collect();
            }
            let line = self.lines[self.cur.r].clone();
            let c = if after && !line.is_empty() { self.cur.c + 1 } else { self.cur.c };
            let b = byte_at(&line, c);
            let (head, tail) = line.split_at(b);
            if text.len() == 1 {
                self.lines[self.cur.r] = format!("{head}{}{tail}", text[0]);
                self.cur.c = c + clen(&text[0]) - usize::from(!text[0].is_empty());
            } else {
                let n = text.len();
                let mut new = vec![format!("{head}{}", text[0])];
                new.extend(text[1..n - 1].iter().cloned());
                new.push(format!("{}{tail}", text[n - 1]));
                self.lines.splice(self.cur.r..=self.cur.r, new);
                self.cur.c = c;
            }
        }
    }

    fn apply_operator(&mut self, op: char, m: Motion) -> bool {
        let cur = self.cur;
        let reg = if m.linewise {
            match op {
                'y' => {
                    let r = self.take_lines(cur.r, m.to.r, false);
                    self.cur.r = cur.r.min(m.to.r);
                    r
                }
                '>' | '<' => {
                    self.snapshot();
                    for r in cur.r.min(m.to.r)..=cur.r.max(m.to.r) {
                        let l = &self.lines[r];
                        self.lines[r] = if op == '>' {
                            if l.is_empty() {
                                String::new()
                            } else {
                                format!("\t{l}")
                            }
                        } else if let Some(s) = l.strip_prefix('\t') {
                            s.to_string()
                        } else {
                            let n = l.chars().take(8).take_while(|c| *c == ' ').count();
                            l[n..].to_string()
                        };
                    }
                    self.cur = Pos { r: cur.r.min(m.to.r), c: self.first_nonblank(cur.r.min(m.to.r)) };
                    return false;
                }
                _ => {
                    self.snapshot();
                    let first = cur.r.min(m.to.r);
                    let r = self.take_lines(cur.r, m.to.r, true);
                    if op == 'c' {
                        let indent: String = r.text[0].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                        self.lines.insert(first.min(self.lines.len()), indent.clone());
                        if self.lines.len() > 1 && self.lines.last().is_some_and(String::is_empty) && first + 1 < self.lines.len() && r.text.len() >= self.lines.len() {
                            self.lines.pop();
                        }
                        self.cur = Pos { r: first, c: clen(&indent) };
                    } else {
                        self.cur.r = first.min(self.lines.len() - 1);
                        self.cur.c = self.first_nonblank(self.cur.r);
                    }
                    r
                }
            }
        } else {
            let (a, mut b) = if m.to < cur { (m.to, cur) } else { (cur, m.to) };
            if m.inclusive {
                b.c += 1;
            }
            let delete = op != 'y';
            if delete {
                self.snapshot();
            }
            let r = self.take_range(a, b, delete);
            self.cur = a;
            r
        };
        self.reg = Some(reg);
        op == 'c'
    }

    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cur.r];
        let b = byte_at(line, self.cur.c);
        if self.mode == Mode::Replace && self.cur.c < clen(line) {
            let e = byte_at(line, self.cur.c + 1);
            line.replace_range(b..e, &c.to_string());
        } else {
            line.insert(b, c);
        }
        self.cur.c += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
        let line = self.lines[self.cur.r].clone();
        let b = byte_at(&line, self.cur.c);
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let rest = line[b..].trim_start_matches([' ', '\t']).to_string();
        self.lines[self.cur.r] = line[..b].trim_end_matches([' ', '\t']).to_string();
        if self.lines[self.cur.r].is_empty() && !line[..b].is_empty() {
            // a line of only indentation keeps nothing
        }
        self.lines.insert(self.cur.r + 1, format!("{indent}{rest}"));
        self.cur = Pos { r: self.cur.r + 1, c: clen(&indent) };
        self.dirty = true;
    }

    // ---- command line

    fn read_cmdline(&mut self, tty: &fs::File, lead: &str) -> io::Result<Option<String>> {
        let mut text = String::new();
        let mut t = tty.try_clone()?;
        loop {
            self.draw(&mut t, Some(&format!("{lead}{text}")))?;
            match self.next_key(tty)? {
                Key::Enter => return Ok(Some(text)),
                Key::Esc | Key::Ctrl('c') => return Ok(None),
                Key::Backspace => {
                    if text.pop().is_none() {
                        return Ok(None);
                    }
                }
                Key::Ctrl('u') => text.clear(),
                Key::Ctrl('w') => {
                    let t = text.trim_end().to_string();
                    let k = t.rfind(|c: char| c.is_whitespace()).map_or(0, |k| k + 1);
                    text.truncate(k);
                }
                Key::Char(c) => text.push(c),
                Key::Tab => text.push('\t'),
                Key::Eof => return Ok(None),
                _ => {}
            }
        }
    }

    fn write(&mut self, name: Option<&str>, range: Option<(usize, usize)>) -> Result<String, String> {
        let name = match name.filter(|n| !n.is_empty()).map(String::from).or_else(|| self.file.clone()) {
            Some(n) => n,
            None => return Err("No file name".into()),
        };
        let (a, b) = range.unwrap_or((0, self.lines.len() - 1));
        let mut data = self.lines[a..=b].join("\n");
        data.push('\n');
        // Write next to the file, then rename: a crash never leaves half a file.
        let path = Path::new(&name);
        let tmp = path.with_file_name(format!(".{}.vi-tmp", path.file_name().map_or("file".into(), |n| n.to_string_lossy().into_owned())));
        let mode = fs::metadata(&name).map(|m| m.permissions());
        let r = fs::write(&tmp, &data).and_then(|_| {
            if let Ok(p) = mode {
                fs::set_permissions(&tmp, p)?;
            }
            fs::rename(&tmp, &name)
        });
        if r.is_err() {
            let _ = fs::remove_file(&tmp);
            // Maybe the directory is not writable but the file is: write in place.
            fs::write(&name, &data).map_err(|e| format!("\"{name}\" {e}"))?;
        }
        if range.is_none() && (self.file.is_none() || self.file.as_deref() == Some(&name)) {
            self.file = Some(name.clone());
            self.dirty = false;
        }
        Ok(format!("\"{name}\" {}L, {}B written", b - a + 1, data.len()))
    }

    fn parse_addr(&self, s: &str) -> Option<(usize, usize)> {
        // returns (line index, chars consumed)
        let mut i = 0;
        let b = s.as_bytes();
        let base = match b.first() {
            Some(b'.') => {
                i = 1;
                self.cur.r as i64
            }
            Some(b'$') => {
                i = 1;
                self.lines.len() as i64 - 1
            }
            Some(c) if c.is_ascii_digit() => {
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                s[..i].parse::<i64>().ok()? - 1
            }
            Some(b'+' | b'-') => self.cur.r as i64,
            _ => return None,
        };
        let mut v = base;
        while i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            let neg = b[i] == b'-';
            i += 1;
            let st = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let n: i64 = if st == i { 1 } else { s[st..i].parse().ok()? };
            v += if neg { -n } else { n };
        }
        Some((v.clamp(0, self.lines.len() as i64 - 1) as usize, i))
    }

    fn ex(&mut self, cmd: &str, tty: &fs::File) -> io::Result<()> {
        let cmd = cmd.trim_start_matches(':').trim();
        if cmd.is_empty() {
            return Ok(());
        }
        // Range.
        let mut rest = cmd;
        let mut range: Option<(usize, usize)> = None;
        if let Some(r) = rest.strip_prefix('%') {
            range = Some((0, self.lines.len() - 1));
            rest = r;
        } else if let Some((a, n)) = self.parse_addr(rest) {
            rest = &rest[n..];
            if let Some(r2) = rest.strip_prefix(',') {
                match self.parse_addr(r2) {
                    Some((b, m)) => {
                        range = Some((a.min(b), a.max(b)));
                        rest = &r2[m..];
                    }
                    None => {
                        self.message = "Invalid range".into();
                        return Ok(());
                    }
                }
            } else {
                range = Some((a, a));
            }
        }
        let rest = rest.trim();
        let (name, arg) = match rest.find(|c: char| !c.is_ascii_alphabetic() && c != '!') {
            Some(k) if k > 0 => (&rest[..k], rest[k..].trim()),
            Some(_) => ("", rest),
            None => (rest, ""),
        };
        let bang = name.ends_with('!');
        let name = name.trim_end_matches('!');
        match name {
            "" if range.is_some() && arg.is_empty() => {
                let (_, b) = range.unwrap();
                self.cur = Pos { r: b, c: self.first_nonblank(b) };
            }
            "w" | "write" | "wq" | "x" | "xit" | "exit" => {
                if name.starts_with('x') && !self.dirty && self.file.is_some() {
                    self.quit = true;
                    return Ok(());
                }
                match self.write(Some(arg), if name == "w" { range.filter(|_| cmd.contains(',') || cmd.starts_with('%')) } else { None }) {
                    Ok(m) => {
                        self.message = m;
                        if name != "w" && name != "write" {
                            self.quit = true;
                        }
                    }
                    Err(e) => self.message = e,
                }
            }
            "q" | "quit" => {
                if self.dirty && !bang {
                    self.message = "No write since last change (add ! to override)".into();
                } else {
                    self.quit = true;
                }
            }
            "e" | "edit" => {
                if self.dirty && !bang {
                    self.message = "No write since last change (add ! to override)".into();
                    return Ok(());
                }
                let f = if arg.is_empty() { self.file.clone() } else { Some(arg.to_string()) };
                let Some(f) = f else {
                    self.message = "No file name".into();
                    return Ok(());
                };
                self.load(&f);
            }
            "r" | "read" => match fs::read_to_string(arg) {
                Ok(t) => {
                    self.snapshot();
                    let at = range.map_or(self.cur.r, |r| r.1) + 1;
                    let new: Vec<String> = t.lines().map(String::from).collect();
                    let n = new.len();
                    self.lines.splice(at..at, new);
                    self.message = format!("\"{arg}\" {n}L");
                }
                Err(e) => self.message = format!("\"{arg}\" {e}"),
            },
            "d" | "delete" => {
                let (a, b) = range.unwrap_or((self.cur.r, self.cur.r));
                self.snapshot();
                self.reg = Some(self.take_lines(a, b, true));
                self.cur = Pos { r: a.min(self.lines.len() - 1), c: 0 };
                self.cur.c = self.first_nonblank(self.cur.r);
            }
            "s" | "substitute" => {
                let (a, b) = range.unwrap_or((self.cur.r, self.cur.r));
                self.substitute(arg, a, b);
            }
            "set" | "se" => {
                for o in arg.split_whitespace() {
                    match o {
                        "nu" | "number" => self.numbers = true,
                        "nonu" | "nonumber" => self.numbers = false,
                        "ic" | "ignorecase" => self.icase = true,
                        "noic" | "noignorecase" => self.icase = false,
                        _ => self.message = format!("Unknown option: {o}"),
                    }
                }
            }
            "noh" | "nohlsearch" => {}
            _ => {
                let _ = tty;
                self.message = format!("Not an editor command: {cmd}");
            }
        }
        Ok(())
    }

    fn substitute(&mut self, arg: &str, a: usize, b: usize) {
        let mut chars = arg.chars();
        let Some(d) = chars.next() else {
            self.message = "Usage: :s/pattern/replacement/[g]".into();
            return;
        };
        let rest: String = chars.collect();
        let mut parts = Vec::new();
        let mut cur = String::new();
        let mut esc = false;
        for c in rest.chars() {
            if esc {
                if c != d {
                    cur.push('\\');
                }
                cur.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == d && parts.len() < 2 {
                parts.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        }
        parts.push(cur);
        let pat = parts.first().cloned().unwrap_or_default();
        let repl = parts.get(1).cloned().unwrap_or_default();
        let flags = parts.get(2).cloned().unwrap_or_default();
        let pat = if pat.is_empty() { self.search.as_ref().map(|s| s.0.clone()).unwrap_or_default() } else { pat };
        let re = match self.regex(&pat) {
            Ok(r) => r,
            Err(e) => {
                self.message = format!("bad pattern: {e}");
                return;
            }
        };
        let which = if flags.contains('g') { Which::All } else { Which::Nth(1) };
        let mut total = 0;
        let mut changed_lines = 0;
        let before = self.lines.clone();
        for r in a..=b.min(self.lines.len() - 1) {
            let (new, n) = regex::replace(&re, self.lines[r].as_bytes(), which, |caps, t| vi_repl(repl.as_bytes(), caps, t));
            if n > 0 {
                let s = String::from_utf8_lossy(&new).into_owned();
                // A replacement may contain newlines (\r): split them into lines later.
                self.lines[r] = s;
                total += n;
                changed_lines += 1;
                self.cur = Pos { r, c: 0 };
            }
        }
        if total == 0 {
            self.message = format!("Pattern not found: {pat}");
            return;
        }
        // Expand newlines produced by \r in the replacement.
        let mut expanded = Vec::with_capacity(self.lines.len());
        for l in self.lines.drain(..) {
            expanded.extend(l.split('\n').map(String::from));
        }
        self.lines = expanded;
        self.undo.push((before, self.cur));
        self.redo.clear();
        self.dirty = true;
        self.cur.c = self.first_nonblank(self.cur.r.min(self.lines.len() - 1));
        if changed_lines > 1 {
            self.message = format!("{total} substitutions on {changed_lines} lines");
        }
    }

    fn load(&mut self, f: &str) {
        match fs::read(f) {
            Ok(d) => {
                let text = String::from_utf8_lossy(&d);
                let lossy = std::str::from_utf8(&d).is_err();
                let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
                let no_eol = !text.is_empty() && !text.ends_with('\n');
                if text.ends_with('\n') {
                    lines.pop();
                }
                if lines.is_empty() {
                    lines.push(String::new());
                }
                self.message = format!("\"{f}\" {}L, {}B{}{}", lines.len(), d.len(), if no_eol { " [noeol]" } else { "" }, if lossy { " [not UTF-8: invalid bytes will be replaced if you save]" } else { "" });
                self.lines = lines;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.lines = vec![String::new()];
                self.message = format!("\"{f}\" [New]");
            }
            Err(e) => {
                self.lines = vec![String::new()];
                self.message = format!("\"{f}\" {e}");
            }
        }
        self.file = Some(f.to_string());
        self.cur = Pos { r: 0, c: 0 };
        self.top = 0;
        self.dirty = false;
        self.undo.clear();
        self.redo.clear();
    }
}

/// :s replacement: & is the match, \1..\9 groups, \& literal &, \r and \n newline.
fn vi_repl(repl: &[u8], caps: &regex::Captures, text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < repl.len() {
        let c = repl[i];
        i += 1;
        match c {
            b'&' => {
                if let Some((s, e)) = caps[0] {
                    out.extend_from_slice(&text[s..e]);
                }
            }
            b'\\' if i < repl.len() => {
                let n = repl[i];
                i += 1;
                match n {
                    b'0'..=b'9' => {
                        if let Some(Some((s, e))) = caps.get(usize::from(n - b'0')) {
                            out.extend_from_slice(&text[*s..*e]);
                        }
                    }
                    b'r' | b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    _ => out.push(n),
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn cell_width(c: char) -> usize {
    if (c as u32) < 0x20 || c == '\x7f' {
        1
    } else {
        term::char_width(c)
    }
}

pub fn vi(args: &[String]) -> i32 {
    let mut file = None;
    let mut start_line: Option<String> = None;
    for a in args {
        if let Some(l) = a.strip_prefix('+') {
            start_line = Some(l.to_string());
        } else if a == "--help" || a == "-h" {
            println!("usage: vi [+LINE | +/PATTERN] [FILE]");
            return 0;
        } else if a.starts_with('-') && a.len() > 1 {
            error("vi", format!("unknown option {a}\nusage: vi [+LINE | +/PATTERN] [FILE]"));
            return 1;
        } else if file.is_none() {
            file = Some(a.clone());
        } else {
            error("vi", "only one file at a time");
            return 1;
        }
    }
    let mut tty = match fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
        Ok(t) => t,
        Err(e) => {
            error("vi", format!("needs a terminal: {e}"));
            return 1;
        }
    };
    let (rows, cols) = term::size(tty.as_raw_fd());
    let mut ed = Editor {
        lines: vec![String::new()],
        cur: Pos { r: 0, c: 0 },
        want_col: 0,
        top: 0,
        rows: rows.max(3),
        cols: cols.max(10),
        mode: Mode::Normal,
        file: None,
        dirty: false,
        message: String::new(),
        reg: None,
        undo: Vec::new(),
        redo: Vec::new(),
        search: None,
        last_find: None,
        pending: VecDeque::new(),
        recording: None,
        last_change: Vec::new(),
        numbers: false,
        icase: false,
        quit: false,
        insert_start: None,
        count_given: false,
    };
    if let Some(f) = &file {
        ed.load(f);
    }
    if let Some(l) = start_line {
        if let Some(p) = l.strip_prefix('/') {
            ed.search = Some((p.to_string(), true));
            if let Some(p) = ed.search_from(Pos { r: ed.lines.len() - 1, c: usize::MAX / 2 }, true) {
                ed.cur = p;
            }
        } else {
            let r = if l.is_empty() { ed.lines.len() } else { l.parse().unwrap_or(1) };
            ed.cur.r = r.max(1).min(ed.lines.len()) - 1;
        }
    }
    let raw = match Raw::enter(tty.as_raw_fd(), false) {
        Ok(r) => r,
        Err(e) => {
            error("vi", format!("cannot set up the terminal: {e}"));
            return 1;
        }
    };
    term::watch_resize();
    let _ = term::alt_screen(&mut tty, true);
    let r = run(&mut ed, &mut tty);
    let _ = term::alt_screen(&mut tty, false);
    drop(raw);
    match r {
        Ok(()) => 0,
        Err(e) => {
            error("vi", e);
            1
        }
    }
}

fn run(ed: &mut Editor, tty: &mut fs::File) -> io::Result<()> {
    while !ed.quit {
        ed.clamp();
        ed.draw(tty, None)?;
        let t = tty.try_clone()?;
        match ed.mode {
            Mode::Insert | Mode::Replace => insert_key(ed, &t)?,
            Mode::Normal => normal_key(ed, &t)?,
        }
    }
    Ok(())
}

fn insert_key(ed: &mut Editor, tty: &fs::File) -> io::Result<()> {
    let k = ed.next_key(tty)?;
    match k {
        Key::Esc | Key::Ctrl('c') => {
            ed.mode = Mode::Normal;
            ed.cur.c = ed.cur.c.saturating_sub(1);
            if let Some(rec) = ed.recording.take() {
                ed.last_change = rec;
            }
            ed.insert_start = None;
        }
        Key::Enter => ed.newline(),
        Key::Backspace => {
            if ed.cur.c > 0 {
                let line = &mut ed.lines[ed.cur.r];
                let (a, b) = (byte_at(line, ed.cur.c - 1), byte_at(line, ed.cur.c));
                line.replace_range(a..b, "");
                ed.cur.c -= 1;
            } else if ed.cur.r > 0 {
                let l = ed.lines.remove(ed.cur.r);
                ed.cur.r -= 1;
                ed.cur.c = clen(&ed.lines[ed.cur.r]);
                ed.lines[ed.cur.r].push_str(&l);
            }
            ed.dirty = true;
        }
        Key::Delete => {
            let len = ed.line_len(ed.cur.r);
            if ed.cur.c < len {
                let line = &mut ed.lines[ed.cur.r];
                let (a, b) = (byte_at(line, ed.cur.c), byte_at(line, ed.cur.c + 1));
                line.replace_range(a..b, "");
            } else if ed.cur.r + 1 < ed.lines.len() {
                let l = ed.lines.remove(ed.cur.r + 1);
                ed.lines[ed.cur.r].push_str(&l);
            }
            ed.dirty = true;
        }
        Key::Ctrl('w') => {
            let line: Vec<char> = ed.line().chars().collect();
            let mut c = ed.cur.c;
            while c > 0 && line[c - 1].is_whitespace() {
                c -= 1;
            }
            let k = if c > 0 { class(line[c - 1], false) } else { Class::Space };
            while c > 0 && class(line[c - 1], false) == k && k != Class::Space {
                c -= 1;
            }
            let l = &mut ed.lines[ed.cur.r];
            let (a, b) = (byte_at(l, c), byte_at(l, ed.cur.c));
            l.replace_range(a..b, "");
            ed.cur.c = c;
        }
        Key::Ctrl('u') => {
            let l = &mut ed.lines[ed.cur.r];
            let b = byte_at(l, ed.cur.c);
            l.replace_range(..b, "");
            ed.cur.c = 0;
        }
        Key::Tab => ed.insert_char('\t'),
        Key::Char(c) => ed.insert_char(c),
        Key::Left => ed.cur.c = ed.cur.c.saturating_sub(1),
        Key::Right => ed.cur.c = (ed.cur.c + 1).min(ed.line_len(ed.cur.r)),
        Key::Up => ed.cur.r = ed.cur.r.saturating_sub(1),
        Key::Down => ed.cur.r = (ed.cur.r + 1).min(ed.lines.len() - 1),
        Key::Home => ed.cur.c = 0,
        Key::End => ed.cur.c = ed.line_len(ed.cur.r),
        Key::Ctrl('l') => {
            let mut t = tty.try_clone()?;
            t.write_all(b"\x1b[2J")?;
        }
        _ => {}
    }
    Ok(())
}

fn start_insert(ed: &mut Editor, replace: bool) {
    ed.mode = if replace { Mode::Replace } else { Mode::Insert };
    ed.insert_start = Some(ed.cur);
    ed.message.clear();
}

fn normal_key(ed: &mut Editor, tty: &fs::File) -> io::Result<()> {
    // A command starts here: record its keys for `.`.
    let replaying = !ed.pending.is_empty();
    ed.recording = if replaying { None } else { Some(Vec::new()) };
    let mut k = ed.next_key(tty)?;
    ed.message.clear();
    let mut count = 0usize;
    ed.count_given = false;
    while let Key::Char(c @ '0'..='9') = k {
        if c == '0' && count == 0 {
            break;
        }
        count = count * 10 + (c as usize - '0' as usize);
        ed.count_given = true;
        k = ed.next_key(tty)?;
    }
    let n = count.max(1);
    let mut changed = false; // a change command: remember it for `.`
    let mut finished = true; // false when insert mode follows (the change ends at Esc)
    match k {
        Key::Char(op @ ('d' | 'c' | 'y' | '>' | '<')) => {
            let mut k2 = ed.next_key(tty)?;
            let mut count2 = 0usize;
            while let Key::Char(c @ '0'..='9') = k2 {
                if c == '0' && count2 == 0 {
                    break;
                }
                count2 = count2 * 10 + (c as usize - '0' as usize);
                ed.count_given = true;
                k2 = ed.next_key(tty)?;
            }
            let total = n * count2.max(1);
            let motion = if k2 == Key::Char(op) {
                // dd, cc, yy, >>, <<: whole lines
                let last = (ed.cur.r + total - 1).min(ed.lines.len() - 1);
                Some(Motion { to: Pos { r: last, c: 0 }, linewise: true, inclusive: false })
            } else {
                ed.motion(k2, total, tty, Some(op))?
            };
            match motion {
                Some(m) => {
                    let insert = ed.apply_operator(op, m);
                    if op != 'y' {
                        changed = true;
                    }
                    if insert {
                        start_insert(ed, false);
                        finished = false;
                    }
                    if op == 'y' {
                        if let Some(r) = &ed.reg {
                            if r.linewise && r.text.len() > 2 {
                                ed.message = format!("{} lines yanked", r.text.len());
                            }
                        }
                    }
                }
                None => {}
            }
        }
        Key::Char('x') | Key::Delete => {
            if ed.line_len(ed.cur.r) > 0 {
                let to = Pos { r: ed.cur.r, c: (ed.cur.c + n).min(ed.line_len(ed.cur.r)) };
                ed.apply_operator('d', Motion { to, linewise: false, inclusive: false });
                changed = true;
            }
        }
        Key::Char('X') => {
            if ed.cur.c > 0 {
                let to = Pos { r: ed.cur.r, c: ed.cur.c.saturating_sub(n) };
                ed.apply_operator('d', Motion { to, linewise: false, inclusive: false });
                changed = true;
            }
        }
        Key::Char('D') | Key::Char('C') => {
            let to = Pos { r: ed.cur.r, c: ed.line_len(ed.cur.r) };
            let op = if k == Key::Char('D') { 'd' } else { 'c' };
            ed.apply_operator(op, Motion { to, linewise: false, inclusive: false });
            changed = true;
            if op == 'c' {
                start_insert(ed, false);
                finished = false;
            }
        }
        Key::Char('s') => {
            let to = Pos { r: ed.cur.r, c: (ed.cur.c + n).min(ed.line_len(ed.cur.r)) };
            ed.apply_operator('c', Motion { to, linewise: false, inclusive: false });
            start_insert(ed, false);
            changed = true;
            finished = false;
        }
        Key::Char('S') => {
            let last = (ed.cur.r + n - 1).min(ed.lines.len() - 1);
            ed.apply_operator('c', Motion { to: Pos { r: last, c: 0 }, linewise: true, inclusive: false });
            start_insert(ed, false);
            changed = true;
            finished = false;
        }
        Key::Char('Y') => {
            let last = (ed.cur.r + n - 1).min(ed.lines.len() - 1);
            ed.apply_operator('y', Motion { to: Pos { r: last, c: 0 }, linewise: true, inclusive: false });
        }
        Key::Char('p') | Key::Char('P') => {
            ed.put(k == Key::Char('p'), n);
            changed = true;
        }
        Key::Char('J') => {
            ed.snapshot();
            for _ in 0..n.max(2) - 1 {
                if ed.cur.r + 1 >= ed.lines.len() {
                    break;
                }
                let next = ed.lines.remove(ed.cur.r + 1);
                let l = &mut ed.lines[ed.cur.r];
                let trimmed = l.trim_end().to_string();
                *l = trimmed;
                let col = clen(l);
                let nt = next.trim_start();
                if !l.is_empty() && !nt.is_empty() && !nt.starts_with(')') {
                    l.push(' ');
                }
                l.push_str(nt);
                ed.cur.c = col;
            }
            changed = true;
        }
        Key::Char('r') => {
            if let Key::Char(c) = ed.next_key(tty)? {
                let len = ed.line_len(ed.cur.r);
                if ed.cur.c + n <= len {
                    ed.snapshot();
                    let l = &mut ed.lines[ed.cur.r];
                    let (a, b) = (byte_at(l, ed.cur.c), byte_at(l, ed.cur.c + n));
                    l.replace_range(a..b, &c.to_string().repeat(n));
                    ed.cur.c += n - 1;
                    changed = true;
                }
            }
        }
        Key::Char('~') => {
            ed.snapshot();
            let l: Vec<char> = ed.line().chars().collect();
            let end = (ed.cur.c + n).min(l.len());
            let flipped: String = l.iter().enumerate().map(|(i, c)| if i >= ed.cur.c && i < end { if c.is_uppercase() { c.to_lowercase().next().unwrap_or(*c) } else { c.to_uppercase().next().unwrap_or(*c) } } else { *c }).collect();
            ed.lines[ed.cur.r] = flipped;
            ed.cur.c = end.min(l.len().saturating_sub(1));
            changed = true;
        }
        Key::Char(c @ ('i' | 'a' | 'I' | 'A' | 'o' | 'O')) => {
            ed.snapshot();
            match c {
                'a' => {
                    if ed.line_len(ed.cur.r) > 0 {
                        ed.cur.c += 1;
                    }
                }
                'I' => ed.cur.c = ed.first_nonblank(ed.cur.r),
                'A' => ed.cur.c = ed.line_len(ed.cur.r),
                'o' | 'O' => {
                    let indent: String = ed.line().chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                    let at = if c == 'o' { ed.cur.r + 1 } else { ed.cur.r };
                    ed.lines.insert(at, indent.clone());
                    ed.cur = Pos { r: at, c: clen(&indent) };
                }
                _ => {}
            }
            ed.mode = Mode::Insert;
            start_insert(ed, false);
            changed = true;
            finished = false;
        }
        Key::Char('R') => {
            ed.snapshot();
            start_insert(ed, true);
            changed = true;
            finished = false;
        }
        Key::Char('u') => match ed.undo.pop() {
            Some((lines, cur)) => {
                ed.redo.push((ed.lines.clone(), ed.cur));
                ed.lines = lines;
                ed.cur = cur;
                ed.dirty = true;
            }
            None => ed.message = "Already at oldest change".into(),
        },
        Key::Ctrl('r') => match ed.redo.pop() {
            Some((lines, cur)) => {
                ed.undo.push((ed.lines.clone(), ed.cur));
                ed.lines = lines;
                ed.cur = cur;
                ed.dirty = true;
            }
            None => ed.message = "Already at newest change".into(),
        },
        Key::Char('.') => {
            let keys = ed.last_change.clone();
            for k in keys.into_iter().rev() {
                ed.pending.push_front(k);
            }
            ed.recording = None;
            return Ok(());
        }
        Key::Char(':') => {
            if let Some(cmd) = ed.read_cmdline(tty, ":")? {
                ed.ex(&cmd, tty)?;
            }
        }
        Key::Char('Z') => match ed.next_key(tty)? {
            Key::Char('Z') => ed.ex("x", tty)?,
            Key::Char('Q') => ed.ex("q!", tty)?,
            _ => {}
        },
        Key::Ctrl('g') => {
            let total = ed.lines.len();
            ed.message = format!("\"{}\"{} {total} lines --{}%--", ed.file.as_deref().unwrap_or("[No Name]"), if ed.dirty { " [Modified]" } else { "" }, (ed.cur.r + 1) * 100 / total);
        }
        Key::Ctrl('l') => {
            let mut t = tty.try_clone()?;
            t.write_all(b"\x1b[2J")?;
        }
        Key::Ctrl('f' | 'b' | 'd' | 'u') | Key::PageDown | Key::PageUp => {
            let page = ed.rows.saturating_sub(2).max(1);
            let c = match k {
                Key::PageDown => 'f',
                Key::PageUp => 'b',
                Key::Ctrl(c) => c,
                _ => 'f',
            };
            let step = if c == 'd' || c == 'u' { page / 2 } else { page } * n;
            let last = ed.lines.len() - 1;
            if c == 'f' || c == 'd' {
                ed.cur.r = (ed.cur.r + step).min(last);
                ed.top = (ed.top + step).min(last);
            } else {
                ed.cur.r = ed.cur.r.saturating_sub(step);
                ed.top = ed.top.saturating_sub(step);
            }
            ed.cur.c = ed.first_nonblank(ed.cur.r);
        }
        Key::Esc => {}
        other => {
            if let Some(m) = ed.motion(other, n, tty, None)? {
                ed.cur = m.to;
                if !matches!(other, Key::Char('j' | 'k') | Key::Up | Key::Down | Key::Ctrl('n') | Key::Ctrl('p')) {
                    ed.want_col = ed.cur.c;
                } else {
                    ed.cur.c = ed.want_col;
                }
                ed.recording = None;
                return Ok(());
            }
        }
    }
    if changed && finished {
        if let Some(rec) = ed.recording.take() {
            ed.last_change = rec;
        }
    } else if !changed {
        ed.recording = None;
    }
    ed.want_col = ed.cur.c;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor {
            lines: text.split('\n').map(String::from).collect(),
            cur: Pos { r: 0, c: 0 },
            want_col: 0,
            top: 0,
            rows: 24,
            cols: 80,
            mode: Mode::Normal,
            file: None,
            dirty: false,
            message: String::new(),
            reg: None,
            undo: Vec::new(),
            redo: Vec::new(),
            search: None,
            last_find: None,
            pending: VecDeque::new(),
            recording: None,
            last_change: Vec::new(),
            numbers: false,
            icase: false,
            quit: false,
            insert_start: None,
            count_given: false,
        }
    }

    #[test]
    fn word_motions() {
        let e = ed("foo bar.baz  qux\n\nnext line");
        assert_eq!(e.word_fwd(Pos { r: 0, c: 0 }, false, false), Pos { r: 0, c: 4 });
        assert_eq!(e.word_fwd(Pos { r: 0, c: 4 }, false, false), Pos { r: 0, c: 7 });
        assert_eq!(e.word_fwd(Pos { r: 0, c: 4 }, true, false), Pos { r: 0, c: 13 });
        assert_eq!(e.word_fwd(Pos { r: 0, c: 13 }, false, false), Pos { r: 1, c: 0 }, "an empty line is a word");
        assert_eq!(e.word_fwd(Pos { r: 0, c: 0 }, false, true), Pos { r: 0, c: 2 });
        assert_eq!(e.word_back(Pos { r: 0, c: 8 }, false), Pos { r: 0, c: 7 });
        assert_eq!(e.word_back(Pos { r: 0, c: 7 }, false), Pos { r: 0, c: 4 });
        assert_eq!(e.word_back(Pos { r: 2, c: 0 }, false), Pos { r: 1, c: 0 });
    }

    #[test]
    fn delete_and_put() {
        let mut e = ed("one two three");
        e.cur = Pos { r: 0, c: 4 };
        e.apply_operator('d', Motion { to: Pos { r: 0, c: 8 }, linewise: false, inclusive: false });
        assert_eq!(e.lines, ["one three"]);
        e.cur = Pos { r: 0, c: 8 };
        e.put(true, 1);
        assert_eq!(e.lines, ["one threetwo "]);
        e = ed("a\nb\nc");
        e.apply_operator('d', Motion { to: Pos { r: 1, c: 0 }, linewise: true, inclusive: false });
        assert_eq!(e.lines, ["c"]);
        e.put(true, 1);
        assert_eq!(e.lines, ["c", "a", "b"]);
        e.cur = Pos { r: 0, c: 0 };
        e.put(false, 1);
        assert_eq!(e.lines, ["a", "b", "c", "a", "b"]);
        // undo restores
        let (old, _) = e.undo.pop().unwrap();
        assert_eq!(old, ["c", "a", "b"]);
    }

    #[test]
    fn multi_line_ranges() {
        let mut e = ed("abc\ndef\nghi");
        e.cur = Pos { r: 0, c: 1 };
        let r = e.take_range(Pos { r: 0, c: 1 }, Pos { r: 2, c: 1 }, true);
        assert_eq!(r.text, ["bc", "def", "g"]);
        assert_eq!(e.lines, ["ahi"]);
    }

    #[test]
    fn ex_commands() {
        let mut e = ed("foo foo\nbar foo\nbaz");
        e.substitute("/foo/X/g", 0, 1);
        assert_eq!(e.lines, ["X X", "bar X", "baz"]);
        e.substitute("/\\(ba\\)\\(.\\)/\\2\\1/", 0, 2);
        assert_eq!(e.lines, ["X X", "rba X", "zba"]);
        e.substitute("/ /\\r/", 0, 0);
        assert_eq!(e.lines, ["X", "X", "rba X", "zba"]);
        assert_eq!(e.parse_addr("$"), Some((3, 1)));
        assert_eq!(e.parse_addr("2+1"), Some((2, 3)));
        assert_eq!(e.parse_addr("x"), None);
    }

    #[test]
    fn find_and_match() {
        let mut e = ed("call(a, (b), c) x");
        assert_eq!(e.find_char('f', ',', 2), Some(Pos { r: 0, c: 11 }));
        assert_eq!(e.find_char('t', ',', 1), Some(Pos { r: 0, c: 5 }));
        assert_eq!(e.match_pair(), Some(Pos { r: 0, c: 14 }));
        e.cur.c = 14;
        assert_eq!(e.match_pair(), Some(Pos { r: 0, c: 4 }));
        e.search = Some(("x".into(), true));
        assert_eq!(e.search_from(Pos { r: 0, c: 0 }, true), Some(Pos { r: 0, c: 16 }));
    }
}
