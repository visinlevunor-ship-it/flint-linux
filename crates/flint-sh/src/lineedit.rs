//! The interactive line editor: cursor movement and editing (emacs-style keys), history
//! (kept in ~/.sh_history between sessions), Ctrl-R search, and Tab completion.
//!
//! Keys: Left/Right ^B/^F, Home/End ^A/^E, Alt-B/Alt-F or Ctrl-Left/Right (word),
//! Backspace, Delete, ^D (delete, or end of input on an empty line), ^K ^U ^W Alt-D (kill),
//! ^Y (yank), ^T (transpose), Up/Down ^P/^N (history), ^R (search history), Alt-. (last
//! word of the previous command), Tab (complete), ^L (clear screen), ^C (cancel).

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use flint_core::term::{self, Key, Raw};

pub enum Edit {
    Line(String),
    Eof,
    Interrupted,
}

/// What completion found: the replacement starts at byte `start` of the line; candidates
/// are full replacement words. `suffix` is added after a single match (" " or "/").
pub struct Completions {
    pub start: usize,
    pub items: Vec<(String, &'static str)>,
}

pub struct LineEditor {
    pub history: Vec<String>,
    file: Option<PathBuf>,
    kill: String,
    max: usize,
}

/// Columns the text takes, skipping ANSI escape sequences.
pub fn visible_width(s: &str) -> usize {
    let mut w = 0;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            } else if it.peek() == Some(&']') {
                // OSC: up to BEL or ST
                for d in it.by_ref() {
                    if d == '\x07' {
                        break;
                    }
                }
            }
            continue;
        }
        w += term::char_width(c);
    }
    w
}

fn char_starts(s: &str) -> Vec<usize> {
    s.char_indices().map(|(b, _)| b).chain(std::iter::once(s.len())).collect()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte index of the start of the word before `pos`.
fn word_left(s: &str, pos: usize) -> usize {
    let chars: Vec<(usize, char)> = s[..pos].char_indices().collect();
    let mut i = chars.len();
    while i > 0 && !is_word(chars[i - 1].1) {
        i -= 1;
    }
    while i > 0 && is_word(chars[i - 1].1) {
        i -= 1;
    }
    chars.get(i).map_or(if i == 0 { 0 } else { pos }, |c| c.0)
}

/// Byte index after the word at or after `pos`.
fn word_right(s: &str, pos: usize) -> usize {
    let mut it = s[pos..].char_indices().peekable();
    while let Some(&(_, c)) = it.peek() {
        if is_word(c) {
            break;
        }
        it.next();
    }
    while let Some(&(_, c)) = it.peek() {
        if !is_word(c) {
            break;
        }
        it.next();
    }
    it.peek().map_or(s.len(), |(b, _)| pos + b)
}

/// Longest common prefix of the candidates.
pub fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else { return String::new() };
    let mut len = first.len();
    for it in &items[1..] {
        len = first.char_indices().zip(it.chars()).take_while(|((_, a), b)| a == b).map(|((i, a), _)| i + a.len_utf8()).last().unwrap_or(0).min(len);
    }
    first[..len].to_string()
}

struct State<'a> {
    line: String,
    pos: usize,
    prompt_last: &'a str,
    /// Rows above the cursor's row that the previous draw used (to get back to its start).
    cursor_row: usize,
}

impl State<'_> {
    fn refresh(&mut self, out: &mut impl Write, cols: usize) -> io::Result<()> {
        let cols = cols.max(10);
        let mut s = String::new();
        if self.cursor_row > 0 {
            s.push_str(&format!("\x1b[{}A", self.cursor_row));
        }
        s.push_str("\r\x1b[J");
        s.push_str(self.prompt_last);
        s.push_str(&self.line);
        let plen = visible_width(self.prompt_last);
        let total = plen + term::str_width(&self.line);
        // At an exact multiple of the width the terminal waits before wrapping: force it.
        if total > 0 && total % cols == 0 {
            s.push_str("\r\n");
        }
        let end_row = total / cols;
        let cpos = plen + term::str_width(&self.line[..self.pos]);
        let (crow, ccol) = (cpos / cols, cpos % cols);
        if end_row > crow {
            s.push_str(&format!("\x1b[{}A", end_row - crow));
        }
        s.push('\r');
        if ccol > 0 {
            s.push_str(&format!("\x1b[{ccol}C"));
        }
        self.cursor_row = crow;
        out.write_all(s.as_bytes())?;
        out.flush()
    }

    fn insert(&mut self, text: &str) {
        self.line.insert_str(self.pos, text);
        self.pos += text.len();
    }

    fn prev_char(&self) -> Option<usize> {
        self.line[..self.pos].char_indices().last().map(|(b, _)| b)
    }

    fn next_char(&self) -> Option<usize> {
        self.line[self.pos..].chars().next().map(|c| self.pos + c.len_utf8())
    }
}

