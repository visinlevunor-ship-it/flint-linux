//! Splits shell source into tokens. Words keep their quotes and `$` constructs as raw text;
//! the lexer only has to find where each word ends.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Word(String),
    /// `2` in `2>file`: digits immediately followed by `<` or `>`
    IoNumber(i32),
    Pipe,
    AndIf,
    OrIf,
    Semi,
    DSemi,
    Amp,
    LParen,
    RParen,
    Less,
    Great,
    DGreat,
    LessAnd,
    GreatAnd,
    /// `<<` or `<<-`; the body is in `Lexed::heredocs[idx]`
    HereDoc { strip: bool, quoted: bool, idx: usize },
    Newline,
}

#[derive(Debug, PartialEq)]
pub enum LexError {
    /// The input ends inside a quote, `$(`, here-document, ...: more input could complete it.
    Incomplete,
    Syntax(String),
}

#[derive(Debug)]
pub struct Lexed {
    pub toks: Vec<Tok>,
    pub heredocs: Vec<String>,
}

struct Lexer {
    c: Vec<char>,
    i: usize,
    toks: Vec<Tok>,
    pending: Vec<(String, bool, usize)>,
    bodies: Vec<String>,
}

pub fn lex(src: &str) -> Result<Lexed, LexError> {
    let mut lx = Lexer { c: src.chars().collect(), i: 0, toks: Vec::new(), pending: Vec::new(), bodies: Vec::new() };
    lx.run()?;
    if !lx.pending.is_empty() {
        return Err(LexError::Incomplete);
    }
    Ok(Lexed { toks: lx.toks, heredocs: lx.bodies })
}

impl Lexer {
    fn at(&self, off: usize) -> Option<char> {
        self.c.get(self.i + off).copied()
    }

    fn run(&mut self) -> Result<(), LexError> {
        while let Some(ch) = self.at(0) {
            match ch {
                ' ' | '\t' => self.i += 1,
                '\\' if self.at(1) == Some('\n') => self.i += 2,
                '\n' => {
                    self.toks.push(Tok::Newline);
                    self.i += 1;
                    self.read_heredoc_bodies()?;
                }
                '#' => {
                    while self.at(0).is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '|' => {
                    if self.at(1) == Some('|') {
                        self.i += 2;
                        self.toks.push(Tok::OrIf);
                    } else {
                        self.i += 1;
                        self.toks.push(Tok::Pipe);
                    }
                }
                '&' => {
                    if self.at(1) == Some('&') {
                        self.i += 2;
                        self.toks.push(Tok::AndIf);
                    } else {
                        self.i += 1;
                        self.toks.push(Tok::Amp);
                    }
                }
                ';' => {
                    if self.at(1) == Some(';') {
                        self.i += 2;
                        self.toks.push(Tok::DSemi);
                    } else {
                        self.i += 1;
                        self.toks.push(Tok::Semi);
                    }
                }
                '(' => {
                    self.i += 1;
                    self.toks.push(Tok::LParen);
                }
                ')' => {
                    self.i += 1;
                    self.toks.push(Tok::RParen);
                }
                '<' => self.lex_less()?,
                '>' => {
                    match self.at(1) {
                        Some('>') => {
                            self.i += 2;
                            self.toks.push(Tok::DGreat);
                        }
                        Some('&') => {
                            self.i += 2;
                            self.toks.push(Tok::GreatAnd);
                        }
                        Some('|') => {
                            self.i += 2;
                            self.toks.push(Tok::Great);
                        }
                        _ => {
                            self.i += 1;
                            self.toks.push(Tok::Great);
                        }
                    }
                }
                _ => {
                    let w = self.read_word()?;
                    let all_digits = !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit());
                    if all_digits && matches!(self.at(0), Some('<' | '>')) {
                        match w.parse::<i32>() {
                            Ok(n) => self.toks.push(Tok::IoNumber(n)),
                            Err(_) => return Err(LexError::Syntax(format!("bad file descriptor {w}"))),
                        }
                    } else {
                        self.toks.push(Tok::Word(w));
                    }
                }
            }
        }
        Ok(())
    }

    fn lex_less(&mut self) -> Result<(), LexError> {
        match self.at(1) {
            Some('&') => {
                self.i += 2;
                self.toks.push(Tok::LessAnd);
            }
            Some('<') => {
                let strip = self.at(2) == Some('-');
                self.i += if strip { 3 } else { 2 };
                while matches!(self.at(0), Some(' ' | '\t')) {
                    self.i += 1;
                }
                if self.at(0).is_none() {
                    return Err(LexError::Syntax("missing here-document delimiter".into()));
                }
                let raw = self.read_word()?;
                if raw.is_empty() {
                    return Err(LexError::Syntax("missing here-document delimiter".into()));
                }
                let quoted = raw.contains(['\'', '"', '\\']);
                let delim: String = raw.chars().filter(|c| !matches!(c, '\'' | '"' | '\\')).collect();
                let idx = self.bodies.len();
                self.bodies.push(String::new());
                self.pending.push((delim, strip, idx));
                self.toks.push(Tok::HereDoc { strip, quoted, idx });
            }
            _ => {
                self.i += 1;
                self.toks.push(Tok::Less);
            }
        }
        Ok(())
    }

