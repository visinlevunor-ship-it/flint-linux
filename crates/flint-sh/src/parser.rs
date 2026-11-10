//! Recursive-descent parser: tokens to [`Command`].

use std::rc::Rc;

use crate::ast::{Command, Item, Redir, RedirKind, Simple};
use crate::lexer::{lex, LexError, Tok};

#[derive(Debug, PartialEq)]
pub enum ParseError {
    /// The input ends in the middle of a construct; reading more lines may complete it.
    Incomplete,
    Syntax(String),
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        match e {
            LexError::Incomplete => ParseError::Incomplete,
            LexError::Syntax(m) => ParseError::Syntax(m),
        }
    }
}

const RESERVED: [&str; 14] = ["if", "then", "elif", "else", "fi", "while", "until", "do", "done", "for", "case", "esac", "in", "}"];

pub fn parse(src: &str) -> Result<Command, ParseError> {
    let lexed = lex(src)?;
    let mut p = Parser { t: lexed.toks, p: 0, bodies: lexed.heredocs };
    let list = p.list(&[])?;
    match p.peek() {
        None => Ok(list),
        Some(t) => Err(ParseError::Syntax(format!("unexpected {}", describe(t)))),
    }
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Word(w) => format!("'{w}'"),
        Tok::IoNumber(n) => format!("'{n}'"),
        Tok::Pipe => "'|'".into(),
        Tok::AndIf => "'&&'".into(),
        Tok::OrIf => "'||'".into(),
        Tok::Semi => "';'".into(),
        Tok::DSemi => "';;'".into(),
        Tok::Amp => "'&'".into(),
        Tok::LParen => "'('".into(),
        Tok::RParen => "')'".into(),
        Tok::Less => "'<'".into(),
        Tok::Great => "'>'".into(),
        Tok::DGreat => "'>>'".into(),
        Tok::LessAnd => "'<&'".into(),
        Tok::GreatAnd => "'>&'".into(),
        Tok::HereDoc { .. } => "'<<'".into(),
        Tok::Newline => "newline".into(),
    }
}

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// `NAME=value` with an unquoted name.
fn split_assignment(w: &str) -> Option<(String, String)> {
    let eq = w.find('=')?;
    let name = &w[..eq];
    is_name(name).then(|| (name.to_string(), w[eq + 1..].to_string()))
}

