//! Abstract syntax tree for the nash language.

use std::cell::RefCell;
use std::rc::Rc;

pub type Word = Vec<WordPart>;

#[derive(Debug, Clone)]
pub enum WordPart {
    /// Unquoted literal text (subject to globbing and brace expansion).
    Lit(String),
    /// Quoted literal text (single quotes, backslash escapes, $'..').
    Quoted(String),
    DQuote(Vec<WordPart>),
    Param(Box<ParamExp>),
    CmdSubst(Rc<Cmd>),
    Arith(String),
    Tilde(String),
    /// Process substitution; `true` means `<(...)`.
    ProcSubst(bool, Rc<Cmd>),
    /// An assignment argument of a declaration builtin (`local a=(1 2)`).
    Assign(Box<Assign>),
}

#[derive(Debug, Clone)]
pub struct ParamExp {
    pub name: String,
    pub index: Option<String>,
    pub indirect: bool,
    pub op: ParamOp,
}

#[derive(Debug, Clone)]
pub enum ParamOp {
    Plain,
    Length,
    Keys,
    Default(bool, Word),
    Assign(bool, Word),
    Alt(bool, Word),
    Error(bool, Word),
    TrimPrefix(bool, Word),
    TrimSuffix(bool, Word),
    Replace { mode: char, pat: Word, rep: Word },
    Substr(String, Option<String>),
    Case { upper: bool, all: bool },
    Transform(char),
}

#[derive(Debug, Clone)]
pub struct Assign {
    pub name: String,
    pub index: Option<String>,
    pub append: bool,
    pub value: AssignVal,
}

#[derive(Debug, Clone)]
pub enum AssignVal {
    Scalar(Word),
    Array(Vec<(Option<String>, Word)>),
}

#[derive(Debug, Clone)]
pub enum RedirKind {
    In,
    Out,
    Clobber,
    Append,
    RW,
    DupIn,
    DupOut,
    HereStr,
    HereDoc(Rc<RefCell<Word>>),
    OutErr,
    AppendErr,
}

#[derive(Debug, Clone)]
pub struct Redir {
    pub fd: Option<i32>,
    pub kind: RedirKind,
    pub target: Word,
}

#[derive(Debug, Clone)]
pub struct SimpleCmd {
    pub assigns: Vec<Assign>,
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct ListItem {
    pub cmd: Cmd,
    pub bg: bool,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaseTerm {
    Break,
    Fall,
    Cont,
}

#[derive(Debug, Clone)]
pub struct FnParam {
    pub name: String,
    pub default: Option<Word>,
    pub rest: bool,
}

#[derive(Debug)]
pub struct Func {
    pub name: String,
    pub params: Option<Vec<FnParam>>,
    pub body: Cmd,
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum CondExpr {
    Unary(String, Word),
    Binary(Word, String, Word),
    Word(Word),
    Not(Box<CondExpr>),
    And(Box<CondExpr>, Box<CondExpr>),
    Or(Box<CondExpr>, Box<CondExpr>),
}

#[derive(Debug, Clone)]
pub enum Cmd {
    Simple(SimpleCmd),
    Pipeline { cmds: Vec<Cmd>, negate: bool, timed: bool },
    AndOr(Box<Cmd>, bool, Box<Cmd>),
    List(Vec<ListItem>),
    Subshell(Box<Cmd>),
    Group(Box<Cmd>),
    If { conds: Vec<(Cmd, Cmd)>, else_: Option<Box<Cmd>> },
    While { cond: Box<Cmd>, body: Box<Cmd>, until: bool },
    For { var: String, words: Option<Vec<Word>>, body: Box<Cmd> },
    CFor { init: String, cond: String, step: String, body: Box<Cmd> },
    Case { word: Word, arms: Vec<(Vec<Word>, Cmd, CaseTerm)> },
    FuncDef(Rc<Func>),
    Cond(CondExpr),
    Arith(String),
    Try { body: Box<Cmd>, catch: Option<Box<Cmd>> },
    Redirected(Box<Cmd>, Vec<Redir>),
}

/// Zero-allocation fast path: if the word is a single unquoted literal part, borrow it.
#[inline]
pub fn word_literal_str(w: &Word) -> Option<&str> {
    match w.as_slice() {
        [WordPart::Lit(s)] => Some(s.as_str()),
        _ => None,
    }
}