impl LineEditor {
    pub fn new(file: Option<PathBuf>) -> LineEditor {
        let mut history = Vec::new();
        if let Some(f) = &file {
            if let Ok(text) = fs::read_to_string(f) {
                history = text.lines().filter(|l| !l.is_empty()).map(String::from).collect();
                // Keep the file from growing without bound.
                if history.len() > 2000 {
                    history.drain(..history.len() - 1000);
                    let _ = fs::write(f, history.join("\n") + "\n");
                }
            }
        }
        LineEditor { history, file, kill: String::new(), max: 1000 }
    }

    /// Remember an entered line (not blank ones, not repeats, not ones starting with a space).
    pub fn add(&mut self, line: &str) {
        let line = line.trim_end_matches('\n');
        if line.trim().is_empty() || line.starts_with(' ') || self.history.last().is_some_and(|l| l == line) {
            return;
        }
        // Multi-line commands are stored on one line: joined with "; ", or a space after
        // something that continues the command (a pipe, `&&`, `do`, `then`, ...).
        let mut one = String::new();
        for (k, piece) in line.split('\n').enumerate() {
            if k > 0 {
                let prev = one.trim_end();
                let cont = ["|", "&&", "||", "do", "then", "else", "{", "(", ";"].iter().any(|t| prev.ends_with(t)) || prev.is_empty();
                one = prev.to_string();
                one.push_str(if cont { " " } else { "; " });
            }
            one.push_str(piece.trim_start());
        }
        self.history.push(one.clone());
        if self.history.len() > self.max {
            self.history.remove(0);
        }
        if let Some(f) = &self.file {
            if let Ok(mut h) = fs::OpenOptions::new().append(true).create(true).open(f) {
                let _ = writeln!(h, "{one}");
            }
        }
    }

