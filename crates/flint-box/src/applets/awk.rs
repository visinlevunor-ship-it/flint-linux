//! awk: the POSIX language (patterns and actions, fields, associative arrays, user
//! functions, printf, getline, output redirection and pipes) plus a few common extensions
//! (`length(array)`, `delete array`, `fflush`, `nextfile`, `**`, `toupper`/`tolower`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};

use crate::regex::{self, Regex, Syntax};
use crate::util::error;

// ---------------------------------------------------------------------------------------
// Lexer

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ere(String),
    Name(String),
    /// A name directly followed by '(' : a function call.
    Func(String),
    Builtin(&'static str),
    Kw(&'static str),
    Op(&'static str),
    Newline,
    Eof,
}

const KEYWORDS: &[&str] = &["BEGIN", "END", "function", "func", "if", "else", "while", "for", "do", "break", "continue", "next", "nextfile", "exit", "return", "delete", "in", "getline", "print", "printf"];
const BUILTINS: &[&str] = &["length", "substr", "index", "split", "sub", "gsub", "match", "sprintf", "sin", "cos", "atan2", "exp", "log", "sqrt", "int", "rand", "srand", "tolower", "toupper", "system", "close", "fflush"];
const OPS: &[&str] = &["**=", "**", "+=", "-=", "*=", "/=", "%=", "^=", "==", "<=", ">=", "!=", "++", "--", "&&", "||", ">>", "!~", "{", "}", "(", ")", "[", "]", ";", ",", "+", "-", "*", "/", "%", "^", "!", ">", "<", "|", "?", ":", "~", "$", "="];

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    last: Option<Tok>,
    line: usize,
}

impl Lexer<'_> {
    fn regex_allowed(&self) -> bool {
        !matches!(&self.last, Some(Tok::Num(_) | Tok::Str(_) | Tok::Name(_) | Tok::Builtin(_)) | Some(Tok::Op(")" | "]" | "$" | "++" | "--")))
    }

    fn next(&mut self) -> Result<Tok, String> {
        let t = self.scan()?;
        self.last = Some(t.clone());
        Ok(t)
    }

    fn scan(&mut self) -> Result<Tok, String> {
        loop {
            match self.s.get(self.i) {
                Some(b' ' | b'\t' | b'\r') => self.i += 1,
                Some(b'\\') if self.s.get(self.i + 1) == Some(&b'\n') => {
                    self.i += 2;
                    self.line += 1;
                }
                Some(b'\\') if self.s.get(self.i + 1) == Some(&b'\r') && self.s.get(self.i + 2) == Some(&b'\n') => self.i += 3,
                Some(b'#') => {
                    while self.s.get(self.i).is_some_and(|&c| c != b'\n') {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
        let Some(&c) = self.s.get(self.i) else { return Ok(Tok::Eof) };
        if c == b'\n' {
            self.i += 1;
            self.line += 1;
            return Ok(Tok::Newline);
        }
        if c.is_ascii_digit() || (c == b'.' && self.s.get(self.i + 1).is_some_and(u8::is_ascii_digit)) {
            let st = self.i;
            if c == b'0' && matches!(self.s.get(self.i + 1), Some(b'x' | b'X')) && self.s.get(self.i + 2).is_some_and(u8::is_ascii_hexdigit) {
                self.i += 2;
                while self.s.get(self.i).is_some_and(u8::is_ascii_hexdigit) {
                    self.i += 1;
                }
                let v = u64::from_str_radix(std::str::from_utf8(&self.s[st + 2..self.i]).unwrap(), 16).unwrap_or(0);
                return Ok(Tok::Num(v as f64));
            }
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
                self.i += 1;
            }
            if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
                let save = self.i;
                self.i += 1;
                if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                    self.i += 1;
                }
                if self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                    while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                        self.i += 1;
                    }
                } else {
                    self.i = save;
                }
            }
            let t = std::str::from_utf8(&self.s[st..self.i]).unwrap();
            return t.parse().map(Tok::Num).map_err(|_| format!("bad number {t}"));
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let st = self.i;
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                self.i += 1;
            }
            let w = std::str::from_utf8(&self.s[st..self.i]).unwrap().to_string();
            if let Some(k) = KEYWORDS.iter().find(|k| **k == w) {
                return Ok(Tok::Kw(if *k == "func" { "function" } else { k }));
            }
            if let Some(b) = BUILTINS.iter().find(|b| **b == w) {
                return Ok(Tok::Builtin(b));
            }
            if self.s.get(self.i) == Some(&b'(') {
                return Ok(Tok::Func(w));
            }
            return Ok(Tok::Name(w));
        }
        if c == b'"' {
            self.i += 1;
            let mut out = Vec::new();
            loop {
                let Some(&c) = self.s.get(self.i) else { return Err("unterminated string".into()) };
                self.i += 1;
                match c {
                    b'"' => break,
                    b'\n' => return Err("newline in string".into()),
                    b'\\' => {
                        let Some(&n) = self.s.get(self.i) else { return Err("unterminated string".into()) };
                        self.i += 1;
                        match n {
                            b'n' => out.push(b'\n'),
                            b't' => out.push(b'\t'),
                            b'r' => out.push(b'\r'),
                            b'\\' => out.push(b'\\'),
                            b'"' => out.push(b'"'),
                            b'/' => out.push(b'/'),
                            b'a' => out.push(7),
                            b'b' => out.push(8),
                            b'f' => out.push(12),
                            b'v' => out.push(11),
                            b'\n' => {}
                            b'0'..=b'7' => {
                                let mut v = u32::from(n - b'0');
                                for _ in 0..2 {
                                    match self.s.get(self.i) {
                                        Some(&d @ b'0'..=b'7') => {
                                            v = v * 8 + u32::from(d - b'0');
                                            self.i += 1;
                                        }
                                        _ => break,
                                    }
                                }
                                out.push(v as u8);
                            }
                            other => {
                                // Unknown escapes keep the backslash (so "\." still reaches a regex as \.)
                                out.push(b'\\');
                                out.push(other);
                            }
                        }
                    }
                    _ => out.push(c),
                }
            }
            return Ok(Tok::Str(String::from_utf8_lossy(&out).into_owned()));
        }
        if c == b'/' && self.regex_allowed() {
            self.i += 1;
            let mut out = Vec::new();
            let mut in_bracket = false;
            loop {
                let Some(&c) = self.s.get(self.i) else { return Err("unterminated regex".into()) };
                self.i += 1;
                match c {
                    b'\n' => return Err("newline in regex".into()),
                    b'\\' => {
                        let n = *self.s.get(self.i).ok_or("unterminated regex")?;
                        self.i += 1;
                        if n == b'/' {
                            out.push(b'/');
                        } else {
                            out.push(b'\\');
                            out.push(n);
                        }
                    }
                    b'[' => {
                        in_bracket = true;
                        out.push(c);
                        if self.s.get(self.i) == Some(&b'^') {
                            out.push(b'^');
                            self.i += 1;
                        }
                        if self.s.get(self.i) == Some(&b']') {
                            out.push(b']');
                            self.i += 1;
                        }
                    }
                    b']' => {
                        in_bracket = false;
                        out.push(c);
                    }
                    b'/' if !in_bracket => break,
                    _ => out.push(c),
                }
            }
            return Ok(Tok::Ere(String::from_utf8_lossy(&out).into_owned()));
        }
        for op in OPS {
            if self.s[self.i..].starts_with(op.as_bytes()) {
                self.i += op.len();
                return Ok(Tok::Op(op));
            }
        }
        Err(format!("unexpected character '{}'", c as char))
    }
}

// ---------------------------------------------------------------------------------------
// Syntax tree

