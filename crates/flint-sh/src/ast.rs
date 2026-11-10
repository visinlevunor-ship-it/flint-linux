//! The syntax tree. Words stay as raw text (quotes and `$` intact); expansion happens when
//! a command runs.

use std::rc::Rc;

pub type Word = String;

#[derive(Debug, Clone, PartialEq)]
pub enum RedirKind {
    /// `< word`
    In(Word),
    /// `> word`
    Out(Word),
    /// `>> word`
    Append(Word),
    /// `n>&word` / `n<&word`: duplicate descriptor `word`, or close with `-`
    Dup(Word),
    /// `<<` here-document; `expand` is false when the delimiter was quoted
    HereDoc { body: String, expand: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Redir {
    pub fd: i32,
    pub kind: RedirKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Simple {
    pub assigns: Vec<(String, Word)>,
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub cmd: Command,
    pub background: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Simple(Simple),
    Pipeline { negate: bool, stages: Vec<Command> },
    /// `first && a || b ...`: `true` means `&&`
    AndOr(Box<Command>, Vec<(bool, Command)>),
    List(Vec<Item>),
    Subshell(Box<Command>),
    Group(Box<Command>),
    If { arms: Vec<(Command, Command)>, otherwise: Option<Box<Command>> },
    While { cond: Box<Command>, body: Box<Command>, until: bool },
    For { var: String, words: Option<Vec<Word>>, body: Box<Command> },
    Case { word: Word, arms: Vec<(Vec<Word>, Command)> },
    FuncDef { name: String, body: Rc<Command> },
    Redirected(Box<Command>, Vec<Redir>),
}