struct Parser {
    t: Vec<Tok>,
    p: usize,
    bodies: Vec<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.p)
    }

    fn peek_at(&self, off: usize) -> Option<&Tok> {
        self.t.get(self.p + off)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.t.get(self.p).cloned();
        if t.is_some() {
            self.p += 1;
        }
        t
    }

    fn at_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(x)) if x == w)
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Some(Tok::Newline)) {
            self.p += 1;
        }
    }

    fn expect_word(&mut self, w: &str) -> Result<(), ParseError> {
        match self.peek() {
            None => Err(ParseError::Incomplete),
            Some(Tok::Word(x)) if x == w => {
                self.p += 1;
                Ok(())
            }
            Some(t) => Err(ParseError::Syntax(format!("expected '{w}' but found {}", describe(t)))),
        }
    }

    fn is_stop(&self, stops: &[&str]) -> bool {
        match self.peek() {
            None | Some(Tok::RParen) | Some(Tok::DSemi) => true,
            Some(Tok::Word(w)) => stops.contains(&w.as_str()),
            _ => false,
        }
    }

    fn list(&mut self, stops: &[&str]) -> Result<Command, ParseError> {
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.is_stop(stops) {
                break;
            }
            let cmd = self.and_or()?;
            match self.peek() {
                Some(Tok::Amp) => {
                    self.p += 1;
                    items.push(Item { cmd, background: true });
                }
                Some(Tok::Semi) | Some(Tok::Newline) => {
                    self.p += 1;
                    items.push(Item { cmd, background: false });
                }
                _ => {
                    items.push(Item { cmd, background: false });
                    if !self.is_stop(stops) {
                        let t = self.peek().expect("not a stop token, so not EOF");
                        return Err(ParseError::Syntax(format!("unexpected {}", describe(t))));
                    }
                }
            }
        }
        Ok(Command::List(items))
    }

    fn and_or(&mut self) -> Result<Command, ParseError> {
        let first = self.pipeline()?;
        let mut rest = Vec::new();
        loop {
            let is_and = match self.peek() {
                Some(Tok::AndIf) => true,
                Some(Tok::OrIf) => false,
                _ => break,
            };
            self.p += 1;
            self.skip_newlines();
            rest.push((is_and, self.pipeline()?));
        }
        Ok(if rest.is_empty() { first } else { Command::AndOr(Box::new(first), rest) })
    }

    fn pipeline(&mut self) -> Result<Command, ParseError> {
        let negate = self.at_word("!");
        if negate {
            self.p += 1;
        }
        let mut stages = vec![self.command()?];
        while matches!(self.peek(), Some(Tok::Pipe)) {
            self.p += 1;
            self.skip_newlines();
            stages.push(self.command()?);
        }
        if stages.len() == 1 && !negate {
            Ok(stages.pop().expect("one stage"))
        } else {
            Ok(Command::Pipeline { negate, stages })
        }
    }

    fn is_redirect_start(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::IoNumber(_) | Tok::Less | Tok::Great | Tok::DGreat | Tok::LessAnd | Tok::GreatAnd | Tok::HereDoc { .. })
        )
    }

    fn redirect(&mut self) -> Result<Redir, ParseError> {
        let explicit_fd = if let Some(Tok::IoNumber(n)) = self.peek() {
            let n = *n;
            self.p += 1;
            Some(n)
        } else {
            None
        };
        let op = self.bump().ok_or(ParseError::Incomplete)?;
        if let Tok::HereDoc { quoted, idx, .. } = op {
            return Ok(Redir { fd: explicit_fd.unwrap_or(0), kind: RedirKind::HereDoc { body: self.bodies[idx].clone(), expand: !quoted } });
        }
        let target = match self.bump() {
            Some(Tok::Word(w)) => w,
            Some(t) => return Err(ParseError::Syntax(format!("expected a file name after redirection, found {}", describe(&t)))),
            None => return Err(ParseError::Syntax("expected a file name after redirection".into())),
        };
        let (default_fd, kind) = match op {
            Tok::Less => (0, RedirKind::In(target)),
            Tok::Great => (1, RedirKind::Out(target)),
            Tok::DGreat => (1, RedirKind::Append(target)),
            Tok::LessAnd => (0, RedirKind::Dup(target)),
            Tok::GreatAnd => (1, RedirKind::Dup(target)),
            other => return Err(ParseError::Syntax(format!("unexpected {}", describe(&other)))),
        };
        Ok(Redir { fd: explicit_fd.unwrap_or(default_fd), kind })
    }

    fn trailing_redirects(&mut self, cmd: Command) -> Result<Command, ParseError> {
        let mut redirs = Vec::new();
        while self.is_redirect_start() {
            redirs.push(self.redirect()?);
        }
        Ok(if redirs.is_empty() { cmd } else { Command::Redirected(Box::new(cmd), redirs) })
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        match self.peek() {
            None => return Err(ParseError::Incomplete),
            Some(Tok::LParen) => {
                self.p += 1;
                let body = self.list(&[])?;
                match self.peek() {
                    Some(Tok::RParen) => self.p += 1,
                    None => return Err(ParseError::Incomplete),
                    Some(t) => return Err(ParseError::Syntax(format!("expected ')' but found {}", describe(t)))),
                }
                let cmd = Command::Subshell(Box::new(body));
                return self.trailing_redirects(cmd);
            }
            Some(Tok::Word(w)) => {
                let w = w.clone();
                let compound = match w.as_str() {
                    "if" => Some(self.if_clause()?),
                    "while" | "until" => Some(self.while_clause()?),
                    "for" => Some(self.for_clause()?),
                    "case" => Some(self.case_clause()?),
                    "{" => Some(self.group()?),
                    _ => None,
                };
                if let Some(c) = compound {
                    return self.trailing_redirects(c);
                }
                if RESERVED.contains(&w.as_str()) {
                    return Err(ParseError::Syntax(format!("unexpected '{w}'")));
                }
                if is_name(&w) && matches!(self.peek_at(1), Some(Tok::LParen)) && matches!(self.peek_at(2), Some(Tok::RParen)) {
                    self.p += 3;
                    self.skip_newlines();
                    let body = self.command()?;
                    return Ok(Command::FuncDef { name: w, body: Rc::new(body) });
                }
            }
            _ => {}
        }
        self.simple()
    }

    fn simple(&mut self) -> Result<Command, ParseError> {
        let mut s = Simple::default();
        loop {
            match self.peek() {
                Some(Tok::Word(w)) => {
                    let w = w.clone();
                    self.p += 1;
                    if s.words.is_empty() {
                        if let Some((n, v)) = split_assignment(&w) {
                            s.assigns.push((n, v));
                            continue;
                        }
                    }
                    s.words.push(w);
                }
                _ if self.is_redirect_start() => s.redirs.push(self.redirect()?),
                _ => break,
            }
        }
        if s.words.is_empty() && s.assigns.is_empty() && s.redirs.is_empty() {
            return match self.peek() {
                None => Err(ParseError::Incomplete),
                Some(t) => Err(ParseError::Syntax(format!("unexpected {}", describe(t)))),
            };
        }
        Ok(Command::Simple(s))
    }

    fn group(&mut self) -> Result<Command, ParseError> {
        self.p += 1; // {
        let body = self.list(&["}"])?;
        self.expect_word("}")?;
        Ok(Command::Group(Box::new(body)))
    }

    fn if_clause(&mut self) -> Result<Command, ParseError> {
        self.p += 1; // if
        let mut arms = Vec::new();
        let mut otherwise = None;
        let cond = self.list(&["then"])?;
        self.expect_word("then")?;
        let body = self.list(&["elif", "else", "fi"])?;
        arms.push((cond, body));
        loop {
            if self.at_word("elif") {
                self.p += 1;
                let cond = self.list(&["then"])?;
                self.expect_word("then")?;
                let body = self.list(&["elif", "else", "fi"])?;
                arms.push((cond, body));
            } else if self.at_word("else") {
                self.p += 1;
                otherwise = Some(Box::new(self.list(&["fi"])?));
                self.expect_word("fi")?;
                break;
            } else {
                self.expect_word("fi")?;
                break;
            }
        }
        Ok(Command::If { arms, otherwise })
    }

    fn while_clause(&mut self) -> Result<Command, ParseError> {
        let until = self.at_word("until");
        self.p += 1;
        let cond = self.list(&["do"])?;
        self.expect_word("do")?;
        let body = self.list(&["done"])?;
        self.expect_word("done")?;
        Ok(Command::While { cond: Box::new(cond), body: Box::new(body), until })
    }

    fn for_clause(&mut self) -> Result<Command, ParseError> {
        self.p += 1; // for
        let var = match self.bump() {
            Some(Tok::Word(w)) if is_name(&w) => w,
            Some(t) => return Err(ParseError::Syntax(format!("bad loop variable {}", describe(&t)))),
            None => return Err(ParseError::Incomplete),
        };
        self.skip_newlines();
        let mut words = None;
        if self.at_word("in") {
            self.p += 1;
            let mut list = Vec::new();
            while let Some(Tok::Word(w)) = self.peek() {
                list.push(w.clone());
                self.p += 1;
            }
            words = Some(list);
        }
        match self.peek() {
            Some(Tok::Semi) | Some(Tok::Newline) => self.p += 1,
            None => return Err(ParseError::Incomplete),
            Some(t) if !self.at_word("do") => return Err(ParseError::Syntax(format!("unexpected {}", describe(t)))),
            _ => {}
        }
        self.skip_newlines();
        self.expect_word("do")?;
        let body = self.list(&["done"])?;
        self.expect_word("done")?;
        Ok(Command::For { var, words, body: Box::new(body) })
    }

    fn case_clause(&mut self) -> Result<Command, ParseError> {
        self.p += 1; // case
        let word = match self.bump() {
            Some(Tok::Word(w)) => w,
            Some(t) => return Err(ParseError::Syntax(format!("unexpected {}", describe(&t)))),
            None => return Err(ParseError::Incomplete),
        };
        self.skip_newlines();
        self.expect_word("in")?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if self.at_word("esac") {
                self.p += 1;
                break;
            }
            if self.peek().is_none() {
                return Err(ParseError::Incomplete);
            }
            if matches!(self.peek(), Some(Tok::LParen)) {
                self.p += 1;
            }
            let mut pats = Vec::new();
            loop {
                match self.bump() {
                    Some(Tok::Word(w)) => pats.push(w),
                    Some(t) => return Err(ParseError::Syntax(format!("bad case pattern {}", describe(&t)))),
                    None => return Err(ParseError::Incomplete),
                }
                match self.peek() {
                    Some(Tok::Pipe) => self.p += 1,
                    Some(Tok::RParen) => {
                        self.p += 1;
                        break;
                    }
                    None => return Err(ParseError::Incomplete),
                    Some(t) => return Err(ParseError::Syntax(format!("unexpected {} in case pattern", describe(t)))),
                }
            }
            let body = self.list(&["esac"])?;
            if matches!(self.peek(), Some(Tok::DSemi)) {
                self.p += 1;
            }
            arms.push((pats, body));
        }
        Ok(Command::Case { word, arms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Command {
        parse(s).unwrap_or_else(|e| panic!("{s:?}: {e:?}"))
    }

    fn items(c: Command) -> Vec<Item> {
        match c {
            Command::List(i) => i,
            other => panic!("not a list: {other:?}"),
        }
    }

    fn only(s: &str) -> Command {
        let mut i = items(p(s));
        assert_eq!(i.len(), 1, "{s:?}");
        i.pop().unwrap().cmd
    }

    fn simple(c: &Command) -> &Simple {
        match c {
            Command::Simple(s) => s,
            other => panic!("not simple: {other:?}"),
        }
    }

    #[test]
    fn simple_commands_assignments_and_redirects() {
        let c = only("A=1 B=2 echo hi 'a b' >out 2>&1");
        let s = simple(&c);
        assert_eq!(s.assigns, [("A".to_string(), "1".to_string()), ("B".to_string(), "2".to_string())]);
        assert_eq!(s.words, ["echo", "hi", "'a b'"]);
        assert_eq!(s.redirs, [Redir { fd: 1, kind: RedirKind::Out("out".into()) }, Redir { fd: 2, kind: RedirKind::Dup("1".into()) }]);

        let a = only("X=5");
        assert_eq!(simple(&a).assigns.len(), 1);
        assert!(simple(&a).words.is_empty());

        let late = only("echo A=1");
        assert_eq!(simple(&late).words, ["echo", "A=1"], "assignments only count before the command word");
        assert_eq!(simple(&only("<in cat")).redirs[0].fd, 0);
    }

    #[test]
    fn lists_and_background() {
        let i = items(p("a; b & c\nd"));
        assert_eq!(i.len(), 4);
        assert!(!i[0].background && i[1].background && !i[2].background);
        assert_eq!(items(p("")).len(), 0);
        assert_eq!(items(p("\n\n")).len(), 0);
        assert!(matches!(parse("; a").unwrap_err(), ParseError::Syntax(_)), "a leading ; is a syntax error, as in other shells");
    }

    #[test]
    fn pipelines_and_and_or() {
        match only("a | b | c") {
            Command::Pipeline { negate: false, stages } => assert_eq!(stages.len(), 3),
            other => panic!("{other:?}"),
        }
        match only("! a | b") {
            Command::Pipeline { negate: true, stages } => assert_eq!(stages.len(), 2),
            other => panic!("{other:?}"),
        }
        match only("a && b || c") {
            Command::AndOr(_, rest) => assert_eq!(rest.iter().map(|r| r.0).collect::<Vec<_>>(), [true, false]),
            other => panic!("{other:?}"),
        }
        assert!(matches!(only("a &&\n b"), Command::AndOr(..)), "newline allowed after &&");
        assert!(matches!(only("a |\n b"), Command::Pipeline { .. }));
    }

    #[test]
    fn if_while_for_case() {
        match only("if a; then b; elif c; then d; else e; fi") {
            Command::If { arms, otherwise } => {
                assert_eq!(arms.len(), 2);
                assert!(otherwise.is_some());
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(only("if a\nthen\n b\nfi"), Command::If { .. }));
        assert!(matches!(only("while a; do b; done"), Command::While { until: false, .. }));
        assert!(matches!(only("until a; do b; done"), Command::While { until: true, .. }));
        match only("for x in 1 2 3; do echo $x; done") {
            Command::For { var, words, .. } => {
                assert_eq!(var, "x");
                assert_eq!(words.unwrap(), ["1", "2", "3"]);
            }
            other => panic!("{other:?}"),
        }
        match only("for x; do b; done") {
            Command::For { words, .. } => assert!(words.is_none()),
            other => panic!("{other:?}"),
        }
        match only("case $x in a|b) one;; c) two ;; *) three\nesac") {
            Command::Case { arms, .. } => {
                assert_eq!(arms.len(), 3);
                assert_eq!(arms[0].0, ["a", "b"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn groups_subshells_functions_and_compound_redirects() {
        assert!(matches!(only("{ a; b; }"), Command::Group(_)));
        assert!(matches!(only("(a; b)"), Command::Subshell(_)));
        match only("f() { echo hi; }") {
            Command::FuncDef { name, .. } => assert_eq!(name, "f"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(only("f()\n{\n echo hi\n}"), Command::FuncDef { .. }));
        match only("{ a; } > out") {
            Command::Redirected(inner, r) => {
                assert!(matches!(*inner, Command::Group(_)));
                assert_eq!(r.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        match only("while read x; do echo $x; done < file") {
            Command::Redirected(inner, _) => assert!(matches!(*inner, Command::While { .. })),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn heredoc_becomes_a_redirect() {
        match only("cat <<EOF\nhello $x\nEOF\n") {
            Command::Simple(s) => assert_eq!(s.redirs[0], Redir { fd: 0, kind: RedirKind::HereDoc { body: "hello $x\n".into(), expand: true } }),
            other => panic!("{other:?}"),
        }
        match only("cat <<'EOF'\nhello $x\nEOF\n") {
            Command::Simple(s) => assert!(matches!(&s.redirs[0].kind, RedirKind::HereDoc { expand: false, .. })),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn incomplete_versus_syntax_errors() {
        for s in ["if a", "if a; then", "if a; then b", "while a; do", "for x in 1 2", "case x in", "case x in a)", "{ a;", "(a", "a |", "a &&", "f() {", "echo 'x"] {
            assert_eq!(parse(s).unwrap_err(), ParseError::Incomplete, "{s:?}");
        }
        for s in ["fi", "done", "then", "a )", "if; then b; fi", "a | | b", "echo >", "}", "&& a", "for 1x in a; do b; done"] {
            assert!(matches!(parse(s).unwrap_err(), ParseError::Syntax(_)), "{s:?} -> {:?}", parse(s));
        }
    }
}