#[derive(Debug, Clone)]
enum Expr {
    Num(f64),
    Str(String),
    Re(usize), // index into the regex table: matches against $0
    Var(String),
    Field(Box<Expr>),
    Index(String, Vec<Expr>),
    Assign(Box<Expr>, Option<&'static str>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    In(Vec<Expr>, String),
    Match(bool, Box<Expr>, Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
    Cmp(&'static str, Box<Expr>, Box<Expr>),
    Concat(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Plus(Box<Expr>),
    Not(Box<Expr>),
    Incr(bool, i8, Box<Expr>), // prefix?, +1/-1, lvalue
    Call(String, Vec<Expr>),
    Builtin(&'static str, Vec<Expr>),
    Getline { src: GetSrc, var: Option<Box<Expr>> },
    Group(Box<Expr>),
}

#[derive(Debug, Clone)]
enum GetSrc {
    Main,
    File(Box<Expr>),
    Cmd(Box<Expr>),
}

#[derive(Debug, Clone)]
enum Redir {
    File(Expr),
    Append(Expr),
    Pipe(Expr),
}

#[derive(Debug, Clone)]
enum Stmt {
    Expr(Expr),
    Print(Vec<Expr>, Option<Redir>),
    Printf(Vec<Expr>, Option<Redir>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    While(Expr, Box<Stmt>),
    Do(Box<Stmt>, Expr),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Box<Stmt>>, Box<Stmt>),
    ForIn(String, String, Box<Stmt>),
    Block(Vec<Stmt>),
    Next,
    NextFile,
    Exit(Option<Expr>),
    Return(Option<Expr>),
    Break,
    Continue,
    Delete(String, Option<Vec<Expr>>),
    Getline(Expr),
}

#[derive(Debug, Clone)]
enum Pattern {
    Begin,
    End,
    All,
    Expr(Expr),
    Range(Expr, Expr),
}

#[derive(Debug, Clone)]
struct Item {
    pattern: Pattern,
    action: Option<Vec<Stmt>>,
}

#[derive(Debug, Clone)]
struct Func {
    params: Vec<String>,
    body: Vec<Stmt>,
}

// ---------------------------------------------------------------------------------------
// Parser

struct Parser<'a> {
    lex: Lexer<'a>,
    tok: Tok,
    regexes: Vec<String>,
    /// Inside print's expression list a bare `>` is redirection, not comparison.
    no_gt: bool,
    /// While parsing `for (x in a)` lookahead: `in` is not an operator.
    no_in: bool,
}

type PResult<T> = Result<T, String>;

impl<'a> Parser<'a> {
    fn new(src: &'a [u8]) -> PResult<Parser<'a>> {
        let mut lex = Lexer { s: src, i: 0, last: None, line: 1 };
        let tok = lex.next()?;
        Ok(Parser { lex, tok, regexes: Vec::new(), no_gt: false, no_in: false })
    }

    fn advance(&mut self) -> PResult<Tok> {
        let next = self.lex.next()?;
        Ok(std::mem::replace(&mut self.tok, next))
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(&self.tok, Tok::Op(o) if *o == op)
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(&self.tok, Tok::Kw(k) if *k == kw)
    }

    fn expect_op(&mut self, op: &str) -> PResult<()> {
        if self.is_op(op) {
            self.advance()?;
            Ok(())
        } else {
            Err(format!("line {}: expected '{op}', found {}", self.lex.line, describe(&self.tok)))
        }
    }

    fn newlines(&mut self) -> PResult<()> {
        while self.tok == Tok::Newline {
            self.advance()?;
        }
        Ok(())
    }

    fn terminators(&mut self) -> PResult<()> {
        while self.tok == Tok::Newline || self.is_op(";") {
            self.advance()?;
        }
        Ok(())
    }

    fn program(&mut self) -> PResult<(Vec<Item>, HashMap<String, Func>)> {
        let mut items = Vec::new();
        let mut funcs = HashMap::new();
        self.terminators()?;
        while self.tok != Tok::Eof {
            if self.is_kw("function") {
                self.advance()?;
                let name = match self.advance()? {
                    Tok::Name(n) | Tok::Func(n) => n,
                    t => return Err(format!("bad function name {}", describe(&t))),
                };
                self.expect_op("(")?;
                let mut params = Vec::new();
                while !self.is_op(")") {
                    match self.advance()? {
                        Tok::Name(n) => params.push(n),
                        t => return Err(format!("bad parameter {}", describe(&t))),
                    }
                    if self.is_op(",") {
                        self.advance()?;
                        self.newlines()?;
                    }
                }
                self.advance()?;
                self.newlines()?;
                let body = self.block()?;
                if funcs.insert(name.clone(), Func { params, body }).is_some() {
                    return Err(format!("function {name} defined twice"));
                }
            } else {
                let pattern = if self.is_kw("BEGIN") {
                    self.advance()?;
                    Pattern::Begin
                } else if self.is_kw("END") {
                    self.advance()?;
                    Pattern::End
                } else if self.is_op("{") {
                    Pattern::All
                } else {
                    let a = self.expr()?;
                    if self.is_op(",") {
                        self.advance()?;
                        self.newlines()?;
                        Pattern::Range(a, self.expr()?)
                    } else {
                        Pattern::Expr(a)
                    }
                };
                let action = if self.is_op("{") {
                    Some(self.block()?)
                } else {
                    if matches!(pattern, Pattern::Begin | Pattern::End) {
                        return Err("BEGIN and END need an action".into());
                    }
                    None
                };
                items.push(Item { pattern, action });
            }
            self.terminators()?;
        }
        Ok((items, funcs))
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_op("{")?;
        let mut v = Vec::new();
        self.terminators()?;
        while !self.is_op("}") {
            if self.tok == Tok::Eof {
                return Err("missing '}'".into());
            }
            v.push(self.stmt()?);
            self.terminators()?;
        }
        self.advance()?;
        Ok(v)
    }

    /// A statement as the body of if/while/for: may be on the next line, may be `;`.
    fn body(&mut self) -> PResult<Stmt> {
        self.newlines()?;
        if self.is_op(";") {
            self.advance()?;
            return Ok(Stmt::Block(Vec::new()));
        }
        self.stmt()
    }

    fn end_simple(&mut self) -> PResult<()> {
        if self.is_op(";") || self.tok == Tok::Newline {
            self.advance()?;
        } else if !self.is_op("}") && self.tok != Tok::Eof {
            return Err(format!("line {}: unexpected {}", self.lex.line, describe(&self.tok)));
        }
        Ok(())
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        if self.is_op("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        if self.is_kw("if") {
            self.advance()?;
            self.expect_op("(")?;
            let c = self.expr()?;
            self.expect_op(")")?;
            let then = self.body()?;
            // `else` may follow after newlines or a semicolon.
            let save_i = self.lex.i;
            let save_tok = self.tok.clone();
            let save_last = self.lex.last.clone();
            let save_line = self.lex.line;
            self.terminators()?;
            if self.is_kw("else") {
                self.advance()?;
                let e = self.body()?;
                return Ok(Stmt::If(c, Box::new(then), Some(Box::new(e))));
            }
            self.lex.i = save_i;
            self.tok = save_tok;
            self.lex.last = save_last;
            self.lex.line = save_line;
            return Ok(Stmt::If(c, Box::new(then), None));
        }
        if self.is_kw("while") {
            self.advance()?;
            self.expect_op("(")?;
            let c = self.expr()?;
            self.expect_op(")")?;
            if self.is_op(";") {
                self.advance()?;
                return Ok(Stmt::While(c, Box::new(Stmt::Block(Vec::new()))));
            }
            return Ok(Stmt::While(c, Box::new(self.body()?)));
        }
        if self.is_kw("do") {
            self.advance()?;
            let b = self.body()?;
            self.terminators()?;
            if !self.is_kw("while") {
                return Err("do without while".into());
            }
            self.advance()?;
            self.expect_op("(")?;
            let c = self.expr()?;
            self.expect_op(")")?;
            self.end_simple()?;
            return Ok(Stmt::Do(Box::new(b), c));
        }
        if self.is_kw("for") {
            self.advance()?;
            self.expect_op("(")?;
            // for (name in array)
            if let Tok::Name(n) = self.tok.clone() {
                let save = (self.lex.i, self.tok.clone(), self.lex.last.clone(), self.lex.line);
                self.advance()?;
                if self.is_kw("in") {
                    self.advance()?;
                    if let Tok::Name(arr) = self.tok.clone() {
                        self.advance()?;
                        if self.is_op(")") {
                            self.advance()?;
                            return Ok(Stmt::ForIn(n, arr, Box::new(self.body()?)));
                        }
                    }
                }
                self.lex.i = save.0;
                self.tok = save.1;
                self.lex.last = save.2;
                self.lex.line = save.3;
            }
            let init = if self.is_op(";") { None } else { Some(Box::new(self.simple_stmt()?)) };
            self.expect_op(";")?;
            self.newlines()?;
            let cond = if self.is_op(";") { None } else { Some(self.expr()?) };
            self.expect_op(";")?;
            self.newlines()?;
            let step = if self.is_op(")") { None } else { Some(Box::new(self.simple_stmt()?)) };
            self.expect_op(")")?;
            if self.is_op(";") {
                self.advance()?;
                return Ok(Stmt::For(init, cond, step, Box::new(Stmt::Block(Vec::new()))));
            }
            return Ok(Stmt::For(init, cond, step, Box::new(self.body()?)));
        }
        let s = match &self.tok {
            Tok::Kw("next") => {
                self.advance()?;
                Stmt::Next
            }
            Tok::Kw("nextfile") => {
                self.advance()?;
                Stmt::NextFile
            }
            Tok::Kw("break") => {
                self.advance()?;
                Stmt::Break
            }
            Tok::Kw("continue") => {
                self.advance()?;
                Stmt::Continue
            }
            Tok::Kw("exit") => {
                self.advance()?;
                let e = if self.ends_stmt() { None } else { Some(self.expr()?) };
                Stmt::Exit(e)
            }
            Tok::Kw("return") => {
                self.advance()?;
                let e = if self.ends_stmt() { None } else { Some(self.expr()?) };
                Stmt::Return(e)
            }
            _ => self.simple_stmt()?,
        };
        self.end_simple()?;
        Ok(s)
    }

    fn ends_stmt(&self) -> bool {
        self.tok == Tok::Newline || self.is_op(";") || self.is_op("}") || self.tok == Tok::Eof
    }

    fn simple_stmt(&mut self) -> PResult<Stmt> {
        if self.is_kw("print") || self.is_kw("printf") {
            let is_printf = self.is_kw("printf");
            self.advance()?;
            let mut args = Vec::new();
            if !self.ends_stmt() && !self.is_op(">") && !self.is_op(">>") && !self.is_op("|") {
                // print (a, b) > "file": a parenthesized list
                self.no_gt = true;
                let r = (|| -> PResult<()> {
                    args.push(self.expr()?);
                    while self.is_op(",") {
                        self.advance()?;
                        self.newlines()?;
                        args.push(self.expr()?);
                    }
                    Ok(())
                })();
                self.no_gt = false;
                r?;
                // A grouping (a,b) parsed as a list: unwrap.
                if args.len() == 1 {
                    if let Expr::Call(n, list) = &args[0] {
                        if n == "\0list" {
                            args = list.clone();
                        }
                    }
                }
            }
            let redir = if self.is_op(">") {
                self.advance()?;
                Some(Redir::File(self.concat_no_gt()?))
            } else if self.is_op(">>") {
                self.advance()?;
                Some(Redir::Append(self.concat_no_gt()?))
            } else if self.is_op("|") {
                self.advance()?;
                Some(Redir::Pipe(self.concat_no_gt()?))
            } else {
                None
            };
            if is_printf && args.is_empty() {
                return Err("printf: no format".into());
            }
            return Ok(if is_printf { Stmt::Printf(args, redir) } else { Stmt::Print(args, redir) });
        }
        if self.is_kw("delete") {
            self.advance()?;
            let Tok::Name(n) = self.advance()? else { return Err("delete needs an array".into()) };
            if self.is_op("[") {
                self.advance()?;
                let subs = self.expr_list("]")?;
                return Ok(Stmt::Delete(n, Some(subs)));
            }
            return Ok(Stmt::Delete(n, None));
        }
        let e = self.expr()?;
        if let Expr::Getline { .. } = e {
            return Ok(Stmt::Getline(e));
        }
        Ok(Stmt::Expr(e))
    }

    fn concat_no_gt(&mut self) -> PResult<Expr> {
        let save = self.no_gt;
        self.no_gt = true;
        let r = self.concat();
        self.no_gt = save;
        r
    }

    fn expr_list(&mut self, close: &str) -> PResult<Vec<Expr>> {
        let mut v = Vec::new();
        self.newlines()?;
        if self.is_op(close) {
            self.advance()?;
            return Ok(v);
        }
        loop {
            v.push(self.expr()?);
            self.newlines()?;
            if self.is_op(",") {
                self.advance()?;
                self.newlines()?;
                continue;
            }
            self.expect_op(close)?;
            return Ok(v);
        }
    }

    fn expr(&mut self) -> PResult<Expr> {
        self.ternary()
    }

    fn ternary(&mut self) -> PResult<Expr> {
        let c = self.or()?;
        if self.is_op("?") {
            self.advance()?;
            self.newlines()?;
            let a = self.ternary()?;
            self.newlines()?;
            self.expect_op(":")?;
            self.newlines()?;
            let b = self.ternary()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        // Assignment (right associative); the left side must be an lvalue.
        if let Tok::Op(op @ ("=" | "+=" | "-=" | "*=" | "/=" | "%=" | "^=" | "**=")) = self.tok {
            if matches!(c, Expr::Var(_) | Expr::Field(_) | Expr::Index(..)) {
                self.advance()?;
                self.newlines()?;
                let rhs = self.ternary()?;
                let bin = match op {
                    "=" => None,
                    "+=" => Some("+"),
                    "-=" => Some("-"),
                    "*=" => Some("*"),
                    "/=" => Some("/"),
                    "%=" => Some("%"),
                    _ => Some("^"),
                };
                return Ok(Expr::Assign(Box::new(c), bin, Box::new(rhs)));
            }
        }
        Ok(c)
    }

    fn or(&mut self) -> PResult<Expr> {
        let mut l = self.and()?;
        while self.is_op("||") {
            self.advance()?;
            self.newlines()?;
            let r = self.and()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and(&mut self) -> PResult<Expr> {
        let mut l = self.in_expr()?;
        while self.is_op("&&") {
            self.advance()?;
            self.newlines()?;
            let r = self.in_expr()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn in_expr(&mut self) -> PResult<Expr> {
        let mut l = self.matching()?;
        while self.is_kw("in") && !self.no_in {
            self.advance()?;
            let Tok::Name(arr) = self.advance()? else { return Err("'in' needs an array name".into()) };
            let subs = match l {
                Expr::Call(ref n, ref list) if n == "\0list" => list.clone(),
                other => vec![other],
            };
            l = Expr::In(subs, arr);
        }
        Ok(l)
    }

    fn matching(&mut self) -> PResult<Expr> {
        let mut l = self.comparison()?;
        loop {
            let neg = if self.is_op("~") {
                false
            } else if self.is_op("!~") {
                true
            } else {
                return Ok(l);
            };
            self.advance()?;
            let r = self.comparison()?;
            l = Expr::Match(neg, Box::new(l), Box::new(r));
        }
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let mut l = self.concat()?;
        loop {
            let op = match &self.tok {
                Tok::Op(o @ ("<" | "<=" | "!=" | "==" | ">=")) => *o,
                Tok::Op(">") if !self.no_gt => ">",
                Tok::Op("|") => {
                    // cmd | getline [var]
                    let save = (self.lex.i, self.tok.clone(), self.lex.last.clone(), self.lex.line);
                    self.advance()?;
                    if self.is_kw("getline") {
                        self.advance()?;
                        let var = self.opt_lvalue()?;
                        l = Expr::Getline { src: GetSrc::Cmd(Box::new(l)), var };
                        continue;
                    }
                    self.lex.i = save.0;
                    self.tok = save.1;
                    self.lex.last = save.2;
                    self.lex.line = save.3;
                    return Ok(l);
                }
                _ => return Ok(l),
            };
            self.advance()?;
            let r = self.concat()?;
            l = Expr::Cmp(op, Box::new(l), Box::new(r));
        }
    }

    fn starts_operand(&self) -> bool {
        match &self.tok {
            Tok::Num(_) | Tok::Str(_) | Tok::Ere(_) | Tok::Name(_) | Tok::Func(_) | Tok::Builtin(_) => true,
            Tok::Op(o) => matches!(*o, "$" | "!" | "(" | "-" | "+" | "++" | "--"),
            Tok::Kw("getline") => true,
            _ => false,
        }
    }

    fn concat(&mut self) -> PResult<Expr> {
        let mut l = self.additive()?;
        while self.starts_operand() && !matches!(self.tok, Tok::Op("-" | "+" | "!")) && !(self.is_kw("in")) {
            if self.is_kw("getline") {
                // `x getline` is not a thing; stop
                break;
            }
            let r = self.additive()?;
            l = Expr::Concat(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut l = self.multiplicative()?;
        loop {
            let op = match &self.tok {
                Tok::Op("+") => "+",
                Tok::Op("-") => "-",
                _ => return Ok(l),
            };
            self.advance()?;
            let r = self.multiplicative()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn multiplicative(&mut self) -> PResult<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = match &self.tok {
                Tok::Op("*") => "*",
                Tok::Op("/") => "/",
                Tok::Op("%") => "%",
                _ => return Ok(l),
            };
            self.advance()?;
            let r = self.unary()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        match &self.tok {
            Tok::Op("!") => {
                self.advance()?;
                Ok(Expr::Not(Box::new(self.unary()?)))
            }
            Tok::Op("-") => {
                self.advance()?;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Tok::Op("+") => {
                self.advance()?;
                Ok(Expr::Plus(Box::new(self.unary()?)))
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> PResult<Expr> {
        let base = self.postfix()?;
        if self.is_op("^") || self.is_op("**") {
            self.advance()?;
            // right associative, and binds tighter than unary minus on its left only
            let exp = if self.is_op("-") {
                self.advance()?;
                Expr::Neg(Box::new(self.power_operand()?))
            } else {
                self.power_operand()?
            };
            return Ok(Expr::Bin("^", Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn power_operand(&mut self) -> PResult<Expr> {
        let b = self.postfix()?;
        if self.is_op("^") || self.is_op("**") {
            self.advance()?;
            let e = self.power_operand()?;
            return Ok(Expr::Bin("^", Box::new(b), Box::new(e)));
        }
        Ok(b)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        if self.is_op("++") || self.is_op("--") {
            let d = if self.is_op("++") { 1 } else { -1 };
            self.advance()?;
            let lv = self.postfix()?;
            if !matches!(lv, Expr::Var(_) | Expr::Field(_) | Expr::Index(..)) {
                return Err("++ or -- needs a variable".into());
            }
            return Ok(Expr::Incr(true, d, Box::new(lv)));
        }
        let e = self.primary()?;
        if matches!(e, Expr::Var(_) | Expr::Field(_) | Expr::Index(..)) && (self.is_op("++") || self.is_op("--")) {
            let d = if self.is_op("++") { 1 } else { -1 };
            self.advance()?;
            return Ok(Expr::Incr(false, d, Box::new(e)));
        }
        Ok(e)
    }

    fn opt_lvalue(&mut self) -> PResult<Option<Box<Expr>>> {
        match &self.tok {
            Tok::Name(_) | Tok::Op("$") => {
                let e = self.primary_lvalue()?;
                Ok(Some(Box::new(e)))
            }
            _ => Ok(None),
        }
    }

    fn primary_lvalue(&mut self) -> PResult<Expr> {
        if self.is_op("$") {
            self.advance()?;
            let e = self.primary_for_field()?;
            return Ok(Expr::Field(Box::new(e)));
        }
        let Tok::Name(n) = self.advance()? else { return Err("expected a variable".into()) };
        if self.is_op("[") {
            self.advance()?;
            let subs = self.expr_list("]")?;
            return Ok(Expr::Index(n, subs));
        }
        Ok(Expr::Var(n))
    }

    /// `$` binds tighter than everything but grouping and ++/--: `$i++` is ($i)++, `$NF-1` is ($NF)-1.
    fn primary_for_field(&mut self) -> PResult<Expr> {
        if self.is_op("++") || self.is_op("--") {
            let d = if self.is_op("++") { 1 } else { -1 };
            self.advance()?;
            let lv = self.primary_for_field()?;
            return Ok(Expr::Incr(true, d, Box::new(lv)));
        }
        if self.is_op("-") {
            self.advance()?;
            return Ok(Expr::Neg(Box::new(self.primary_for_field()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> PResult<Expr> {
        let t = self.advance()?;
        match t {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Ere(r) => {
                self.regexes.push(r);
                Ok(Expr::Re(self.regexes.len() - 1))
            }
            Tok::Op("$") => {
                let e = self.primary_for_field()?;
                Ok(Expr::Field(Box::new(e)))
            }
            Tok::Op("(") => {
                self.newlines()?;
                let save_gt = self.no_gt;
                let save_in = self.no_in;
                self.no_gt = false;
                self.no_in = false;
                let first = self.expr()?;
                self.newlines()?;
                let r = if self.is_op(",") {
                    let mut list = vec![first];
                    while self.is_op(",") {
                        self.advance()?;
                        self.newlines()?;
                        list.push(self.expr()?);
                        self.newlines()?;
                    }
                    self.expect_op(")")?;
                    // (a, b) in arr, or print (a, b)
                    Expr::Call("\0list".into(), list)
                } else {
                    self.expect_op(")")?;
                    Expr::Group(Box::new(first))
                };
                self.no_gt = save_gt;
                self.no_in = save_in;
                Ok(r)
            }
            Tok::Name(n) => {
                if self.is_op("[") {
                    self.advance()?;
                    let subs = self.expr_list("]")?;
                    return Ok(Expr::Index(n, subs));
                }
                Ok(Expr::Var(n))
            }
            Tok::Func(n) => {
                self.expect_op("(")?;
                let args = self.expr_list(")")?;
                Ok(Expr::Call(n, args))
            }
            Tok::Builtin(b) => {
                if self.is_op("(") {
                    self.advance()?;
                    let args = self.expr_list(")")?;
                    return Ok(Expr::Builtin(b, args));
                }
                if b == "length" {
                    return Ok(Expr::Builtin("length", Vec::new()));
                }
                Err(format!("{b} needs arguments in parentheses"))
            }
            Tok::Kw("getline") => {
                let var = self.opt_lvalue()?;
                if self.is_op("<") {
                    self.advance()?;
                    let f = self.primary()?;
                    return Ok(Expr::Getline { src: GetSrc::File(Box::new(f)), var });
                }
                Ok(Expr::Getline { src: GetSrc::Main, var })
            }
            Tok::Op("-") => Ok(Expr::Neg(Box::new(self.unary()?))),
            Tok::Op("+") => Ok(Expr::Plus(Box::new(self.unary()?))),
            Tok::Op("!") => Ok(Expr::Not(Box::new(self.unary()?))),
            t => Err(format!("line {}: syntax error at {}", self.lex.line, describe(&t))),
        }
    }
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Num(n) => format!("number {n}"),
        Tok::Str(s) => format!("string \"{s}\""),
        Tok::Ere(r) => format!("regex /{r}/"),
        Tok::Name(n) | Tok::Func(n) => format!("'{n}'"),
        Tok::Builtin(b) | Tok::Kw(b) | Tok::Op(b) => format!("'{b}'"),
        Tok::Newline => "end of line".into(),
        Tok::Eof => "end of program".into(),
    }
}

// ---------------------------------------------------------------------------------------
// Values

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Uninit,
    Num(f64),
    Str(String),
    /// From input: a string that compares as a number if it looks like one.
    StrNum(String),
}

pub fn looks_numeric(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c| c == ' ' || c == '\t' || c == '\n');
    if t.is_empty() {
        return None;
    }
    let b = t.as_bytes();
    let mut i = 0;
    if matches!(b[0], b'+' | b'-') {
        i = 1;
    }
    let rest = &t[i..];
    if rest.eq_ignore_ascii_case("inf") || rest.eq_ignore_ascii_case("nan") || rest.eq_ignore_ascii_case("infinity") {
        return None; // POSIX awks treat these as strings from input
    }
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let save = i;
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let st = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == st {
            i = save;
        }
    }
    if i != b.len() {
        return None;
    }
    t.parse().ok()
}

/// The numeric value of a string: its longest numeric prefix (as strtod), else 0.
pub fn str_to_num(s: &str) -> f64 {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C']);
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if t.len() >= i + 3 && (t[i..i + 3].eq_ignore_ascii_case("inf") || t[i..i + 3].eq_ignore_ascii_case("nan")) {
        // "+inf" / "-nan" are numbers in arithmetic contexts
        if i == 1 {
            let v = if t[i..i + 3].eq_ignore_ascii_case("inf") { f64::INFINITY } else { f64::NAN };
            return if b[0] == b'-' { -v } else { v };
        }
        return 0.0;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let save = i;
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let st = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == st {
            i = save;
        }
    }
    t[..i].parse().unwrap_or(0.0)
}

pub fn num_to_str(n: f64, fmt: &str) -> String {
    // Integral values print as integers (as POSIX awk and gawk do), up to i64's range.
    if n == n.trunc() && n.abs() < 9.2e18 {
        return format!("{}", n as i64);
    }
    if n.is_nan() {
        return if n.is_sign_negative() { "-nan".into() } else { "nan".into() };
    }
    if n.is_infinite() {
        return if n < 0.0 { "-inf".into() } else { "inf".into() };
    }
    sprintf(fmt, &[Value::Num(n)], "%.6g")
}

impl Value {
    fn num(&self) -> f64 {
        match self {
            Value::Uninit => 0.0,
            Value::Num(n) => *n,
            Value::Str(s) | Value::StrNum(s) => str_to_num(s),
        }
    }

    fn string(&self, convfmt: &str) -> String {
        match self {
            Value::Uninit => String::new(),
            Value::Num(n) => num_to_str(*n, convfmt),
            Value::Str(s) | Value::StrNum(s) => s.clone(),
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Value::Uninit => false,
            Value::Num(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::StrNum(s) => match looks_numeric(s) {
                Some(n) => n != 0.0,
                None => !s.is_empty(),
            },
        }
    }

    /// Numeric for comparison: numbers, uninitialized values, numeric-looking input.
    fn cmp_num(&self) -> Option<f64> {
        match self {
            Value::Uninit => Some(0.0),
            Value::Num(n) => Some(*n),
            Value::StrNum(s) => looks_numeric(s),
            Value::Str(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------------------
// printf

fn c_exp(v: f64, prec: usize, upper: bool) -> String {
    let s = format!("{:.*e}", prec, v);
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    let r = format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    if upper {
        r.to_uppercase()
    } else {
        r
    }
}

fn c_general(v: f64, prec: usize, upper: bool, alt: bool) -> String {
    let p = if prec == 0 { 1 } else { prec };
    if v == 0.0 {
        return if alt { format!("{:.*}", p - 1, 0.0) } else { "0".into() };
    }
    let probe = format!("{:.*e}", p - 1, v);
    let x: i32 = probe.split_once('e').map_or(0, |(_, e)| e.parse().unwrap_or(0));
    let mut s = if x < -4 || x >= p as i32 { c_exp(v, p - 1, upper) } else { format!("{:.*}", (p as i32 - 1 - x).max(0) as usize, v) };
    if !alt {
        // strip trailing zeros of the fraction
        let (mant, exp) = match s.find(['e', 'E']) {
            Some(k) => (s[..k].to_string(), s[k..].to_string()),
            None => (s.clone(), String::new()),
        };
        let mant = if mant.contains('.') { mant.trim_end_matches('0').trim_end_matches('.').to_string() } else { mant };
        s = mant + &exp;
    }
    s
}

pub fn sprintf(fmt: &str, args: &[Value], convfmt: &str) -> String {
    let mut out = String::new();
    let f: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    let mut ai = 0;
    let mut next_arg = || -> Value {
        let v = args.get(ai).cloned().unwrap_or(Value::Uninit);
        ai += 1;
        v
    };
    while i < f.len() {
        if f[i] != '%' {
            out.push(f[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i < f.len() && f[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        let (mut left, mut plus, mut space, mut alt, mut zero) = (false, false, false, false, false);
        while i < f.len() && "-+ #0".contains(f[i]) {
            match f[i] {
                '-' => left = true,
                '+' => plus = true,
                ' ' => space = true,
                '#' => alt = true,
                _ => zero = true,
            }
            i += 1;
        }
        let mut width: usize = 0;
        if i < f.len() && f[i] == '*' {
            let w = next_arg().num() as i64;
            if w < 0 {
                left = true;
            }
            width = w.unsigned_abs() as usize;
            i += 1;
        } else {
            while i < f.len() && f[i].is_ascii_digit() {
                width = width * 10 + f[i].to_digit(10).unwrap() as usize;
                i += 1;
            }
        }
        let mut prec: Option<usize> = None;
        if i < f.len() && f[i] == '.' {
            i += 1;
            let mut p = 0usize;
            if i < f.len() && f[i] == '*' {
                p = next_arg().num().max(0.0) as usize;
                i += 1;
            } else {
                while i < f.len() && f[i].is_ascii_digit() {
                    p = p * 10 + f[i].to_digit(10).unwrap() as usize;
                    i += 1;
                }
            }
            prec = Some(p);
        }
        // length modifiers are accepted and ignored
        while i < f.len() && "hlLqjzt".contains(f[i]) {
            i += 1;
        }
        if i >= f.len() {
            out.push('%');
            break;
        }
        let conv = f[i];
        i += 1;
        let sign = |neg: bool| if neg { "-" } else if plus { "+" } else if space { " " } else { "" };
        let body: (String, String, bool) = match conv {
            'd' | 'i' => {
                let v = next_arg().num();
                let n = if v.is_finite() { v.trunc() } else { 0.0 };
                let mut digits = format!("{}", (n.abs()) as u128);
                if let Some(p) = prec {
                    if p == 0 && n == 0.0 {
                        digits.clear();
                    }
                    while digits.len() < p {
                        digits.insert(0, '0');
                    }
                }
                (sign(n < 0.0).to_string(), digits, prec.is_none())
            }
            'o' | 'x' | 'X' | 'u' => {
                let v = next_arg().num();
                let n = if v < 0.0 { (v as i64) as u64 } else { v as u64 };
                let mut digits = match conv {
                    'o' => format!("{n:o}"),
                    'x' => format!("{n:x}"),
                    'X' => format!("{n:X}"),
                    _ => format!("{n}"),
                };
                if let Some(p) = prec {
                    while digits.len() < p {
                        digits.insert(0, '0');
                    }
                }
                let pre = if alt && n != 0 {
                    match conv {
                        'x' => "0x",
                        'X' => "0X",
                        'o' if !digits.starts_with('0') => "0",
                        _ => "",
                    }
                } else {
                    ""
                };
                (pre.to_string(), digits, prec.is_none())
            }
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
                let v = next_arg().num();
                let p = prec.unwrap_or(6);
                let neg = v.is_sign_negative() && !(v == 0.0 && !v.is_sign_negative());
                let a = v.abs();
                let digits = if a.is_nan() {
                    "nan".to_string()
                } else if a.is_infinite() {
                    "inf".to_string()
                } else {
                    match conv {
                        'e' | 'E' => c_exp(a, p, conv == 'E'),
                        'f' | 'F' => {
                            let s = format!("{:.*}", p, a);
                            if alt && p == 0 {
                                s + "."
                            } else {
                                s
                            }
                        }
                        _ => c_general(a, p, conv == 'G', alt),
                    }
                };
                let digits = if conv.is_uppercase() { digits.to_uppercase() } else { digits };
                (sign(neg && v != 0.0 || (v == 0.0 && v.is_sign_negative())).to_string(), digits, a.is_finite())
            }
            'c' => {
                let v = next_arg();
                let s = match v {
                    Value::Num(n) => char::from_u32(n as u32).map(String::from).unwrap_or_default(),
                    other => other.string(convfmt).chars().next().map(String::from).unwrap_or_default(),
                };
                (String::new(), s, false)
            }
            's' => {
                let mut s = next_arg().string(convfmt);
                if let Some(p) = prec {
                    s = s.chars().take(p).collect();
                }
                (String::new(), s, false)
            }
            other => {
                out.push('%');
                out.push(other);
                continue;
            }
        };
        let (pre, digits, zero_ok) = body;
        let len = pre.chars().count() + digits.chars().count();
        if len >= width {
            out.push_str(&pre);
            out.push_str(&digits);
        } else if left {
            out.push_str(&pre);
            out.push_str(&digits);
            out.extend(std::iter::repeat(' ').take(width - len));
        } else if zero && zero_ok {
            out.push_str(&pre);
            out.extend(std::iter::repeat('0').take(width - len));
            out.push_str(&digits);
        } else {
            out.extend(std::iter::repeat(' ').take(width - len));
            out.push_str(&pre);
            out.push_str(&digits);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Interpreter

#[derive(Debug, Clone)]
enum Var {
    Scalar(Value),
    Array(usize),
}

enum Ctl {
    Normal,
    Next,
    NextFile,
    Exit,
    Return(Value),
    Break,
    Continue,
}

struct Reader {
    r: Box<dyn BufRead>,
    eof: bool,
    child: Option<Child>,
}

enum Out {
    File(io::BufWriter<fs::File>),
    Pipe(Child),
}

struct Interp {
    globals: HashMap<String, Var>,
    frames: Vec<HashMap<String, Var>>,
    arrays: Vec<HashMap<String, Value>>,
    funcs: HashMap<String, Func>,
    /// For each function, the parameters it uses as arrays.
    array_params: HashMap<String, HashSet<String>>,
    regexes: Vec<Regex>,
    dyn_re: HashMap<String, Regex>,
    fields: Vec<String>,
    record: String,
    fields_valid: bool,
    record_valid: bool,
    nf: usize,
    out: io::BufWriter<io::Stdout>,
    outputs: HashMap<String, Out>,
    readers: HashMap<String, Reader>,
    main: Option<MainInput>,
    exit_code: i32,
    range_active: Vec<bool>,
    rand_state: u64,
    prev_seed: f64,
}

struct MainInput {
    args: Vec<String>,
    next_arg: usize,
    cur: Option<Box<dyn BufRead>>,
    used_stdin: bool,
}

fn rt_err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg.into())
}

type R<T> = io::Result<T>;

fn uses_as_array(stmts: &[Stmt], name: &str) -> bool {
    fn e(x: &Expr, n: &str) -> bool {
        match x {
            Expr::Index(a, subs) => a == n || subs.iter().any(|s| e(s, n)),
            Expr::In(subs, a) => a == n || subs.iter().any(|s| e(s, n)),
            Expr::Builtin("split", args) => matches!(args.get(1), Some(Expr::Var(v)) if v == n) || args.iter().any(|a| e(a, n)),
            Expr::Field(a) | Expr::Neg(a) | Expr::Plus(a) | Expr::Not(a) | Expr::Group(a) | Expr::Incr(_, _, a) => e(a, n),
            Expr::Assign(a, _, b) | Expr::And(a, b) | Expr::Or(a, b) | Expr::Match(_, a, b) | Expr::Bin(_, a, b) | Expr::Cmp(_, a, b) | Expr::Concat(a, b) => e(a, n) || e(b, n),
            Expr::Cond(a, b, c) => e(a, n) || e(b, n) || e(c, n),
            Expr::Call(_, args) | Expr::Builtin(_, args) => args.iter().any(|a| e(a, n)),
            Expr::Getline { src, var } => {
                var.as_ref().is_some_and(|v| e(v, n))
                    || match src {
                        GetSrc::File(f) | GetSrc::Cmd(f) => e(f, n),
                        GetSrc::Main => false,
                    }
            }
            _ => false,
        }
    }
    fn s(x: &Stmt, n: &str) -> bool {
        match x {
            Stmt::Expr(a) | Stmt::Getline(a) => e(a, n),
            Stmt::Print(a, _) | Stmt::Printf(a, _) => a.iter().any(|x| e(x, n)),
            Stmt::If(c, a, b) => e(c, n) || s(a, n) || b.as_ref().is_some_and(|b| s(b, n)),
            Stmt::While(c, b) | Stmt::Do(b, c) => e(c, n) || s(b, n),
            Stmt::For(a, c, b, body) => a.as_ref().is_some_and(|a| s(a, n)) || c.as_ref().is_some_and(|c| e(c, n)) || b.as_ref().is_some_and(|b| s(b, n)) || s(body, n),
            Stmt::ForIn(_, a, b) => a == n || s(b, n),
            Stmt::Block(v) => v.iter().any(|x| s(x, n)),
            Stmt::Delete(a, _) => a == n,
            Stmt::Exit(Some(a)) | Stmt::Return(Some(a)) => e(a, n),
            _ => false,
        }
    }
    stmts.iter().any(|x| s(x, name))
}

impl Interp {
    fn var_str(&self, name: &str) -> String {
        match self.globals.get(name) {
            Some(Var::Scalar(v)) => v.string("%.6g"),
            _ => String::new(),
        }
    }

    fn convfmt(&self) -> String {
        let c = self.var_str("CONVFMT");
        if c.is_empty() {
            "%.6g".into()
        } else {
            c
        }
    }

    fn to_s(&self, v: &Value) -> String {
        match v {
            Value::Num(n) => num_to_str(*n, &self.convfmt()),
            other => other.string("%.6g"),
        }
    }

    fn out_s(&self, v: &Value) -> String {
        match v {
            Value::Num(n) => {
                let f = self.var_str("OFMT");
                num_to_str(*n, if f.is_empty() { "%.6g" } else { &f })
            }
            other => other.string("%.6g"),
        }
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        if let Some(f) = self.frames.last() {
            if let Some(v) = f.get(name) {
                return Some(v);
            }
        }
        self.globals.get(name)
    }

    fn is_local(&self, name: &str) -> bool {
        self.frames.last().is_some_and(|f| f.contains_key(name))
    }

    fn set_var(&mut self, name: &str, v: Value) -> R<()> {
        let local = self.is_local(name);
        let slot = if local { self.frames.last_mut().unwrap().get_mut(name).unwrap() } else { self.globals.entry(name.to_string()).or_insert(Var::Scalar(Value::Uninit)) };
        if let Var::Array(_) = slot {
            return Err(rt_err(format!("can't use array {name} as a scalar")));
        }
        *slot = Var::Scalar(v);
        if !local && name == "NF" {
            self.set_nf()?;
        }
        Ok(())
    }

    fn get_var(&mut self, name: &str) -> R<Value> {
        if name == "NF" && !self.is_local(name) {
            self.split_fields();
            return Ok(Value::Num(self.nf as f64));
        }
        match self.lookup(name) {
            Some(Var::Scalar(v)) => Ok(v.clone()),
            Some(Var::Array(_)) => Err(rt_err(format!("can't use array {name} in a scalar context"))),
            None => Ok(Value::Uninit),
        }
    }

    fn array_id(&mut self, name: &str) -> R<usize> {
        let local = self.is_local(name);
        let n_arrays = self.arrays.len();
        let slot = if local { self.frames.last_mut().unwrap().get_mut(name).unwrap() } else { self.globals.entry(name.to_string()).or_insert(Var::Scalar(Value::Uninit)) };
        match slot {
            Var::Array(id) => Ok(*id),
            Var::Scalar(Value::Uninit) => {
                *slot = Var::Array(n_arrays);
                self.arrays.push(HashMap::new());
                Ok(n_arrays)
            }
            Var::Scalar(_) => Err(rt_err(format!("can't use scalar {name} as an array"))),
        }
    }

    fn subscript(&mut self, subs: &[Expr]) -> R<String> {
        if subs.len() == 1 {
            let v = self.eval(&subs[0])?;
            return Ok(self.to_s(&v));
        }
        let sep = self.var_str("SUBSEP");
        let mut parts = Vec::new();
        for s in subs {
            let v = self.eval(s)?;
            parts.push(self.to_s(&v));
        }
        Ok(parts.join(&sep))
    }

    // --- records and fields

    fn set_record(&mut self, s: String) {
        self.record = s;
        self.record_valid = true;
        self.fields_valid = false;
    }

    fn split_fields(&mut self) {
        if self.fields_valid {
            return;
        }
        let fs = self.var_str("FS");
        let rs = self.var_str("RS");
        let mut f = self.split_str(&self.record.clone(), &fs);
        if rs.is_empty() && fs != " " {
            // paragraph mode: newline always separates fields
            f = f.into_iter().flat_map(|x| x.split('\n').map(String::from).collect::<Vec<_>>()).collect();
        }
        self.nf = f.len();
        self.fields = f;
        self.fields_valid = true;
    }

    fn split_str(&mut self, s: &str, fs: &str) -> Vec<String> {
        if fs == " " {
            return s.split([' ', '\t', '\n']).filter(|x| !x.is_empty()).map(String::from).collect();
        }
        if s.is_empty() {
            return Vec::new();
        }
        if fs.is_empty() {
            return s.chars().map(String::from).collect();
        }
        if fs.chars().count() == 1 && fs != "\\" {
            let c = fs.chars().next().unwrap();
            if c == '\t' || !".[]()*+?^$|{}".contains(c) {
                return s.split(c).map(String::from).collect();
            }
        }
        let re = match self.dyn_regex(fs) {
            Ok(r) => r,
            Err(_) => return vec![s.to_string()],
        };
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut at = 0;
        let mut search = 0;
        while search <= b.len() {
            match re.find(b, search) {
                Some((st, e)) if e > st => {
                    out.push(String::from_utf8_lossy(&b[at..st]).into_owned());
                    at = e;
                    search = e;
                }
                Some((st, _)) => {
                    if st >= b.len() {
                        break;
                    }
                    search = st + regex::char_at(b, st).1;
                }
                None => break,
            }
        }
        out.push(String::from_utf8_lossy(&b[at..]).into_owned());
        out
    }

    fn rebuild_record(&mut self) {
        let ofs = self.var_str("OFS");
        self.record = self.fields[..self.nf].join(&ofs);
        self.record_valid = true;
    }

    fn get_field(&mut self, i: usize) -> Value {
        if i == 0 {
            return Value::StrNum(self.record.clone());
        }
        self.split_fields();
        match self.fields.get(i - 1) {
            Some(f) if i <= self.nf => Value::StrNum(f.clone()),
            _ => Value::Uninit,
        }
    }

    fn set_field(&mut self, i: usize, v: String) {
        if i == 0 {
            self.set_record(v);
            return;
        }
        self.split_fields();
        if i > self.nf {
            self.fields.resize(i, String::new());
            self.nf = i;
        }
        self.fields[i - 1] = v;
        self.rebuild_record();
    }

    fn set_nf(&mut self) -> R<()> {
        let n = match self.globals.get("NF") {
            Some(Var::Scalar(v)) => v.num(),
            _ => 0.0,
        };
        if n < 0.0 {
            return Err(rt_err("NF set to a negative value"));
        }
        self.split_fields();
        let n = n as usize;
        self.fields.resize(n.max(self.fields.len()), String::new());
        self.nf = n;
        self.fields.truncate(n);
        self.rebuild_record();
        Ok(())
    }

    fn dyn_regex(&mut self, pat: &str) -> R<Regex> {
        if let Some(r) = self.dyn_re.get(pat) {
            return Ok(r.clone());
        }
        let re = Regex::new(pat, Syntax::Extended, false).map_err(|e| rt_err(format!("regular expression /{pat}/: {e}")))?;
        if self.dyn_re.len() > 500 {
            self.dyn_re.clear();
        }
        self.dyn_re.insert(pat.to_string(), re.clone());
        Ok(re)
    }

    fn regex_of(&mut self, e: &Expr) -> R<Regex> {
        if let Expr::Re(k) = e {
            return Ok(self.regexes[*k].clone());
        }
        let v = self.eval(e)?;
        let s = self.to_s(&v);
        self.dyn_regex(&s)
    }

    // --- input

    fn read_record(r: &mut dyn BufRead, rs: &str) -> R<Option<String>> {
        if rs == "\n" || rs.chars().count() == 1 {
            let sep = if rs == "\n" { b'\n' } else { rs.as_bytes()[0] };
            let mut buf = Vec::new();
            let n = r.read_until(sep, &mut buf)?;
            if n == 0 {
                return Ok(None);
            }
            if buf.last() == Some(&sep) {
                buf.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        if rs.is_empty() {
            // Paragraph mode: skip leading newlines, read until a blank line.
            let mut lines: Vec<String> = Vec::new();
            loop {
                let mut l = String::new();
                let n = r.read_line(&mut l)?;
                if n == 0 {
                    break;
                }
                let l = l.trim_end_matches('\n').to_string();
                if l.is_empty() {
                    if lines.is_empty() {
                        continue;
                    }
                    break;
                }
                lines.push(l);
            }
            return Ok(if lines.is_empty() { None } else { Some(lines.join("\n")) });
        }
        // A regex RS: read everything that is left and split on the first match.
        let mut rest = Vec::new();
        r.read_to_end(&mut rest)?;
        if rest.is_empty() {
            return Ok(None);
        }
        let re = Regex::new(rs, Syntax::Extended, false).map_err(|e| rt_err(e.0))?;
        // (A simple approach: callers re-feed the remainder; see main_record.)
        let _ = re;
        Ok(Some(String::from_utf8_lossy(&rest).into_owned()))
    }

    /// The next record from the main input (files on the command line, or stdin).
    fn main_record(&mut self) -> R<Option<String>> {
        let rs = self.var_str("RS");
        loop {
            if self.main.as_ref().unwrap().cur.is_none() {
                // Next file operand; var=value operands are assignments.
                let mut opened = false;
                loop {
                    let mi = self.main.as_mut().unwrap();
                    if mi.next_arg >= mi.args.len() {
                        break;
                    }
                    let a = mi.args[mi.next_arg].clone();
                    mi.next_arg += 1;
                    if a.is_empty() {
                        continue;
                    }
                    if let Some((name, val)) = assignment(&a) {
                        self.set_var(&name, Value::StrNum(unescape(&val)))?;
                        continue;
                    }
                    let reader: Box<dyn BufRead> = if a == "-" {
                        Box::new(BufReader::new(io::stdin()))
                    } else {
                        match fs::File::open(&a) {
                            Ok(f) => Box::new(BufReader::new(f)),
                            Err(e) => {
                                error("awk", format!("can't open file {a}: {e}"));
                                self.exit_code = 2;
                                continue;
                            }
                        }
                    };
                    let mi = self.main.as_mut().unwrap();
                    mi.cur = Some(reader);
                    mi.used_stdin = true;
                    self.globals.insert("FILENAME".into(), Var::Scalar(Value::Str(a)));
                    self.globals.insert("FNR".into(), Var::Scalar(Value::Num(0.0)));
                    opened = true;
                    break;
                }
                let mi = self.main.as_mut().unwrap();
                if !opened {
                    if mi.used_stdin {
                        return Ok(None);
                    }
                    mi.used_stdin = true;
                    mi.cur = Some(Box::new(BufReader::new(io::stdin())));
                    self.globals.insert("FNR".into(), Var::Scalar(Value::Num(0.0)));
                }
            }
            let mi = self.main.as_mut().unwrap();
            let r = mi.cur.as_mut().unwrap();
            if rs.chars().count() > 1 {
                // Regex RS: split the whole remaining input once and keep the pieces.
                let mut rest = Vec::new();
                r.read_to_end(&mut rest)?;
                if rest.is_empty() {
                    mi.cur = None;
                    continue;
                }
                let re = Regex::new(&rs, Syntax::Extended, false).map_err(|e| rt_err(e.0))?;
                let mut pieces = Vec::new();
                let mut at = 0;
                while let Some((s, e)) = re.find(&rest, at) {
                    if e == s {
                        break;
                    }
                    pieces.push(rest[at..s].to_vec());
                    at = e;
                }
                if at < rest.len() {
                    let mut tail = rest[at..].to_vec();
                    if tail.ends_with(b"\n") {
                        tail.pop();
                    }
                    pieces.push(tail);
                }
                let joined: Vec<u8> = pieces.join(&b'\0');
                mi.cur = Some(Box::new(io::Cursor::new([joined, vec![0]].concat())));
                // From now on read NUL-separated records from the cursor.
                self.globals.insert("RS".into(), Var::Scalar(Value::Str("\0".into())));
                continue;
            }
            match Self::read_record(r.as_mut(), &rs)? {
                Some(rec) => {
                    for v in ["NR", "FNR"] {
                        let n = match self.globals.get(v) {
                            Some(Var::Scalar(x)) => x.num(),
                            _ => 0.0,
                        };
                        self.globals.insert(v.into(), Var::Scalar(Value::Num(n + 1.0)));
                    }
                    return Ok(Some(rec));
                }
                None => {
                    mi.cur = None;
                }
            }
        }
    }

    // --- output

    fn output(&mut self, redir: &Option<Redir>) -> R<Option<String>> {
        let Some(r) = redir else { return Ok(None) };
        let (e, append, pipe) = match r {
            Redir::File(e) => (e, false, false),
            Redir::Append(e) => (e, true, false),
            Redir::Pipe(e) => (e, false, true),
        };
        let v = self.eval(e)?;
        let name = self.to_s(&v);
        if !self.outputs.contains_key(&name) {
            let o = if pipe {
                self.out.flush()?;
                let child = Command::new("/bin/sh").arg("-c").arg(&name).stdin(Stdio::piped()).spawn().map_err(|e| rt_err(format!("can't run {name}: {e}")))?;
                Out::Pipe(child)
            } else if name == "/dev/stdout" || name == "-" {
                return Ok(None);
            } else if name == "/dev/stderr" {
                return Ok(Some("\0stderr".into()));
            } else {
                let f = if append { fs::OpenOptions::new().append(true).create(true).open(&name) } else { fs::File::create(&name) };
                Out::File(io::BufWriter::new(f.map_err(|e| rt_err(format!("can't redirect to {name}: {e}")))?))
            };
            self.outputs.insert(name.clone(), o);
        }
        Ok(Some(name))
    }

    fn write_to(&mut self, dest: Option<String>, data: &[u8]) -> R<()> {
        match dest {
            None => self.out.write_all(data),
            Some(d) if d == "\0stderr" => {
                self.out.flush()?;
                io::stderr().write_all(data)
            }
            Some(d) => match self.outputs.get_mut(&d) {
                Some(Out::File(f)) => f.write_all(data),
                Some(Out::Pipe(c)) => c.stdin.as_mut().map_or(Ok(()), |s| s.write_all(data)),
                None => Ok(()),
            },
        }
    }

    fn close(&mut self, name: &str) -> i32 {
        let mut r = -1;
        if let Some(o) = self.outputs.remove(name) {
            r = match o {
                Out::File(mut f) => i32::from(f.flush().is_err()),
                Out::Pipe(mut c) => {
                    drop(c.stdin.take());
                    c.wait().ok().and_then(|s| s.code()).unwrap_or(-1)
                }
            };
        }
        if let Some(mut rd) = self.readers.remove(name) {
            r = match rd.child.take() {
                Some(mut c) => c.wait().ok().and_then(|s| s.code()).unwrap_or(-1),
                None => 0,
            };
        }
        r
    }

    fn close_all(&mut self) {
        let _ = self.out.flush();
        let names: Vec<String> = self.outputs.keys().cloned().collect();
        for n in names {
            self.close(&n);
        }
        let names: Vec<String> = self.readers.keys().cloned().collect();
        for n in names {
            self.close(&n);
        }
    }

    // --- evaluation

    fn assign(&mut self, lv: &Expr, v: Value) -> R<Value> {
        match lv {
            Expr::Var(n) => {
                let v = match v {
                    Value::Uninit => Value::Str(String::new()),
                    other => other,
                };
                self.set_var(n, v.clone())?;
                Ok(v)
            }
            Expr::Field(i) => {
                let iv = self.eval(i)?.num();
                if iv < 0.0 {
                    return Err(rt_err(format!("trying to access out of range field {}", iv as i64)));
                }
                let s = self.to_s(&v);
                self.set_field(iv as usize, s.clone());
                Ok(Value::StrNum(s))
            }
            Expr::Index(n, subs) => {
                let k = self.subscript(subs)?;
                let id = self.array_id(n)?;
                self.arrays[id].insert(k, v.clone());
                Ok(v)
            }
            Expr::Group(inner) => self.assign(inner, v),
            _ => Err(rt_err("assignment to something that is not a variable")),
        }
    }

    fn eval(&mut self, e: &Expr) -> R<Value> {
        Ok(match e {
            Expr::Num(n) => Value::Num(*n),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Re(k) => {
                let re = self.regexes[*k].clone();
                Value::Num(if re.is_match(self.record.as_bytes()) { 1.0 } else { 0.0 })
            }
            Expr::Var(n) => self.get_var(n)?,
            Expr::Group(x) => self.eval(x)?,
            Expr::Field(i) => {
                let iv = self.eval(i)?.num();
                if iv < 0.0 {
                    return Err(rt_err(format!("trying to access out of range field {}", iv as i64)));
                }
                self.get_field(iv as usize)
            }
            Expr::Index(n, subs) => {
                let k = self.subscript(subs)?;
                let id = self.array_id(n)?;
                self.arrays[id].entry(k).or_insert(Value::Uninit).clone()
            }
            Expr::Assign(lv, op, rhs) => {
                let r = self.eval(rhs)?;
                let v = match op {
                    None => r,
                    Some(op) => {
                        let l = self.eval(lv)?.num();
                        Value::Num(arith(op, l, r.num())?)
                    }
                };
                self.assign(lv, v)?
            }
            Expr::Cond(c, a, b) => {
                if self.eval(c)?.truthy() {
                    self.eval(a)?
                } else {
                    self.eval(b)?
                }
            }
            Expr::And(a, b) => Value::Num(if self.eval(a)?.truthy() && self.eval(b)?.truthy() { 1.0 } else { 0.0 }),
            Expr::Or(a, b) => Value::Num(if self.eval(a)?.truthy() || self.eval(b)?.truthy() { 1.0 } else { 0.0 }),
            Expr::In(subs, arr) => {
                let k = self.subscript(subs)?;
                let id = self.array_id(arr)?;
                Value::Num(if self.arrays[id].contains_key(&k) { 1.0 } else { 0.0 })
            }
            Expr::Match(neg, l, r) => {
                let lv = self.eval(l)?;
                let s = self.to_s(&lv);
                let re = self.regex_of(r)?;
                Value::Num(if re.is_match(s.as_bytes()) != *neg { 1.0 } else { 0.0 })
            }
            Expr::Bin(op, a, b) => {
                let x = self.eval(a)?.num();
                let y = self.eval(b)?.num();
                Value::Num(arith(op, x, y)?)
            }
            Expr::Cmp(op, a, b) => {
                let x = self.eval(a)?;
                let y = self.eval(b)?;
                let ord = match (x.cmp_num(), y.cmp_num()) {
                    (Some(p), Some(q)) => p.partial_cmp(&q),
                    _ => Some(self.to_s(&x).cmp(&self.to_s(&y))),
                };
                let r = match ord {
                    None => *op == "!=",
                    Some(o) => match *op {
                        "<" => o.is_lt(),
                        "<=" => o.is_le(),
                        ">" => o.is_gt(),
                        ">=" => o.is_ge(),
                        "==" => o.is_eq(),
                        _ => o.is_ne(),
                    },
                };
                Value::Num(if r { 1.0 } else { 0.0 })
            }
            Expr::Concat(a, b) => {
                let x = self.eval(a)?;
                let mut s = self.to_s(&x);
                let y = self.eval(b)?;
                s.push_str(&self.to_s(&y));
                Value::Str(s)
            }
            Expr::Neg(a) => Value::Num(-self.eval(a)?.num()),
            Expr::Plus(a) => Value::Num(self.eval(a)?.num()),
            Expr::Not(a) => Value::Num(if self.eval(a)?.truthy() { 0.0 } else { 1.0 }),
            Expr::Incr(prefix, d, lv) => {
                let old = self.eval(lv)?.num();
                let new = old + f64::from(*d);
                self.assign(lv, Value::Num(new))?;
                Value::Num(if *prefix { new } else { old })
            }
            Expr::Call(name, args) if name == "\0list" => {
                // (a, b) outside `in`: concatenate with SUBSEP like a subscript
                let k = self.subscript(args)?;
                Value::Str(k)
            }
            Expr::Call(name, args) => self.call(name, args)?,
            Expr::Builtin(b, args) => self.builtin(b, args)?,
            Expr::Getline { src, var } => self.getline(src, var.as_deref())?,
        })
    }

    fn getline(&mut self, src: &GetSrc, var: Option<&Expr>) -> R<Value> {
        let rs = self.var_str("RS");
        let rec = match src {
            GetSrc::Main => match self.main_record()? {
                Some(r) => r,
                None => return Ok(Value::Num(0.0)),
            },
            GetSrc::File(f) | GetSrc::Cmd(f) => {
                let v = self.eval(f)?;
                let name = self.to_s(&v);
                let is_cmd = matches!(src, GetSrc::Cmd(_));
                if !self.readers.contains_key(&name) {
                    let rd = if is_cmd {
                        self.out.flush()?;
                        let mut child = match Command::new("/bin/sh").arg("-c").arg(&name).stdout(Stdio::piped()).spawn() {
                            Ok(c) => c,
                            Err(_) => return Ok(Value::Num(-1.0)),
                        };
                        let out = child.stdout.take().unwrap();
                        Reader { r: Box::new(BufReader::new(out)), eof: false, child: Some(child) }
                    } else if name == "-" || name == "/dev/stdin" {
                        Reader { r: Box::new(BufReader::new(io::stdin())), eof: false, child: None }
                    } else {
                        match fs::File::open(&name) {
                            Ok(f) => Reader { r: Box::new(BufReader::new(f)), eof: false, child: None },
                            Err(_) => return Ok(Value::Num(-1.0)),
                        }
                    };
                    self.readers.insert(name.clone(), rd);
                }
                let rd = self.readers.get_mut(&name).unwrap();
                if rd.eof {
                    return Ok(Value::Num(0.0));
                }
                match Self::read_record(rd.r.as_mut(), &rs)? {
                    Some(r) => {
                        if var.is_none() || is_cmd {
                            let n = self.get_var("NR")?.num();
                            self.globals.insert("NR".into(), Var::Scalar(Value::Num(n + 1.0)));
                        }
                        r
                    }
                    None => {
                        rd.eof = true;
                        return Ok(Value::Num(0.0));
                    }
                }
            }
        };
        match var {
            Some(v) => {
                self.assign(v, Value::StrNum(rec))?;
            }
            None => {
                self.set_record(rec);
                self.split_fields();
            }
        }
        Ok(Value::Num(1.0))
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> R<Value> {
        let Some(f) = self.funcs.get(name).cloned() else { return Err(rt_err(format!("calling undefined function {name}"))) };
        if args.len() > f.params.len() {
            return Err(rt_err(format!("function {name} called with {} args, accepts only {}", args.len(), f.params.len())));
        }
        let arr_params = self.array_params.get(name).cloned().unwrap_or_default();
        let mut frame: HashMap<String, Var> = HashMap::new();
        for (k, p) in f.params.iter().enumerate() {
            let v = match args.get(k) {
                Some(Expr::Var(n)) => {
                    // A bare name: an array is passed by reference.
                    match self.lookup(n).cloned() {
                        Some(Var::Array(id)) => Var::Array(id),
                        Some(Var::Scalar(Value::Uninit)) | None if arr_params.contains(p) => Var::Array(self.array_id(n)?),
                        Some(Var::Scalar(v)) => Var::Scalar(v),
                        None => Var::Scalar(Value::Uninit),
                    }
                }
                Some(e) => Var::Scalar(self.eval(e)?),
                None => Var::Scalar(Value::Uninit),
            };
            frame.insert(p.clone(), v);
        }
        if self.frames.len() > 1000 {
            return Err(rt_err("function call nesting too deep"));
        }
        self.frames.push(frame);
        let r = self.exec_block(&f.body);
        self.frames.pop();
        match r? {
            Ctl::Return(v) => Ok(v),
            Ctl::Exit => Err(io::Error::new(io::ErrorKind::Interrupted, "exit")),
            Ctl::Next => Err(io::Error::new(io::ErrorKind::WouldBlock, "next")),
            _ => Ok(Value::Uninit),
        }
    }

    fn builtin(&mut self, b: &str, args: &[Expr]) -> R<Value> {
        let argc = args.len();
        let want = |lo: usize, hi: usize| -> R<()> {
            if argc < lo || argc > hi {
                Err(rt_err(format!("{b}: wrong number of arguments")))
            } else {
                Ok(())
            }
        };
        Ok(match b {
            "length" => {
                want(0, 1)?;
                match args.first() {
                    None => Value::Num(self.record.chars().count() as f64),
                    Some(Expr::Var(n)) if matches!(self.lookup(n), Some(Var::Array(_))) => {
                        let id = self.array_id(n)?;
                        Value::Num(self.arrays[id].len() as f64)
                    }
                    Some(e) => {
                        let v = self.eval(e)?;
                        Value::Num(self.to_s(&v).chars().count() as f64)
                    }
                }
            }
            "substr" => {
                want(2, 3)?;
                let sv = self.eval(&args[0])?;
                let s: Vec<char> = self.to_s(&sv).chars().collect();
                let m = self.eval(&args[1])?.num();
                let m = round_half_even(m);
                let (start, end) = if argc == 3 {
                    let n = round_half_even(self.eval(&args[2])?.num());
                    (m, m + n)
                } else {
                    (m, f64::INFINITY)
                };
                let st = start.max(1.0);
                let en = end.min(s.len() as f64 + 1.0);
                if en <= st || st.is_nan() || en.is_nan() {
                    Value::Str(String::new())
                } else {
                    Value::Str(s[(st as usize - 1)..(en as usize - 1)].iter().collect())
                }
            }
            "index" => {
                want(2, 2)?;
                let a = self.eval(&args[0])?;
                let a = self.to_s(&a);
                let t = self.eval(&args[1])?;
                let t = self.to_s(&t);
                Value::Num(match a.find(&t) {
                    Some(k) => (a[..k].chars().count() + 1) as f64,
                    None => 0.0,
                })
            }
            "split" => {
                want(2, 3)?;
                let sv = self.eval(&args[0])?;
                let s = self.to_s(&sv);
                let Expr::Var(arr) = &args[1] else { return Err(rt_err("split: second argument must be an array")) };
                let parts = match args.get(2) {
                    Some(Expr::Re(k)) => {
                        let re = self.regexes[*k].clone();
                        split_re(&s, &re)
                    }
                    Some(e) => {
                        let v = self.eval(e)?;
                        let fs = self.to_s(&v);
                        self.split_str(&s, &fs)
                    }
                    None => {
                        let fs = self.var_str("FS");
                        self.split_str(&s, &fs)
                    }
                };
                let id = self.array_id(arr)?;
                self.arrays[id].clear();
                for (k, p) in parts.iter().enumerate() {
                    self.arrays[id].insert((k + 1).to_string(), Value::StrNum(p.clone()));
                }
                Value::Num(parts.len() as f64)
            }
            "sub" | "gsub" => {
                want(2, 3)?;
                let re = self.regex_of(&args[0])?;
                let rv = self.eval(&args[1])?;
                let repl = self.to_s(&rv);
                let target = args.get(2).cloned().unwrap_or(Expr::Field(Box::new(Expr::Num(0.0))));
                let tv = self.eval(&target)?;
                let text = self.to_s(&tv);
                let which = if b == "gsub" { regex::Which::All } else { regex::Which::Nth(1) };
                let (new, n) = regex::replace(&re, text.as_bytes(), which, |caps, t| awk_repl(repl.as_bytes(), caps, t));
                if n > 0 {
                    self.assign(&target, Value::Str(String::from_utf8_lossy(&new).into_owned()))?;
                }
                Value::Num(n as f64)
            }
            "match" => {
                want(2, 3)?;
                let sv = self.eval(&args[0])?;
                let s = self.to_s(&sv);
                let re = self.regex_of(&args[1])?;
                let (start, len) = match re.captures_at(s.as_bytes(), 0) {
                    Some(c) => {
                        let (a, e) = c[0].unwrap();
                        if let Some(Expr::Var(arr)) = args.get(2) {
                            let id = self.array_id(arr)?;
                            self.arrays[id].clear();
                            for (k, g) in c.iter().enumerate() {
                                if let Some((gs, ge)) = g {
                                    self.arrays[id].insert(k.to_string(), Value::StrNum(s[*gs..*ge].to_string()));
                                }
                            }
                        }
                        ((s[..a].chars().count() + 1) as f64, s[a..e].chars().count() as f64)
                    }
                    None => (0.0, -1.0),
                };
                self.globals.insert("RSTART".into(), Var::Scalar(Value::Num(start)));
                self.globals.insert("RLENGTH".into(), Var::Scalar(Value::Num(len)));
                Value::Num(start)
            }
            "sprintf" => {
                if argc == 0 {
                    return Err(rt_err("sprintf: no format"));
                }
                let mut vals = Vec::new();
                for a in args {
                    vals.push(self.eval(a)?);
                }
                let fmt = self.to_s(&vals[0]);
                let conv = self.convfmt();
                Value::Str(sprintf(&fmt, &vals[1..], &conv))
            }
            "sin" | "cos" | "exp" | "log" | "sqrt" | "int" => {
                want(1, 1)?;
                let x = self.eval(&args[0])?.num();
                Value::Num(match b {
                    "sin" => x.sin(),
                    "cos" => x.cos(),
                    "exp" => x.exp(),
                    "log" => x.ln(),
                    "sqrt" => x.sqrt(),
                    _ => x.trunc(),
                })
            }
            "atan2" => {
                want(2, 2)?;
                let y = self.eval(&args[0])?.num();
                let x = self.eval(&args[1])?.num();
                Value::Num(y.atan2(x))
            }
            "rand" => {
                want(0, 0)?;
                // xorshift64*
                let mut x = self.rand_state;
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                self.rand_state = x;
                Value::Num((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64)
            }
            "srand" => {
                want(0, 1)?;
                let seed = match args.first() {
                    Some(e) => self.eval(e)?.num(),
                    None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as f64).unwrap_or(0.0),
                };
                let prev = self.prev_seed;
                self.prev_seed = seed;
                self.rand_state = (seed as i64 as u64).wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407) | 1;
                Value::Num(prev)
            }
            "tolower" | "toupper" => {
                want(1, 1)?;
                let v = self.eval(&args[0])?;
                let s = self.to_s(&v);
                Value::Str(if b == "tolower" { s.to_lowercase() } else { s.to_uppercase() })
            }
            "system" => {
                want(1, 1)?;
                let v = self.eval(&args[0])?;
                let cmd = self.to_s(&v);
                self.out.flush()?;
                for o in self.outputs.values_mut() {
                    if let Out::File(f) = o {
                        let _ = f.flush();
                    }
                }
                let st = Command::new("/bin/sh").arg("-c").arg(&cmd).status();
                Value::Num(match st {
                    Ok(s) => match s.code() {
                        Some(c) => f64::from(c),
                        None => 256.0,
                    },
                    Err(_) => -1.0,
                })
            }
            "close" => {
                want(1, 2)?;
                let v = self.eval(&args[0])?;
                let n = self.to_s(&v);
                Value::Num(f64::from(self.close(&n)))
            }
            "fflush" => {
                self.out.flush()?;
                for o in self.outputs.values_mut() {
                    match o {
                        Out::File(f) => {
                            let _ = f.flush();
                        }
                        Out::Pipe(c) => {
                            if let Some(s) = c.stdin.as_mut() {
                                let _ = s.flush();
                            }
                        }
                    }
                }
                Value::Num(0.0)
            }
            _ => return Err(rt_err(format!("unknown function {b}"))),
        })
    }

    fn exec_block(&mut self, v: &[Stmt]) -> R<Ctl> {
        for s in v {
            match self.exec(s)? {
                Ctl::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Ctl::Normal)
    }

    fn exec(&mut self, s: &Stmt) -> R<Ctl> {
        match s {
            Stmt::Expr(e) | Stmt::Getline(e) => {
                self.eval(e)?;
            }
            Stmt::Print(args, redir) => {
                let dest = self.output(redir)?;
                let line = if args.is_empty() {
                    self.record.clone()
                } else {
                    let ofs = self.var_str("OFS");
                    let mut parts = Vec::new();
                    for a in args {
                        let v = self.eval(a)?;
                        parts.push(self.out_s(&v));
                    }
                    parts.join(&ofs)
                };
                let ors = self.var_str("ORS");
                self.write_to(dest, format!("{line}{ors}").as_bytes())?;
            }
            Stmt::Printf(args, redir) => {
                let dest = self.output(redir)?;
                let mut vals = Vec::new();
                for a in args {
                    vals.push(self.eval(a)?);
                }
                let fmt = self.to_s(&vals[0]);
                let conv = self.convfmt();
                let s = sprintf(&fmt, &vals[1..], &conv);
                self.write_to(dest, s.as_bytes())?;
            }
            Stmt::If(c, a, b) => {
                if self.eval(c)?.truthy() {
                    return self.exec(a);
                } else if let Some(b) = b {
                    return self.exec(b);
                }
            }
            Stmt::While(c, body) => {
                while self.eval(c)?.truthy() {
                    match self.exec(body)? {
                        Ctl::Break => break,
                        Ctl::Continue | Ctl::Normal => {}
                        other => return Ok(other),
                    }
                }
            }
            Stmt::Do(body, c) => loop {
                match self.exec(body)? {
                    Ctl::Break => break,
                    Ctl::Continue | Ctl::Normal => {}
                    other => return Ok(other),
                }
                if !self.eval(c)?.truthy() {
                    break;
                }
            },
            Stmt::For(init, cond, step, body) => {
                if let Some(i) = init {
                    self.exec(i)?;
                }
                loop {
                    if let Some(c) = cond {
                        if !self.eval(c)?.truthy() {
                            break;
                        }
                    }
                    match self.exec(body)? {
                        Ctl::Break => break,
                        Ctl::Continue | Ctl::Normal => {}
                        other => return Ok(other),
                    }
                    if let Some(st) = step {
                        self.exec(st)?;
                    }
                }
            }
            Stmt::ForIn(var, arr, body) => {
                let id = self.array_id(arr)?;
                let mut keys: Vec<String> = self.arrays[id].keys().cloned().collect();
                // Numeric keys in numeric order, then the rest: predictable output.
                keys.sort_by(|a, b| match (looks_numeric(a), looks_numeric(b)) {
                    (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    _ => a.cmp(b),
                });
                for k in keys {
                    if !self.arrays[id].contains_key(&k) {
                        continue; // deleted during the loop
                    }
                    self.set_var(var, Value::StrNum(k))?;
                    match self.exec(body)? {
                        Ctl::Break => break,
                        Ctl::Continue | Ctl::Normal => {}
                        other => return Ok(other),
                    }
                }
            }
            Stmt::Block(v) => return self.exec_block(v),
            Stmt::Next => return Ok(Ctl::Next),
            Stmt::NextFile => return Ok(Ctl::NextFile),
            Stmt::Break => return Ok(Ctl::Break),
            Stmt::Continue => return Ok(Ctl::Continue),
            Stmt::Exit(e) => {
                if let Some(e) = e {
                    self.exit_code = self.eval(e)?.num() as i32;
                }
                return Ok(Ctl::Exit);
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => Value::Uninit,
                };
                return Ok(Ctl::Return(v));
            }
            Stmt::Delete(n, subs) => {
                let id = self.array_id(n)?;
                match subs {
                    Some(s) => {
                        let k = self.subscript(s)?;
                        self.arrays[id].remove(&k);
                    }
                    None => self.arrays[id].clear(),
                }
            }
        }
        Ok(Ctl::Normal)
    }

    fn run_action(&mut self, a: &Option<Vec<Stmt>>) -> R<Ctl> {
        match a {
            Some(stmts) => self.exec_block(stmts),
            None => {
                let ors = self.var_str("ORS");
                let line = format!("{}{ors}", self.record);
                self.out.write_all(line.as_bytes())?;
                Ok(Ctl::Normal)
            }
        }
    }
}

fn round_half_even(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        r - x.signum()
    } else {
        r
    }
}

fn arith(op: &str, x: f64, y: f64) -> R<f64> {
    Ok(match op {
        "+" => x + y,
        "-" => x - y,
        "*" => x * y,
        "/" => {
            if y == 0.0 {
                return Err(rt_err("division by zero"));
            }
            x / y
        }
        "%" => {
            if y == 0.0 {
                return Err(rt_err("division by zero in %"));
            }
            x % y
        }
        _ => x.powf(y),
    })
}

fn split_re(s: &str, re: &Regex) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    let mut search = 0;
    while let Some((st, e)) = re.find(b, search) {
        if e == st {
            if st >= b.len() {
                break;
            }
            search = st + 1;
            continue;
        }
        out.push(String::from_utf8_lossy(&b[at..st]).into_owned());
        at = e;
        search = e;
    }
    out.push(String::from_utf8_lossy(&b[at..]).into_owned());
    out
}

/// sub/gsub replacement: `&` is the match, `\&` a literal ampersand, `\\` a backslash.
fn awk_repl(repl: &[u8], caps: &regex::Captures, text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < repl.len() {
        match repl[i] {
            b'\\' if i + 1 < repl.len() && (repl[i + 1] == b'&' || repl[i + 1] == b'\\') => {
                out.push(repl[i + 1]);
                i += 2;
            }
            b'&' => {
                if let Some((s, e)) = caps[0] {
                    out.extend_from_slice(&text[s..e]);
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn assignment(a: &str) -> Option<(String, String)> {
    let (name, val) = a.split_once('=')?;
    let ok = !name.is_empty() && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then(|| (name.to_string(), val.to_string()))
}

/// Escape sequences in -v and command-line assignments.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('/') => out.push('/'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

const USAGE: &str = "usage: awk [-F FS] [-v VAR=VALUE]... 'PROGRAM' [FILE...]\n       awk [-F FS] [-v VAR=VALUE]... -f PROGFILE [FILE...]";

pub fn awk(args: &[String]) -> i32 {
    let mut fs_opt: Option<String> = None;
    let mut assigns: Vec<(String, String)> = Vec::new();
    let mut prog_src: Option<Vec<u8>> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        i += 1;
        let mut val = |flag: &str| -> Option<String> {
            if a.len() > flag.len() {
                Some(a[flag.len()..].to_string())
            } else {
                let v = args.get(i).cloned();
                i += 1;
                v
            }
        };
        if a.starts_with("-F") {
            match val("-F") {
                Some(f) => fs_opt = Some(if f == "t" { "\t".into() } else { unescape(&f) }),
                None => {
                    error("awk", USAGE);
                    return 2;
                }
            }
        } else if a.starts_with("-v") {
            match val("-v").and_then(|v| assignment(&v)) {
                Some((n, v)) => assigns.push((n, unescape(&v))),
                None => {
                    error("awk", "-v needs NAME=VALUE");
                    return 2;
                }
            }
        } else if a.starts_with("-f") {
            match val("-f") {
                Some(f) => match super::text::read_input(&f) {
                    Ok(d) => {
                        let mut src = prog_src.take().unwrap_or_default();
                        src.extend(d);
                        src.push(b'\n');
                        prog_src = Some(src);
                    }
                    Err(e) => {
                        error("awk", format!("can't open source file {f}: {e}"));
                        return 2;
                    }
                },
                None => {
                    error("awk", USAGE);
                    return 2;
                }
            }
        } else if a == "--version" {
            println!("awk (Flint)");
            return 0;
        } else {
            error("awk", format!("unknown option {a}\n{USAGE}"));
            return 2;
        }
    }
    let src = match prog_src {
        Some(s) => s,
        None => match args.get(i) {
            Some(p) => {
                i += 1;
                p.as_bytes().to_vec()
            }
            None => {
                error("awk", USAGE);
                return 2;
            }
        },
    };
    let operands: Vec<String> = args[i..].to_vec();
    let mut parser = match Parser::new(&src) {
        Ok(p) => p,
        Err(e) => {
            error("awk", format!("syntax error: {e}"));
            return 2;
        }
    };
    let (items, funcs) = match parser.program() {
        Ok(x) => x,
        Err(e) => {
            error("awk", format!("syntax error: {e}"));
            return 2;
        }
    };
    let mut regexes = Vec::new();
    for r in &parser.regexes {
        match Regex::new(r, Syntax::Extended, false) {
            Ok(x) => regexes.push(x),
            Err(e) => {
                error("awk", format!("regular expression /{r}/: {e}"));
                return 2;
            }
        }
    }
    let array_params = funcs.iter().map(|(n, f)| (n.clone(), f.params.iter().filter(|p| uses_as_array(&f.body, p)).cloned().collect())).collect();
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    let mut it = Interp {
        globals: HashMap::new(),
        frames: Vec::new(),
        arrays: Vec::new(),
        funcs,
        array_params,
        regexes,
        dyn_re: HashMap::new(),
        fields: Vec::new(),
        record: String::new(),
        fields_valid: true,
        record_valid: true,
        nf: 0,
        out: io::BufWriter::new(io::stdout()),
        outputs: HashMap::new(),
        readers: HashMap::new(),
        main: Some(MainInput { args: operands.clone(), next_arg: 0, cur: None, used_stdin: false }),
        exit_code: 0,
        range_active: vec![false; items.len()],
        rand_state: 0x2545_F491_4F6C_DD1D,
        prev_seed: 0.0,
    };
    let _ = seed;
    for (k, v) in [("FS", " "), ("OFS", " "), ("ORS", "\n"), ("RS", "\n"), ("SUBSEP", "\x1c"), ("CONVFMT", "%.6g"), ("OFMT", "%.6g"), ("FILENAME", "")] {
        it.globals.insert(k.into(), Var::Scalar(Value::Str(v.into())));
    }
    for k in ["NR", "FNR", "RSTART"] {
        it.globals.insert(k.into(), Var::Scalar(Value::Num(0.0)));
    }
    it.globals.insert("RLENGTH".into(), Var::Scalar(Value::Num(-1.0)));
    let env_id = it.arrays.len();
    it.arrays.push(std::env::vars().map(|(k, v)| (k, Value::StrNum(v))).collect());
    it.globals.insert("ENVIRON".into(), Var::Array(env_id));
    let argv_id = it.arrays.len();
    let mut argv = HashMap::new();
    argv.insert("0".to_string(), Value::Str("awk".into()));
    for (k, a) in operands.iter().enumerate() {
        argv.insert((k + 1).to_string(), Value::StrNum(a.clone()));
    }
    it.arrays.push(argv);
    it.globals.insert("ARGV".into(), Var::Array(argv_id));
    it.globals.insert("ARGC".into(), Var::Scalar(Value::Num((operands.len() + 1) as f64)));
    if let Some(f) = fs_opt {
        it.globals.insert("FS".into(), Var::Scalar(Value::Str(f)));
    }
    for (n, v) in assigns {
        it.globals.insert(n, Var::Scalar(Value::StrNum(v)));
    }

    let result = run_program(&mut it, &items);
    it.close_all();
    match result {
        Ok(()) => it.exit_code,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => it.exit_code,
        Err(e) => {
            let _ = it.out.flush();
            error("awk", e);
            2
        }
    }
}

fn run_program(it: &mut Interp, items: &[Item]) -> R<()> {
    let has_main = items.iter().any(|x| !matches!(x.pattern, Pattern::Begin | Pattern::End));
    let has_end = items.iter().any(|x| matches!(x.pattern, Pattern::End));
    let mut exiting = false;
    for item in items.iter().filter(|x| matches!(x.pattern, Pattern::Begin)) {
        match it.run_action(&item.action) {
            Ok(Ctl::Exit) => {
                exiting = true;
                break;
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                exiting = true;
                break;
            }
            Err(e) => return Err(e),
        }
    }
    // ARGV may have been changed in BEGIN.
    if let Some(Var::Array(id)) = it.globals.get("ARGV").cloned() {
        let argc = it.get_var("ARGC")?.num() as usize;
        let mut ops = Vec::new();
        for k in 1..argc {
            if let Some(v) = it.arrays[id].get(&k.to_string()) {
                ops.push(v.string("%.6g"));
            }
        }
        it.main.as_mut().unwrap().args = ops;
    }
    if !exiting && (has_main || has_end) {
        'records: while let Some(rec) = it.main_record()? {
            it.set_record(rec);
            for (k, item) in items.iter().enumerate() {
                let selected = match &item.pattern {
                    Pattern::Begin | Pattern::End => continue,
                    Pattern::All => true,
                    Pattern::Expr(e) => match it.eval(e) {
                        Ok(v) => v.truthy(),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                            exiting = true;
                            break 'records;
                        }
                        Err(e) => return Err(e),
                    },
                    Pattern::Range(a, b) => {
                        if it.range_active[k] {
                            if it.eval(b)?.truthy() {
                                it.range_active[k] = false;
                            }
                            true
                        } else if it.eval(a)?.truthy() {
                            it.range_active[k] = !it.eval(b)?.truthy();
                            true
                        } else {
                            false
                        }
                    }
                };
                if !selected {
                    continue;
                }
                match it.run_action(&item.action) {
                    Ok(Ctl::Next) => continue 'records,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue 'records,
                    Ok(Ctl::NextFile) => {
                        it.main.as_mut().unwrap().cur = None;
                        continue 'records;
                    }
                    Ok(Ctl::Exit) => {
                        exiting = true;
                        break 'records;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                        exiting = true;
                        break 'records;
                    }
                    Ok(_) => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }
    let _ = exiting;
    // END runs after exit in BEGIN or main too (exit inside END stops it).
    for item in items.iter().filter(|x| matches!(x.pattern, Pattern::End)) {
        match it.run_action(&item.action) {
            Ok(Ctl::Exit) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
            Ok(_) => {}
            Err(e) => return Err(e),
        }
    }
    it.out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_strings() {
        assert_eq!(looks_numeric(" 12 "), Some(12.0));
        assert_eq!(looks_numeric("1e3"), Some(1000.0));
        assert_eq!(looks_numeric("+.5"), Some(0.5));
        assert_eq!(looks_numeric("12a"), None);
        assert_eq!(looks_numeric("."), None);
        assert_eq!(str_to_num("12abc"), 12.0);
        assert_eq!(str_to_num("  -3.5e2x"), -350.0);
        assert_eq!(str_to_num("abc"), 0.0);
        assert_eq!(num_to_str(3.0, "%.6g"), "3");
        assert_eq!(num_to_str(0.1 + 0.2, "%.6g"), "0.3");
        assert_eq!(num_to_str(1e20, "%.6g"), "1e+20");
    }

    #[test]
    fn printf_formats() {
        let n = |x: f64| Value::Num(x);
        let s = |x: &str| Value::Str(x.into());
        assert_eq!(sprintf("%d|%5d|%-5d|%05d", &[n(42.9), n(-7.0), n(3.0), n(-42.0)], "%.6g"), "42|   -7|3    |-0042");
        assert_eq!(sprintf("%x %X %o %#x %u", &[n(255.0), n(255.0), n(8.0), n(255.0), n(7.0)], "%.6g"), "ff FF 10 0xff 7");
        assert_eq!(sprintf("%.2f %e %g %g %g", &[n(3.14159), n(12345.678), n(0.0001), n(123456789.0), n(100.0)], "%.6g"), "3.14 1.234568e+04 0.0001 1.23457e+08 100");
        assert_eq!(sprintf("%s|%10s|%-4s|%.2s|%c%c", &[s("hi"), s("right"), s("l"), s("abc"), n(65.0), s("xyz")], "%.6g"), "hi|     right|l   |ab|Ax");
        assert_eq!(sprintf("%5.1f%%", &[n(99.44)], "%.6g"), " 99.4%");
        assert_eq!(sprintf("%*d|%.*f", &[n(4.0), n(7.0), n(1.0), n(2.25)], "%.6g"), "   7|2.2");
        assert_eq!(sprintf("%+d % d", &[n(5.0), n(5.0)], "%.6g"), "+5  5");
        assert_eq!(sprintf("%g", &[n(1e-5)], "%.6g"), "1e-05");
    }

    fn parse_ok(src: &str) {
        let mut p = Parser::new(src.as_bytes()).unwrap();
        p.program().unwrap_or_else(|e| panic!("{src}: {e}"));
    }

    #[test]
    fn parsing() {
        for src in [
            "{ print $1, $NF }",
            "BEGIN { FS = \":\" } $3 > 100 { n++ } END { print n+0 }",
            "/re/ { x[$1] += $2 } END { for (k in x) print k, x[k] | \"sort\" }",
            "function f(a, b) { return a + b } { print f($1, 2) }",
            "{ if ($1 ~ /^#/) next; else print }",
            "{ while ((getline line < \"f\") > 0) n++ }",
            "{ \"date\" | getline d; print d > \"/dev/stderr\" }",
            "NR==1, NR==3",
            "{ a = b = 3; print a b; print (1,2) in x }",
            "{ printf \"%s\\n\", $0 }",
            "{ print $1 > $2 \".txt\" }",
            "BEGIN { x = 2^3^2; y = -2^2; print x, y, 1 - -1 }",
            "{ for (i = 1; i <= NF; i++) s += $i } END { print s }",
            "{ $3 = \"\"; print; print length }",
            "BEGIN { do { i++ } while (i < 5) }",
            "{ print (length($0) > 10 ? \"long\" : \"short\") }",
            "# comment\nBEGIN {\n  if (1)\n    print \"a\"\n  else\n    print \"b\"\n}",
        ] {
            parse_ok(src);
        }
        assert!(Parser::new(b"{ print ( }").unwrap().program().is_err());
        assert!(Parser::new(b"BEGIN").unwrap().program().is_err());
    }
}