    /// After a newline: read the bodies of the here-documents opened on that line.
    fn read_heredoc_bodies(&mut self) -> Result<(), LexError> {
        let pending = std::mem::take(&mut self.pending);
        for (delim, strip, idx) in pending {
            let mut body = String::new();
            loop {
                if self.i >= self.c.len() {
                    return Err(LexError::Incomplete);
                }
                let start = self.i;
                while self.i < self.c.len() && self.c[self.i] != '\n' {
                    self.i += 1;
                }
                let line: String = self.c[start..self.i].iter().collect();
                let had_newline = self.i < self.c.len();
                if had_newline {
                    self.i += 1;
                }
                let cmp = if strip { line.trim_start_matches('\t') } else { line.as_str() };
                if cmp == delim {
                    break;
                }
                if !had_newline {
                    return Err(LexError::Incomplete);
                }
                body.push_str(cmp);
                body.push('\n');
            }
            self.bodies[idx] = body;
        }
        Ok(())
    }

    fn read_word(&mut self) -> Result<String, LexError> {
        let mut raw = String::new();
        while let Some(ch) = self.at(0) {
            if matches!(ch, ' ' | '\t' | '\n' | '|' | '&' | ';' | '(' | ')' | '<' | '>') {
                break;
            }
            match ch {
                '\\' => match self.at(1) {
                    None => return Err(LexError::Incomplete),
                    Some('\n') => self.i += 2,
                    Some(n) => {
                        raw.push('\\');
                        raw.push(n);
                        self.i += 2;
                    }
                },
                '\'' => {
                    raw.push('\'');
                    self.i += 1;
                    loop {
                        match self.at(0) {
                            None => return Err(LexError::Incomplete),
                            Some('\'') => {
                                raw.push('\'');
                                self.i += 1;
                                break;
                            }
                            Some(c) => {
                                raw.push(c);
                                self.i += 1;
                            }
                        }
                    }
                }
                '"' => self.scan_dquote(&mut raw)?,
                '$' => self.scan_dollar(&mut raw)?,
                '`' => self.scan_backtick(&mut raw)?,
                c => {
                    raw.push(c);
                    self.i += 1;
                }
            }
        }
        Ok(raw)
    }