    /// Read a line on the terminal at fd 0, showing `prompt` (which may span lines and
    /// contain colour escapes).
    pub fn read(&mut self, prompt: &str, complete: &mut dyn FnMut(&str, usize) -> Completions) -> io::Result<Edit> {
        let mut out = io::stderr();
        let (head, last) = match prompt.rfind('\n') {
            Some(k) => (&prompt[..=k], &prompt[k + 1..]),
            None => ("", prompt),
        };
        out.write_all(head.as_bytes())?;
        let raw = Raw::enter(0, true)?;
        let mut st = State { line: String::new(), pos: 0, prompt_last: last, cursor_row: 0 };
        let mut hist_idx = self.history.len();
        let mut saved_new = String::new();
        let mut last_tab = false;
        let mut last_word_idx: Option<usize> = None;
        let r = loop {
            let (_, cols) = term::size(0);
            st.refresh(&mut out, cols)?;
            let key = term::read_key(0, None)?;
            let was_tab = last_tab;
            last_tab = false;
            if !matches!(key, Key::Alt('.')) {
                last_word_idx = None;
            }
            match key {
                Key::None => continue,
                Key::Enter => {
                    st.pos = st.line.len();
                    st.refresh(&mut out, cols)?;
                    out.write_all(b"\r\n")?;
                    break Edit::Line(st.line.clone());
                }
                Key::Ctrl('c') => {
                    st.pos = st.line.len();
                    st.refresh(&mut out, cols)?;
                    out.write_all(b"^C\r\n")?;
                    break Edit::Interrupted;
                }
                Key::Ctrl('d') | Key::Eof => {
                    if st.line.is_empty() {
                        out.write_all(b"\r\n")?;
                        break Edit::Eof;
                    }
                    if let Some(n) = st.next_char() {
                        st.line.replace_range(st.pos..n, "");
                    }
                }
                Key::Char(c) => {
                    let mut b = [0u8; 4];
                    st.insert(c.encode_utf8(&mut b));
                }
                Key::Left | Key::Ctrl('b') => {
                    if let Some(p) = st.prev_char() {
                        st.pos = p;
                    }
                }
                Key::Right | Key::Ctrl('f') => {
                    if let Some(n) = st.next_char() {
                        st.pos = n;
                    }
                }
                Key::Home | Key::Ctrl('a') => st.pos = 0,
                Key::End | Key::Ctrl('e') => st.pos = st.line.len(),
                Key::CtrlLeft | Key::Alt('b') => st.pos = word_left(&st.line, st.pos),
                Key::CtrlRight | Key::Alt('f') => st.pos = word_right(&st.line, st.pos),
                Key::Backspace | Key::Ctrl('h') => {
                    if let Some(p) = st.prev_char() {
                        st.line.replace_range(p..st.pos, "");
                        st.pos = p;
                    }
                }
                Key::Delete => {
                    if let Some(n) = st.next_char() {
                        st.line.replace_range(st.pos..n, "");
                    }
                }
                Key::Ctrl('k') => {
                    self.kill = st.line[st.pos..].to_string();
                    st.line.truncate(st.pos);
                }
                Key::Ctrl('u') => {
                    self.kill = st.line[..st.pos].to_string();
                    st.line.replace_range(..st.pos, "");
                    st.pos = 0;
                }
                Key::Ctrl('w') | Key::Alt('\x7f') => {
                    // ^W: back to whitespace (like readline's unix-word-rubout)
                    let start = if key == Key::Ctrl('w') {
                        let t = &st.line[..st.pos];
                        let trimmed = t.trim_end_matches(' ');
                        trimmed.rfind(' ').map_or(0, |k| k + 1)
                    } else {
                        word_left(&st.line, st.pos)
                    };
                    self.kill = st.line[start..st.pos].to_string();
                    st.line.replace_range(start..st.pos, "");
                    st.pos = start;
                }
                Key::Alt('d') => {
                    let end = word_right(&st.line, st.pos);
                    self.kill = st.line[st.pos..end].to_string();
                    st.line.replace_range(st.pos..end, "");
                }
                Key::Ctrl('y') => {
                    let k = self.kill.clone();
                    st.insert(&k);
                }
                Key::Ctrl('t') => {
                    // swap the two characters before the cursor (or around it)
                    let starts = char_starts(&st.line);
                    let ci = starts.iter().position(|&b| b == st.pos).unwrap_or(0);
                    let n = starts.len() - 1;
                    if n >= 2 && ci > 0 {
                        let ci = if ci == n { ci - 1 } else { ci };
                        let (a, b, c) = (starts[ci - 1], starts[ci], starts[ci + 1]);
                        let first = st.line[a..b].to_string();
                        let second = st.line[b..c].to_string();
                        st.line.replace_range(a..c, &(second + &first));
                        st.pos = c;
                    }
                }
                Key::Up | Key::Ctrl('p') => {
                    if hist_idx > 0 {
                        if hist_idx == self.history.len() {
                            saved_new = st.line.clone();
                        }
                        hist_idx -= 1;
                        st.line = self.history[hist_idx].clone();
                        st.pos = st.line.len();
                    }
                }
                Key::Down | Key::Ctrl('n') => {
                    if hist_idx < self.history.len() {
                        hist_idx += 1;
                        st.line = if hist_idx == self.history.len() { saved_new.clone() } else { self.history[hist_idx].clone() };
                        st.pos = st.line.len();
                    }
                }
                Key::Alt('.') => {
                    // Last word of an earlier command; repeat to go further back.
                    let idx = match last_word_idx {
                        Some(i) if i > 0 => i - 1,
                        Some(_) => continue,
                        None => match self.history.len().checked_sub(1) {
                            Some(i) => i,
                            None => continue,
                        },
                    };
                    if let Some(prev) = last_word_idx.and_then(|i| self.history.get(i)).and_then(|l| l.split_whitespace().last()) {
                        // remove the previously inserted word
                        let start = st.pos - prev.len().min(st.pos);
                        if st.line[start..st.pos] == *prev {
                            st.line.replace_range(start..st.pos, "");
                            st.pos = start;
                        }
                    }
                    if let Some(w) = self.history.get(idx).and_then(|l| l.split_whitespace().last()).map(String::from) {
                        st.insert(&w);
                    }
                    last_word_idx = Some(idx);
                }
                Key::Ctrl('r') => {
                    if let Some(found) = self.search(&mut out, &mut st)? {
                        match found {
                            SearchEnd::Accept => {
                                st.pos = st.line.len();
                                st.refresh(&mut out, cols)?;
                                out.write_all(b"\r\n")?;
                                break Edit::Line(st.line.clone());
                            }
                            SearchEnd::Edit => {}
                        }
                    }
                }
                Key::Ctrl('l') => {
                    out.write_all(b"\x1b[H\x1b[2J")?;
                    out.write_all(head.as_bytes())?;
                    st.cursor_row = 0;
                }
                Key::Tab => {
                    let c = complete(&st.line, st.pos);
                    let names: Vec<String> = c.items.iter().map(|(n, _)| n.clone()).collect();
                    let current = st.line[c.start..st.pos].to_string();
                    match c.items.len() {
                        0 => out.write_all(b"\x07")?,
                        1 => {
                            let (word, suffix) = &c.items[0];
                            let text = format!("{word}{suffix}");
                            st.line.replace_range(c.start..st.pos, &text);
                            st.pos = c.start + text.len();
                        }
                        _ => {
                            let prefix = common_prefix(&names);
                            if prefix.len() > current.len() {
                                st.line.replace_range(c.start..st.pos, &prefix);
                                st.pos = c.start + prefix.len();
                            } else if was_tab || c.items.len() <= 1 {
                                // Second Tab: show the choices under the line.
                                st.pos = st.line.len();
                                st.refresh(&mut out, cols)?;
                                out.write_all(b"\r\n")?;
                                show_columns(&mut out, &names, cols)?;
                                st.cursor_row = 0;
                                st.pos = c.start + current.len();
                            } else {
                                out.write_all(b"\x07")?;
                                last_tab = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        };
        drop(raw);
        Ok(r)
    }

    /// Ctrl-R: incremental search backwards through history.
    fn search(&mut self, out: &mut impl Write, st: &mut State) -> io::Result<Option<SearchEnd>> {
        let mut query = String::new();
        let mut idx = self.history.len();
        let original = (st.line.clone(), st.pos);
        let mut found: Option<usize> = None;
        loop {
            let shown = found.map(|i| self.history[i].as_str()).unwrap_or("");
            let (_, cols) = term::size(0);
            let label = format!("(reverse-i-search)`{query}': ");
            let mut s = format!("\r\x1b[J{label}{shown}");
            let w = visible_width(&label) + term::str_width(shown);
            if w > cols {
                s = format!("\r\x1b[J{label}{}", shown.chars().take(cols.saturating_sub(visible_width(&label) + 1)).collect::<String>());
            }
            if st.cursor_row > 0 {
                s.insert_str(0, &format!("\x1b[{}A", st.cursor_row));
                st.cursor_row = 0;
            }
            out.write_all(s.as_bytes())?;
            out.flush()?;
            let key = term::read_key(0, None)?;
            let find_from = |from: usize, q: &str, h: &[String]| -> Option<usize> { (0..from).rev().find(|&i| h[i].contains(q)) };
            match key {
                Key::Char(c) => {
                    query.push(c);
                    found = find_from(found.map_or(idx, |f| f + 1), &query, &self.history);
                    if let Some(f) = found {
                        idx = f;
                    }
                }
                Key::Backspace => {
                    query.pop();
                    found = if query.is_empty() { None } else { find_from(self.history.len(), &query, &self.history) };
                }
                Key::Ctrl('r') => {
                    if let Some(f) = found {
                        if let Some(n) = find_from(f, &query, &self.history) {
                            found = Some(n);
                            idx = n;
                        }
                    }
                }
                Key::Ctrl('g') | Key::Esc | Key::Ctrl('c') => {
                    st.line = original.0;
                    st.pos = original.1;
                    out.write_all(b"\r\x1b[J")?;
                    return Ok(None);
                }
                Key::Enter => {
                    if let Some(f) = found {
                        st.line = self.history[f].clone();
                    }
                    out.write_all(b"\r\x1b[J")?;
                    return Ok(Some(SearchEnd::Accept));
                }
                _ => {
                    // Any other key: take the match and keep editing it.
                    if let Some(f) = found {
                        st.line = self.history[f].clone();
                        st.pos = st.line.len();
                    }
                    out.write_all(b"\r\x1b[J")?;
                    return Ok(Some(SearchEnd::Edit));
                }
            }
        }
    }
}

enum SearchEnd {
    Accept,
    Edit,
}

fn show_columns(out: &mut impl Write, names: &[String], cols: usize) -> io::Result<()> {
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort();
    sorted.dedup();
    if sorted.len() > 200 {
        out.write_all(format!("({} possibilities: type more of the name)\r\n", sorted.len()).as_bytes())?;
        return Ok(());
    }
    let width = sorted.iter().map(|n| term::str_width(n)).max().unwrap_or(1) + 2;
    let per_row = (cols / width).max(1);
    let rows = sorted.len().div_ceil(per_row);
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..per_row {
            if let Some(n) = sorted.get(c * rows + r) {
                line.push_str(n);
                if c + 1 < per_row {
                    line.push_str(&" ".repeat(width - term::str_width(n)));
                }
            }
        }
        out.write_all(line.trim_end().as_bytes())?;
        out.write_all(b"\r\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(visible_width("\x1b[1;32muser@host\x1b[0m:~$ "), 13);
        assert_eq!(word_left("ls foo/bar", 10), 7);
        assert_eq!(word_left("ls foo  ", 8), 3);
        assert_eq!(word_right("ls foo bar", 2), 6);
        assert_eq!(common_prefix(&["prefix1".into(), "prefix2".into(), "pre".into()]), "pre");
        assert_eq!(common_prefix(&["zaż".into(), "zażółć".into()]), "zaż");
    }

    #[test]
    fn history_rules() {
        let mut e = LineEditor::new(None);
        e.add("ls\n");
        e.add("ls");
        e.add(" secret");
        e.add("   ");
        e.add("for i in 1 2\ndo echo $i\ndone");
        e.add("ls |\n  wc -l");
        assert_eq!(e.history, ["ls", "for i in 1 2; do echo $i; done", "ls | wc -l"]);
    }
}