    fn scan_dquote(&mut self, raw: &mut String) -> Result<(), LexError> {
        raw.push('"');
        self.i += 1;
        loop {
            match self.at(0) {
                None => return Err(LexError::Incomplete),
                Some('"') => {
                    raw.push('"');
                    self.i += 1;
                    return Ok(());
                }
                Some('\\') => match self.at(1) {
                    None => return Err(LexError::Incomplete),
                    Some(n) => {
                        raw.push('\\');
                        raw.push(n);
                        self.i += 2;
                    }
                },
                Some('$') => self.scan_dollar(raw)?,
                Some('`') => self.scan_backtick(raw)?,
                Some(c) => {
                    raw.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn scan_backtick(&mut self, raw: &mut String) -> Result<(), LexError> {
        raw.push('`');
        self.i += 1;
        loop {
            match self.at(0) {
                None => return Err(LexError::Incomplete),
                Some('`') => {
                    raw.push('`');
                    self.i += 1;
                    return Ok(());
                }
                Some('\\') => match self.at(1) {
                    None => return Err(LexError::Incomplete),
                    Some(n) => {
                        raw.push('\\');
                        raw.push(n);
                        self.i += 2;
                    }
                },
                Some(c) => {
                    raw.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// At a `$`: copy `$(...)`, `$((...))` and `${...}` whole so that spaces and operators
    /// inside them do not split the word.
    fn scan_dollar(&mut self, raw: &mut String) -> Result<(), LexError> {
        match self.at(1) {
            Some('(') => {
                raw.push('$');
                self.i += 1;
                self.scan_balanced(raw, '(', ')')
            }
            Some('{') => {
                raw.push('$');
                self.i += 1;
                self.scan_balanced(raw, '{', '}')
            }
            _ => {
                raw.push('$');
                self.i += 1;
                Ok(())
            }
        }
    }

    fn scan_balanced(&mut self, raw: &mut String, open: char, close: char) -> Result<(), LexError> {
        let mut depth = 0usize;
        while let Some(ch) = self.at(0) {
            raw.push(ch);
            self.i += 1;
            match ch {
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                '\\' => {
                    if let Some(n) = self.at(0) {
                        raw.push(n);
                        self.i += 1;
                    }
                }
                '\'' => loop {
                    match self.at(0) {
                        None => return Err(LexError::Incomplete),
                        Some(c) => {
                            raw.push(c);
                            self.i += 1;
                            if c == '\'' {
                                break;
                            }
                        }
                    }
                },
                '"' => loop {
                    match self.at(0) {
                        None => return Err(LexError::Incomplete),
                        Some('\\') => {
                            raw.push('\\');
                            self.i += 1;
                            if let Some(n) = self.at(0) {
                                raw.push(n);
                                self.i += 1;
                            }
                        }
                        Some(c) => {
                            raw.push(c);
                            self.i += 1;
                            if c == '"' {
                                break;
                            }
                        }
                    }
                },
                _ => {}
            }
        }
        Err(LexError::Incomplete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        lex(s).unwrap().toks
    }

    fn w(s: &str) -> Tok {
        Tok::Word(s.to_string())
    }

    #[test]
    fn words_and_operators() {
        assert_eq!(toks("echo hello  world"), [w("echo"), w("hello"), w("world")]);
        assert_eq!(toks("a|b&&c||d;e&"), [w("a"), Tok::Pipe, w("b"), Tok::AndIf, w("c"), Tok::OrIf, w("d"), Tok::Semi, w("e"), Tok::Amp]);
        assert_eq!(toks("a;;b"), [w("a"), Tok::DSemi, w("b")]);
        assert_eq!(toks("(a)"), [Tok::LParen, w("a"), Tok::RParen]);
        assert_eq!(toks("a\nb"), [w("a"), Tok::Newline, w("b")]);
    }

    #[test]
    fn quotes_stay_in_the_word() {
        assert_eq!(toks("echo 'a b' \"c d\" e\\ f"), [w("echo"), w("'a b'"), w("\"c d\""), w("e\\ f")]);
        assert_eq!(toks("a'|'b"), [w("a'|'b")]);
        assert_eq!(toks("\"a|b;c\""), [w("\"a|b;c\"")]);
        assert_eq!(toks("x\"y z\"w"), [w("x\"y z\"w")]);
    }

    #[test]
    fn dollar_constructs_stay_whole() {
        assert_eq!(toks("echo $(ls | wc -l) done"), [w("echo"), w("$(ls | wc -l)"), w("done")]);
        assert_eq!(toks("echo $((1 + (2*3)))"), [w("echo"), w("$((1 + (2*3)))")]);
        assert_eq!(toks("echo ${x:-a b}"), [w("echo"), w("${x:-a b}")]);
        assert_eq!(toks("echo \"$(echo \"a b\")\""), [w("echo"), w("\"$(echo \"a b\")\"")]);
        assert_eq!(toks("echo `echo a b`"), [w("echo"), w("`echo a b`")]);
        assert_eq!(toks("echo $(echo ')')"), [w("echo"), w("$(echo ')')")]);
    }

    #[test]
    fn redirections() {
        assert_eq!(toks("a >f 2>&1 <g >>h"), [w("a"), Tok::Great, w("f"), Tok::IoNumber(2), Tok::GreatAnd, w("1"), Tok::Less, w("g"), Tok::DGreat, w("h")]);
        assert_eq!(toks("echo 2 > f"), [w("echo"), w("2"), Tok::Great, w("f")], "a space after the digit makes it a word");
        assert_eq!(toks("a <&3"), [w("a"), Tok::LessAnd, w("3")]);
    }

    #[test]
    fn comments_and_continuations() {
        assert_eq!(toks("a # comment\nb"), [w("a"), Tok::Newline, w("b")]);
        assert_eq!(toks("a#b"), [w("a#b")], "# inside a word is not a comment");
        assert_eq!(toks("ab\\\ncd"), [w("abcd")]);
        assert_eq!(toks("a \\\n b"), [w("a"), w("b")]);
    }

    #[test]
    fn heredocs() {
        let l = lex("cat <<EOF\nline1\n$x\nEOF\necho after\n").unwrap();
        assert_eq!(l.heredocs, ["line1\n$x\n"]);
        assert_eq!(l.toks[0], w("cat"));
        assert_eq!(l.toks[1], Tok::HereDoc { strip: false, quoted: false, idx: 0 });
        assert_eq!(l.toks[2], Tok::Newline);
        assert_eq!(l.toks[3], w("echo"));

        let q = lex("cat <<'EOF'\n$x\nEOF\n").unwrap();
        assert_eq!(q.toks[1], Tok::HereDoc { strip: false, quoted: true, idx: 0 });

        let d = lex("cat <<-EOF\n\tindented\n\tEOF\n").unwrap();
        assert_eq!(d.heredocs, ["indented\n"]);

        let two = lex("a <<X; b <<Y\n1\nX\n2\nY\n").unwrap();
        assert_eq!(two.heredocs, ["1\n", "2\n"]);
    }

    #[test]
    fn incomplete_input() {
        for s in ["echo 'abc", "echo \"abc", "echo $(ls", "echo ${x", "echo `ls", "cat <<EOF\nabc\n", "cat <<EOF", "echo abc\\"] {
            assert_eq!(lex(s).unwrap_err(), LexError::Incomplete, "{s:?}");
        }
        assert!(matches!(lex("cat <<").unwrap_err(), LexError::Syntax(_)));
    }
}
