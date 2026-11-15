//! Recursive-descent parser for the nash language (POSIX sh + bash/zsh/nash extensions).
//!
//! The parser works directly on characters and parses one complete command at a
//! time, so aliases defined by one command affect the parsing of the next one.

use crate::ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug)]
pub struct ParseError {
    pub msg: String,
    pub incomplete: bool,
    pub line: usize,
}

type PR<T> = Result<T, ParseError>;

struct PendingHere {
    delim: String,
    strip: bool,
    quoted: bool,
    body: Rc<RefCell<Word>>,
}

pub struct Parser {
    src: Vec<char>,
    pos: usize,
    pub aliases: Rc<HashMap<String, String>>,
    pub interactive: bool,
    pending: Vec<PendingHere>,
    alias_guard: Vec<String>,
    extra_stop: Option<char>,
    regex_mode: bool,
    line_cache: (usize, usize),
}

const CLOSERS: &[&str] = &["then", "else", "elif", "fi", "do", "done", "esac", "}"];
pub const RESERVED: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "case", "esac", "while", "until", "for",
    "in", "function", "fn", "try", "catch", "time", "{", "}", "!", "[[",
];
const DECL: &[&str] = &["declare", "typeset", "local", "export", "readonly"];

fn is_meta(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')')
}

pub fn is_name(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    it.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Parser {
    pub fn new(src: &str) -> Parser {
        Parser {
            src: src.chars().collect(),
            pos: 0,
            aliases: Rc::new(HashMap::new()),
            interactive: false,
            pending: Vec::new(),
            alias_guard: Vec::new(),
            extra_stop: None,
            regex_mode: false,
            line_cache: (0, 1),
        }
    }

    // ---------- low level helpers ----------

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }
    fn peek_at(&self, n: usize) -> Option<char> {
        self.src.get(self.pos + n).copied()
    }
    fn starts(&self, s: &str) -> bool {
        let mut i = self.pos;
        for c in s.chars() {
            if self.src.get(i) != Some(&c) {
                return false;
            }
            i += 1;
        }
        true
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.starts(s) {
            self.pos += s.chars().count();
            true
        } else {
            false
        }
    }
    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }
    fn text(&self, a: usize, b: usize) -> String {
        self.src[a..b.min(self.src.len())].iter().collect()
    }

    pub fn line(&mut self) -> usize {
        let (p, l) = self.line_cache;
        let pos = self.pos.min(self.src.len());
        let (from, mut line) = if pos >= p { (p, l) } else { (0, 1) };
        for c in &self.src[from..pos] {
            if *c == '\n' {
                line += 1;
            }
        }
        self.line_cache = (pos, line);
        line
    }

    fn err<T>(&mut self, msg: &str) -> PR<T> {
        let incomplete = self.at_end();
        Err(ParseError { msg: msg.to_string(), incomplete, line: self.line() })
    }

    fn unexpected<T>(&mut self) -> PR<T> {
        if self.at_end() {
            return self.err("unexpected end of input");
        }
        let mut t = self.peek_token();
        if t.is_empty() {
            let c = self.peek().unwrap();
            t = if c == '\n' { "newline".to_string() } else { c.to_string() };
            if self.starts(";;") {
                t = ";;".into();
            }
        }
        self.err(&format!("syntax error near unexpected token `{}'", t))
    }

    /// Skip blanks, line continuations and comments.
    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') => self.pos += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => self.pos += 2,
                Some('#') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn consume_newline(&mut self) -> PR<()> {
        self.pos += 1;
        self.read_heredocs()
    }

    fn skip_newlines(&mut self) -> PR<()> {
        loop {
            self.skip_ws();
            if self.peek() == Some('\n') {
                self.consume_newline()?;
            } else {
                return Ok(());
            }
        }
    }

    fn read_heredocs(&mut self) -> PR<()> {
        let pending = std::mem::take(&mut self.pending);
        for h in pending {
            let mut body = String::new();
            loop {
                if self.at_end() {
                    if self.interactive {
                        return self.err("unterminated here-document");
                    }
                    break;
                }
                let start = self.pos;
                while let Some(c) = self.peek() {
                    if c == '\n' {
                        break;
                    }
                    self.pos += 1;
                }
                let mut line = self.text(start, self.pos);
                let had_nl = self.peek() == Some('\n');
                if had_nl {
                    self.pos += 1;
                }
                if h.strip {
                    line = line.trim_start_matches('\t').to_string();
                }
                if line == h.delim {
                    break;
                }
                body.push_str(&line);
                body.push('\n');
                if !had_nl && self.interactive {
                    return self.err("unterminated here-document");
                }
            }
            let word = if h.quoted {
                vec![WordPart::Quoted(body)]
            } else {
                let mut sub = Parser::new(&body);
                sub.aliases = self.aliases.clone();
                vec![WordPart::DQuote(sub.parse_dquote_body(None)?)]
            };
            *h.body.borrow_mut() = word;
        }
        Ok(())
    }

    /// The raw token at the current position (up to whitespace or a metacharacter).
    fn peek_token(&self) -> String {
        let mut i = self.pos;
        let mut s = String::new();
        while let Some(&c) = self.src.get(i) {
            if is_meta(c) {
                break;
            }
            s.push(c);
            i += 1;
        }
        s
    }

    fn expect_tok(&mut self, w: &str) -> PR<()> {
        self.skip_newlines()?;
        if self.peek_token() == w {
            self.pos += w.chars().count();
            Ok(())
        } else if self.at_end() {
            self.err(&format!("expected `{}'", w))
        } else {
            let t = self.peek_token();
            self.err(&format!("syntax error: expected `{}' but found `{}'", w, if t.is_empty() { self.peek().unwrap().to_string() } else { t }))
        }
    }

    // ---------- words ----------

    fn read_squote(&mut self) -> PR<String> {
        self.pos += 1;
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '\'' {
                let s = self.text(start, self.pos);
                self.pos += 1;
                return Ok(s);
            }
            self.pos += 1;
        }
        self.err("unterminated single quote")
    }

    fn read_ansi_c(&mut self) -> PR<String> {
        // positioned after $'
        let mut out = String::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return self.err("unterminated $' quote"),
            };
            self.pos += 1;
            if c == '\'' {
                return Ok(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let e = match self.peek() {
                Some(e) => e,
                None => return self.err("unterminated $' quote"),
            };
            self.pos += 1;
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'a' => out.push('\x07'),
                'b' => out.push('\x08'),
                'e' | 'E' => out.push('\x1b'),
                'f' => out.push('\x0c'),
                'v' => out.push('\x0b'),
                'x' | 'u' | 'U' => {
                    let max = match e {
                        'x' => 2,
                        'u' => 4,
                        _ => 8,
                    };
                    let mut v = 0u32;
                    let mut n = 0;
                    while n < max {
                        match self.peek().and_then(|c| c.to_digit(16)) {
                            Some(d) => {
                                v = v * 16 + d;
                                self.pos += 1;
                                n += 1;
                            }
                            None => break,
                        }
                    }
                    if n == 0 {
                        out.push('\\');
                        out.push(e);
                    } else if let Some(ch) = char::from_u32(v) {
                        out.push(ch);
                    }
                }
                '0'..='7' => {
                    let mut v = e.to_digit(8).unwrap();
                    let mut n = 1;
                    while n < 3 {
                        match self.peek().and_then(|c| c.to_digit(8)) {
                            Some(d) => {
                                v = v * 8 + d;
                                self.pos += 1;
                                n += 1;
                            }
                            None => break,
                        }
                    }
                    if let Some(ch) = char::from_u32(v) {
                        out.push(ch);
                    }
                }
                'c' => {
                    if let Some(x) = self.peek() {
                        self.pos += 1;
                        out.push(((x as u8) & 0x1f) as char);
                    }
                }
                other => {
                    if !matches!(other, '\\' | '\'' | '"' | '?') {
                        out.push('\\');
                    }
                    out.push(other);
                }
            }
        }
    }

    /// Parse the inside of a double-quoted string. With `end == None` the whole
    /// remaining input is taken (used for here-documents and runtime strings).
    pub fn parse_dquote_body(&mut self, end: Option<char>) -> PR<Vec<WordPart>> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => {
                    if end.is_some() {
                        return self.err("unterminated double quote");
                    }
                    break;
                }
            };
            if Some(c) == end {
                self.pos += 1;
                break;
            }
            match c {
                '\\' => {
                    self.pos += 1;
                    match self.peek() {
                        Some('\n') => self.pos += 1,
                        Some(n) if n == '$' || n == '`' || n == '\\' || (n == '"' && end.is_some()) => {
                            lit.push(n);
                            self.pos += 1;
                        }
                        _ => lit.push('\\'),
                    }
                }
                '$' => {
                    if !lit.is_empty() {
                        parts.push(WordPart::Lit(std::mem::take(&mut lit)));
                    }
                    match self.parse_dollar(true)? {
                        Some(p) => parts.push(p),
                        None => lit.push('$'),
                    }
                }
                '`' => {
                    if !lit.is_empty() {
                        parts.push(WordPart::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(self.parse_backtick(true)?);
                }
                _ => {
                    lit.push(c);
                    self.pos += 1;
                }
            }
        }
        if !lit.is_empty() {
            parts.push(WordPart::Lit(lit));
        }
        Ok(parts)
    }

    fn sub_parser(&self, text: &str) -> Parser {
        let mut p = Parser::new(text);
        p.aliases = self.aliases.clone();
        p
    }

    fn parse_backtick(&mut self, in_dq: bool) -> PR<WordPart> {
        self.pos += 1;
        let mut text = String::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return self.err("unterminated backquote"),
            };
            self.pos += 1;
            if c == '`' {
                break;
            }
            if c == '\\' {
                match self.peek() {
                    Some(n) if n == '$' || n == '`' || n == '\\' || (in_dq && n == '"') => {
                        text.push(n);
                        self.pos += 1;
                    }
                    _ => text.push('\\'),
                }
            } else {
                text.push(c);
            }
        }
        let mut sub = self.sub_parser(&text);
        let cmd = sub.parse_all()?;
        Ok(WordPart::CmdSubst(Rc::new(cmd)))
    }

    /// Find the `))` closing an arithmetic expression starting at `start`.
    /// Ok(Some(i)) = index of the first `)`; Ok(None) = not arithmetic; Err = hit EOF.
    fn scan_arith(&self, start: usize) -> Result<Option<usize>, ()> {
        let mut depth = 0;
        let mut i = start;
        while i < self.src.len() {
            match self.src[i] {
                '(' => depth += 1,
                ')' => {
                    if depth == 0 {
                        return Ok(if self.src.get(i + 1) == Some(&')') { Some(i) } else { None });
                    }
                    depth -= 1;
                }
                _ => {}
            }
            i += 1;
        }
        Err(())
    }

    fn parse_dollar(&mut self, in_dq: bool) -> PR<Option<WordPart>> {
        // at '$'
        let n = match self.peek_at(1) {
            Some(n) => n,
            None => {
                self.pos += 1;
                return Ok(None);
            }
        };
        if n == '(' {
            if self.peek_at(2) == Some('(') {
                match self.scan_arith(self.pos + 3) {
                    Ok(Some(end)) => {
                        let raw = self.text(self.pos + 3, end);
                        self.pos = end + 2;
                        return Ok(Some(WordPart::Arith(raw)));
                    }
                    Ok(None) => {}
                    Err(()) => {
                        self.pos = self.src.len();
                        return self.err("unterminated arithmetic expansion");
                    }
                }
            }
            self.pos += 2;
            let cmd = self.parse_nested()?;
            return Ok(Some(WordPart::CmdSubst(Rc::new(cmd))));
        }
        if n == '{' {
            return Ok(Some(self.parse_braced_param(in_dq)?));
        }
        if n == '\'' && !in_dq {
            self.pos += 2;
            return Ok(Some(WordPart::Quoted(self.read_ansi_c()?)));
        }
        if n == '"' && !in_dq {
            self.pos += 2;
            return Ok(Some(WordPart::DQuote(self.parse_dquote_body(Some('"'))?)));
        }
        let plain = |name: String| WordPart::Param(Box::new(ParamExp { name, index: None, indirect: false, op: ParamOp::Plain }));
        if n.is_ascii_alphabetic() || n == '_' {
            self.pos += 1;
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            return Ok(Some(plain(self.text(start, self.pos))));
        }
        if n.is_ascii_digit() || matches!(n, '@' | '*' | '#' | '?' | '$' | '!' | '-') {
            self.pos += 2;
            return Ok(Some(plain(n.to_string())));
        }
        self.pos += 1;
        Ok(None)
    }

    /// Parse a command list nested in `$(`, `<(` ... up to and including `)`.
    fn parse_nested(&mut self) -> PR<Cmd> {
        let saved = std::mem::take(&mut self.extra_stop);
        let saved_re = std::mem::replace(&mut self.regex_mode, false);
        let cmd = self.parse_list(false)?;
        self.skip_newlines()?;
        self.extra_stop = saved;
        self.regex_mode = saved_re;
        if !self.eat(")") {
            if self.at_end() {
                return self.err("unterminated command substitution");
            }
            return self.unexpected();
        }
        Ok(cmd)
    }

    fn read_balanced(&mut self, open: char, close: char) -> PR<String> {
        // positioned after the opening char; consumes the closing char
        let start = self.pos;
        let mut depth = 0;
        while let Some(c) = self.peek() {
            if c == '\\' {
                self.pos += 2;
                continue;
            }
            if c == open {
                depth += 1;
            } else if c == close {
                if depth == 0 {
                    let s = self.text(start, self.pos);
                    self.pos += 1;
                    return Ok(s);
                }
                depth -= 1;
            }
            self.pos += 1;
        }
        self.pos = self.src.len();
        self.err(&format!("missing `{}'", close))
    }

    fn parse_braced_param(&mut self, in_dq: bool) -> PR<WordPart> {
        self.pos += 2;
        let mut length = false;
        let mut indirect = false;
        if self.peek() == Some('#') && !matches!(self.peek_at(1), Some('}') | None) {
            length = true;
            self.pos += 1;
        } else if self.peek() == Some('!') && !matches!(self.peek_at(1), Some('}') | None) {
            indirect = true;
            self.pos += 1;
        }
        let start = self.pos;
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            Some(c) if c.is_ascii_digit() => {
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            Some(c) if matches!(c, '@' | '*' | '#' | '?' | '$' | '!' | '-') => self.pos += 1,
            _ => return self.err("bad substitution"),
        }
        let name = self.text(start, self.pos);
        let mut index = None;
        if self.peek() == Some('[') {
            self.pos += 1;
            index = Some(self.read_balanced('[', ']')?);
        }
        let mut op = ParamOp::Plain;
        if indirect && matches!(index.as_deref(), Some("@") | Some("*")) {
            indirect = false;
            op = ParamOp::Keys;
        }
        if length {
            op = ParamOp::Length;
        }
        let c = match self.peek() {
            Some(c) => c,
            None => return self.err("unterminated ${"),
        };
        if c != '}' {
            if length || matches!(op, ParamOp::Keys) {
                return self.err("bad substitution");
            }
            let two = self.peek_at(1);
            match c {
                ':' if matches!(two, Some('-') | Some('=') | Some('+') | Some('?')) => {
                    self.pos += 2;
                    let w = self.parse_brace_word(in_dq, false, in_dq)?;
                    op = match two.unwrap() {
                        '-' => ParamOp::Default(true, w),
                        '=' => ParamOp::Assign(true, w),
                        '+' => ParamOp::Alt(true, w),
                        _ => ParamOp::Error(true, w),
                    };
                }
                ':' => {
                    self.pos += 1;
                    let mut off = String::new();
                    let mut len = None;
                    let mut depth = 0;
                    loop {
                        match self.peek() {
                            None => return self.err("unterminated ${"),
                            Some('}') if depth == 0 => break,
                            Some(':') if depth == 0 && len.is_none() => {
                                len = Some(String::new());
                            }
                            Some(ch) => {
                                if ch == '(' || ch == '{' {
                                    depth += 1;
                                } else if (ch == ')' || ch == '}') && depth > 0 {
                                    depth -= 1;
                                }
                                match len.as_mut() {
                                    Some(l) => l.push(ch),
                                    None => off.push(ch),
                                }
                            }
                        }
                        self.pos += 1;
                    }
                    op = ParamOp::Substr(off, len);
                }
                '-' | '=' | '+' | '?' => {
                    self.pos += 1;
                    let w = self.parse_brace_word(in_dq, false, in_dq)?;
                    op = match c {
                        '-' => ParamOp::Default(false, w),
                        '=' => ParamOp::Assign(false, w),
                        '+' => ParamOp::Alt(false, w),
                        _ => ParamOp::Error(false, w),
                    };
                }
                '#' | '%' => {
                    self.pos += 1;
                    let long = self.peek() == Some(c);
                    if long {
                        self.pos += 1;
                    }
                    let w = self.parse_brace_word(false, false, false)?;
                    op = if c == '#' { ParamOp::TrimPrefix(long, w) } else { ParamOp::TrimSuffix(long, w) };
                }
                '/' => {
                    self.pos += 1;
                    let mode = match self.peek() {
                        Some('/') => {
                            self.pos += 1;
                            'a'
                        }
                        Some('#') => {
                            self.pos += 1;
                            '#'
                        }
                        Some('%') => {
                            self.pos += 1;
                            '%'
                        }
                        _ => '/',
                    };
                    let pat = self.parse_brace_word(false, true, false)?;
                    let rep = if self.peek() == Some('/') {
                        self.pos += 1;
                        self.parse_brace_word(in_dq, false, in_dq)?
                    } else {
                        Vec::new()
                    };
                    op = ParamOp::Replace { mode, pat, rep };
                }
                '^' | ',' => {
                    self.pos += 1;
                    let all = self.peek() == Some(c);
                    if all {
                        self.pos += 1;
                    }
                    op = ParamOp::Case { upper: c == '^', all };
                }
                '@' => {
                    self.pos += 1;
                    match self.peek() {
                        Some(t) if t.is_ascii_alphabetic() => {
                            self.pos += 1;
                            op = ParamOp::Transform(t);
                        }
                        _ => return self.err("bad substitution"),
                    }
                }
                _ => return self.err("bad substitution"),
            }
        }
        if !self.eat("}") {
            return self.err("bad substitution: missing `}'");
        }
        Ok(WordPart::Param(Box::new(ParamExp { name, index, indirect, op })))
    }

    /// Parse the operand of a `${x<op>word}` expansion, up to (not including) `}`.
    fn parse_brace_word(&mut self, in_dq: bool, stop_slash: bool, wrap: bool) -> PR<Word> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        macro_rules! flush {
            () => {
                if !lit.is_empty() {
                    parts.push(WordPart::Lit(std::mem::take(&mut lit)));
                }
            };
        }
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return self.err("unterminated ${"),
            };
            match c {
                '}' => break,
                '/' if stop_slash => break,
                '\\' => {
                    self.pos += 1;
                    match self.peek() {
                        None => lit.push('\\'),
                        Some('\n') => self.pos += 1,
                        Some(n) => {
                            if in_dq && !matches!(n, '$' | '`' | '"' | '\\' | '}') {
                                lit.push('\\');
                                lit.push(n);
                            } else {
                                flush!();
                                parts.push(WordPart::Quoted(n.to_string()));
                            }
                            self.pos += 1;
                        }
                    }
                }
                '\'' if !in_dq => {
                    flush!();
                    parts.push(WordPart::Quoted(self.read_squote()?));
                }
                '"' => {
                    flush!();
                    self.pos += 1;
                    parts.push(WordPart::DQuote(self.parse_dquote_body(Some('"'))?));
                }
                '$' => {
                    flush!();
                    match self.parse_dollar(in_dq)? {
                        Some(p) => parts.push(p),
                        None => lit.push('$'),
                    }
                }
                '`' => {
                    flush!();
                    parts.push(self.parse_backtick(in_dq)?);
                }
                _ => {
                    lit.push(c);
                    self.pos += 1;
                }
            }
        }
        flush!();
        if wrap {
            Ok(vec![WordPart::DQuote(parts)])
        } else {
            Ok(parts)
        }
    }

    fn parse_word(&mut self) -> PR<Word> {
        self.parse_word_opt(false)
    }

    /// Parse a word. Returns an empty word if there is none at the current position.
    fn parse_word_opt(&mut self, assign: bool) -> PR<Word> {
        let mut parts: Word = Vec::new();
        let mut lit = String::new();
        let start = self.pos;
        macro_rules! flush {
            () => {
                if !lit.is_empty() {
                    parts.push(WordPart::Lit(std::mem::take(&mut lit)));
                }
            };
        }
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => break,
            };
            if Some(c) == self.extra_stop {
                break;
            }
            match c {
                ' ' | '\t' | '\n' => break,
                ';' | '&' | '|' | ')' | '(' | '<' | '>' if self.regex_mode => {
                    lit.push(c);
                    self.pos += 1;
                }
                ';' | '&' | '|' | ')' => break,
                '<' | '>' => {
                    if self.peek_at(1) == Some('(') {
                        flush!();
                        self.pos += 2;
                        let cmd = self.parse_nested()?;
                        parts.push(WordPart::ProcSubst(c == '<', Rc::new(cmd)));
                    } else {
                        break;
                    }
                }
                '(' => {
                    if lit.ends_with(['?', '*', '+', '@', '!']) {
                        self.pos += 1;
                        let inner = self.read_balanced('(', ')')?;
                        lit.push('(');
                        lit.push_str(&inner);
                        lit.push(')');
                    } else {
                        break;
                    }
                }
                '\\' => {
                    self.pos += 1;
                    match self.peek() {
                        None => lit.push('\\'),
                        Some('\n') => self.pos += 1,
                        Some(n) => {
                            flush!();
                            parts.push(WordPart::Quoted(n.to_string()));
                            self.pos += 1;
                        }
                    }
                }
                '\'' => {
                    flush!();
                    parts.push(WordPart::Quoted(self.read_squote()?));
                }
                '"' => {
                    flush!();
                    self.pos += 1;
                    parts.push(WordPart::DQuote(self.parse_dquote_body(Some('"'))?));
                }
                '`' => {
                    flush!();
                    parts.push(self.parse_backtick(false)?);
                }
                '$' => {
                    flush!();
                    match self.parse_dollar(false)? {
                        Some(p) => parts.push(p),
                        None => lit.push('$'),
                    }
                }
                '~' if self.pos == start || (assign && parts.iter().all(|p| matches!(p, WordPart::Lit(_))) && (lit.ends_with(':') || (lit.is_empty() && parts.is_empty()))) => {
                    let mut i = self.pos + 1;
                    while let Some(&ch) = self.src.get(i) {
                        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | '+') {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    let next = self.src.get(i).copied();
                    let ok = match next {
                        None => true,
                        Some(ch) => ch == '/' || is_meta(ch) || (assign && ch == ':'),
                    };
                    if ok {
                        flush!();
                        parts.push(WordPart::Tilde(self.text(self.pos + 1, i)));
                        self.pos = i;
                    } else {
                        lit.push('~');
                        self.pos += 1;
                    }
                }
                _ => {
                    lit.push(c);
                    self.pos += 1;
                }
            }
        }
        flush!();
        Ok(parts)
    }

    // ---------- redirections ----------

    fn parse_redir(&mut self) -> PR<Option<Redir>> {
        let save = self.pos;
        let mut fd = None;
        let mut i = self.pos;
        while i < self.src.len() && self.src[i].is_ascii_digit() {
            i += 1;
        }
        if i > self.pos && i < self.src.len() && (self.src[i] == '<' || self.src[i] == '>') {
            fd = self.text(self.pos, i).parse::<i32>().ok();
            self.pos = i;
        }
        let c = match self.peek() {
            Some(c) => c,
            None => {
                self.pos = save;
                return Ok(None);
            }
        };
        let mut heredoc = None;
        let kind = if c == '&' && fd.is_none() && self.peek_at(1) == Some('>') {
            self.pos += 2;
            if self.eat(">") {
                RedirKind::AppendErr
            } else {
                RedirKind::OutErr
            }
        } else if c == '<' && self.peek_at(1) != Some('(') {
            self.pos += 1;
            if self.eat("<<") {
                RedirKind::HereStr
            } else if self.eat("<-") {
                heredoc = Some(true);
                RedirKind::In
            } else if self.eat("<") {
                heredoc = Some(false);
                RedirKind::In
            } else if self.eat("&") {
                RedirKind::DupIn
            } else if self.eat(">") {
                RedirKind::RW
            } else {
                RedirKind::In
            }
        } else if c == '>' && self.peek_at(1) != Some('(') {
            self.pos += 1;
            if self.eat(">") {
                RedirKind::Append
            } else if self.eat("&") {
                RedirKind::DupOut
            } else if self.eat("|") {
                RedirKind::Clobber
            } else {
                RedirKind::Out
            }
        } else {
            self.pos = save;
            return Ok(None);
        };
        self.skip_ws();
        let target = self.parse_word()?;
        if target.is_empty() {
            return self.unexpected();
        }
        if let Some(strip) = heredoc {
            let mut delim = String::new();
            let mut quoted = false;
            fn flat(parts: &[WordPart], out: &mut String, quoted: &mut bool) {
                for p in parts {
                    match p {
                        WordPart::Lit(s) => out.push_str(s),
                        WordPart::Quoted(s) => {
                            *quoted = true;
                            out.push_str(s)
                        }
                        WordPart::DQuote(inner) => {
                            *quoted = true;
                            flat(inner, out, quoted)
                        }
                        _ => {}
                    }
                }
            }
            flat(&target, &mut delim, &mut quoted);
            let body = Rc::new(RefCell::new(Vec::new()));
            self.pending.push(PendingHere { delim, strip, quoted, body: body.clone() });
            return Ok(Some(Redir { fd, kind: RedirKind::HereDoc(body), target: Vec::new() }));
        }
        Ok(Some(Redir { fd, kind, target }))
    }

    // ---------- commands ----------

    /// Parse the whole input as a list of commands.
    pub fn parse_all(&mut self) -> PR<Cmd> {
        let mut items = Vec::new();
        while let Some(c) = self.parse_program_command()? {
            items.push(ListItem { cmd: c, bg: false, text: None });
        }
        Ok(Cmd::List(items))
    }

    /// Parse the next complete command (one logical line). `None` at end of input.
    pub fn parse_program_command(&mut self) -> PR<Option<Cmd>> {
        self.skip_newlines()?;
        if self.at_end() {
            return Ok(None);
        }
        let start = self.pos;
        let cmd = self.parse_list(true)?;
        if self.pos == start {
            return self.unexpected();
        }
        if !self.pending.is_empty() {
            // here-doc on the last line without trailing newline
            self.read_heredocs()?;
        }
        Ok(Some(cmd))
    }

    fn at_list_end(&self) -> bool {
        match self.peek() {
            None | Some(')') => true,
            Some(';') => self.starts(";;") || self.starts(";&"),
            _ => {
                let t = self.peek_token();
                CLOSERS.contains(&t.as_str())
            }
        }
    }

    fn parse_list(&mut self, top: bool) -> PR<Cmd> {
        let mut items: Vec<ListItem> = Vec::new();
        loop {
            if top {
                self.skip_ws();
                if self.peek() == Some('\n') {
                    self.consume_newline()?;
                    break;
                }
            } else {
                self.skip_newlines()?;
            }
            if self.at_list_end() {
                break;
            }
            let start = self.pos;
            let cmd = self.parse_and_or()?;
            let end = self.pos;
            self.skip_ws();
            let mut bg = false;
            let mut done = false;
            if self.starts(";;") || self.starts(";&") {
                done = true;
            } else {
                match self.peek() {
                    Some(';') => self.pos += 1,
                    Some('&') => {
                        self.pos += 1;
                        bg = true;
                    }
                    Some('\n') => {
                        self.consume_newline()?;
                        if top {
                            done = true;
                        }
                    }
                    _ => done = true,
                }
            }
            let text = if bg || top { Some(self.text(start, end).trim().to_string()) } else { None };
            items.push(ListItem { cmd, bg, text });
            if done {
                break;
            }
        }
        if items.len() == 1 && !items[0].bg && !top {
            return Ok(items.pop().unwrap().cmd);
        }
        Ok(Cmd::List(items))
    }

    fn parse_and_or(&mut self) -> PR<Cmd> {
        let mut left = self.parse_pipeline()?;
        loop {
            self.skip_ws();
            let and = if self.eat("&&") {
                true
            } else if self.eat("||") {
                false
            } else {
                break;
            };
            self.skip_newlines()?;
            let right = self.parse_pipeline()?;
            left = Cmd::AndOr(Box::new(left), and, Box::new(right));
        }
        Ok(left)
    }

    fn parse_pipeline(&mut self) -> PR<Cmd> {
        let mut negate = false;
        let mut timed = false;
        loop {
            self.skip_ws();
            let t = self.peek_token();
            if t == "!" {
                self.pos += 1;
                negate = !negate;
            } else if t == "time" && !timed {
                self.pos += 4;
                timed = true;
                self.skip_ws();
                if self.peek_token() == "-p" {
                    self.pos += 2;
                }
            } else {
                break;
            }
        }
        if timed {
            self.skip_ws();
            if matches!(self.peek(), None | Some('\n') | Some(';')) {
                return Ok(Cmd::Pipeline { cmds: vec![], negate, timed });
            }
        }
        let mut cmds = vec![self.parse_command()?];
        loop {
            self.skip_ws();
            if self.peek() == Some('|') && self.peek_at(1) != Some('|') {
                self.pos += 1;
                if self.eat("&") {
                    let last = cmds.pop().unwrap();
                    let dup = Redir { fd: Some(2), kind: RedirKind::DupOut, target: vec![WordPart::Lit("1".into())] };
                    cmds.push(Cmd::Redirected(Box::new(last), vec![dup]));
                }
                self.skip_newlines()?;
                cmds.push(self.parse_command()?);
            } else {
                break;
            }
        }
        if cmds.len() == 1 && !negate && !timed {
            return Ok(cmds.pop().unwrap());
        }
        Ok(Cmd::Pipeline { cmds, negate, timed })
    }

    fn parse_command(&mut self) -> PR<Cmd> {
        self.skip_ws();
        let tok = self.peek_token();
        let start = self.pos;
        let cmd = match tok.as_str() {
            "{" => {
                self.pos += 1;
                let body = self.parse_list(false)?;
                self.expect_tok("}")?;
                Cmd::Group(Box::new(body))
            }
            "if" => {
                self.pos += 2;
                let mut conds = Vec::new();
                let mut else_ = None;
                loop {
                    let c = self.parse_list(false)?;
                    self.expect_tok("then")?;
                    let b = self.parse_list(false)?;
                    conds.push((c, b));
                    self.skip_newlines()?;
                    let t = self.peek_token();
                    if t == "elif" {
                        self.pos += 4;
                        continue;
                    }
                    if t == "else" {
                        self.pos += 4;
                        else_ = Some(Box::new(self.parse_list(false)?));
                    }
                    self.expect_tok("fi")?;
                    break;
                }
                Cmd::If { conds, else_ }
            }
            "while" | "until" => {
                self.pos += 5;
                let cond = self.parse_list(false)?;
                self.expect_tok("do")?;
                let body = self.parse_list(false)?;
                self.expect_tok("done")?;
                Cmd::While { cond: Box::new(cond), body: Box::new(body), until: tok == "until" }
            }
            "for" => {
                self.pos += 3;
                self.skip_ws();
                if self.starts("((") {
                    let end = match self.scan_arith(self.pos + 2) {
                        Ok(Some(e)) => e,
                        _ => {
                            self.pos = self.src.len();
                            return self.err("unterminated for ((");
                        }
                    };
                    let raw = self.text(self.pos + 2, end);
                    self.pos = end + 2;
                    let mut it = raw.splitn(3, ';');
                    let init = it.next().unwrap_or("").to_string();
                    let cond = it.next().unwrap_or("").to_string();
                    let step = it.next().unwrap_or("").to_string();
                    self.skip_ws();
                    self.eat(";");
                    let body = self.parse_loop_body()?;
                    Cmd::CFor { init, cond, step, body: Box::new(body) }
                } else {
                    let var = self.peek_token();
                    if !is_name(&var) {
                        return self.err(&format!("`{}': not a valid identifier", var));
                    }
                    self.pos += var.chars().count();
                    self.skip_ws();
                    self.eat(";");
                    self.skip_newlines()?;
                    let mut words = None;
                    if self.peek_token() == "in" {
                        self.pos += 2;
                        let mut ws = Vec::new();
                        loop {
                            self.skip_ws();
                            match self.peek() {
                                None | Some('\n') | Some(';') => break,
                                _ => {
                                    let w = self.parse_word()?;
                                    if w.is_empty() {
                                        return self.unexpected();
                                    }
                                    ws.push(w);
                                }
                            }
                        }
                        self.eat(";");
                        words = Some(ws);
                    }
                    let body = self.parse_loop_body()?;
                    Cmd::For { var, words, body: Box::new(body) }
                }
            }
            "case" => {
                self.pos += 4;
                self.skip_ws();
                let word = self.parse_word()?;
                if word.is_empty() {
                    return self.unexpected();
                }
                self.expect_tok("in")?;
                let mut arms = Vec::new();
                loop {
                    self.skip_newlines()?;
                    if self.peek_token() == "esac" {
                        self.pos += 4;
                        break;
                    }
                    if self.at_end() {
                        return self.err("expected `esac'");
                    }
                    self.eat("(");
                    let mut pats = Vec::new();
                    loop {
                        self.skip_ws();
                        let w = self.parse_word()?;
                        if w.is_empty() {
                            return self.unexpected();
                        }
                        pats.push(w);
                        self.skip_ws();
                        if !self.eat("|") {
                            break;
                        }
                    }
                    if !self.eat(")") {
                        return self.unexpected();
                    }
                    let body = self.parse_list(false)?;
                    self.skip_newlines()?;
                    let term = if self.eat(";;&") {
                        CaseTerm::Cont
                    } else if self.eat(";&") {
                        CaseTerm::Fall
                    } else if self.eat(";;") {
                        CaseTerm::Break
                    } else if self.peek_token() == "esac" {
                        CaseTerm::Break
                    } else {
                        return self.unexpected();
                    };
                    arms.push((pats, body, term));
                }
                Cmd::Case { word, arms }
            }
            "function" => {
                self.pos += 8;
                self.skip_ws();
                let name = self.read_func_name();
                if name.is_empty() {
                    return self.unexpected();
                }
                self.skip_ws();
                if self.eat("(") {
                    self.skip_ws();
                    if !self.eat(")") {
                        return self.unexpected();
                    }
                }
                self.skip_newlines()?;
                let body = self.parse_command()?;
                let text = self.text(start, self.pos);
                Cmd::FuncDef(Rc::new(Func { name, params: None, body, text }))
            }
            "fn" => {
                self.pos += 2;
                self.skip_ws();
                let name = self.read_func_name();
                if name.is_empty() {
                    return self.unexpected();
                }
                self.skip_ws();
                let mut params = None;
                if self.eat("(") {
                    params = Some(self.parse_fn_params()?);
                }
                self.skip_newlines()?;
                let body = self.parse_command()?;
                let text = self.text(start, self.pos);
                Cmd::FuncDef(Rc::new(Func { name, params, body, text }))
            }
            "try" => {
                self.pos += 3;
                self.skip_newlines()?;
                let body = self.parse_command()?;
                let save = self.pos;
                if self.pending.is_empty() {
                    self.skip_newlines()?;
                } else {
                    self.skip_ws();
                }
                let catch = if self.peek_token() == "catch" {
                    self.pos += 5;
                    self.skip_newlines()?;
                    Some(Box::new(self.parse_command()?))
                } else {
                    self.pos = save;
                    None
                };
                Cmd::Try { body: Box::new(body), catch }
            }
            "[[" => {
                self.pos += 2;
                let e = self.cond_or()?;
                self.cond_ws();
                if !self.eat("]]") {
                    return self.err("expected `]]'");
                }
                Cmd::Cond(e)
            }
            _ => {
                if CLOSERS.contains(&tok.as_str()) {
                    return self.unexpected();
                }
                if self.peek() == Some('(') {
                    let mut arith = None;
                    if self.starts("((") {
                        match self.scan_arith(self.pos + 2) {
                            Ok(Some(end)) => arith = Some(end),
                            Ok(None) => {}
                            Err(()) => {
                                self.pos = self.src.len();
                                return self.err("unterminated ((");
                            }
                        }
                    }
                    if let Some(end) = arith {
                        let raw = self.text(self.pos + 2, end);
                        self.pos = end + 2;
                        Cmd::Arith(raw)
                    } else {
                        self.pos += 1;
                        let body = self.parse_list(false)?;
                        self.skip_newlines()?;
                        if !self.eat(")") {
                            if self.at_end() {
                                return self.err("expected `)'");
                            }
                            return self.unexpected();
                        }
                        Cmd::Subshell(Box::new(body))
                    }
                } else if let Some(name) = self.try_func_header() {
                    self.skip_newlines()?;
                    let body = self.parse_command()?;
                    let text = self.text(start, self.pos);
                    return Ok(Cmd::FuncDef(Rc::new(Func { name, params: None, body, text })));
                } else {
                    return self.parse_simple();
                }
            }
        };
        // trailing redirections on compound commands
        let mut redirs = Vec::new();
        loop {
            self.skip_ws();
            match self.parse_redir()? {
                Some(r) => redirs.push(r),
                None => break,
            }
        }
        if redirs.is_empty() {
            Ok(cmd)
        } else {
            Ok(Cmd::Redirected(Box::new(cmd), redirs))
        }
    }

    fn parse_loop_body(&mut self) -> PR<Cmd> {
        self.skip_newlines()?;
        if self.peek_token() == "{" {
            self.pos += 1;
            let body = self.parse_list(false)?;
            self.expect_tok("}")?;
            return Ok(body);
        }
        self.expect_tok("do")?;
        let body = self.parse_list(false)?;
        self.expect_tok("done")?;
        Ok(body)
    }

    fn read_func_name(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if is_meta(c) || matches!(c, '"' | '\'' | '$' | '`' | '\\' | '{' | '}') {
                break;
            }
            self.pos += 1;
        }
        self.text(start, self.pos)
    }

    /// Detect `name ( )` and consume it.
    fn try_func_header(&mut self) -> Option<String> {
        let mut i = self.pos;
        while let Some(&c) = self.src.get(i) {
            if is_meta(c) || matches!(c, '"' | '\'' | '$' | '`' | '\\' | '=' | '{' | '}') {
                break;
            }
            i += 1;
        }
        if i == self.pos {
            return None;
        }
        let name = self.text(self.pos, i);
        while matches!(self.src.get(i), Some(' ') | Some('\t')) {
            i += 1;
        }
        if self.src.get(i) != Some(&'(') {
            return None;
        }
        i += 1;
        while matches!(self.src.get(i), Some(' ') | Some('\t')) {
            i += 1;
        }
        if self.src.get(i) != Some(&')') {
            return None;
        }
        self.pos = i + 1;
        Some(name)
    }

    fn parse_fn_params(&mut self) -> PR<Vec<FnParam>> {
        let mut params = Vec::new();
        loop {
            self.skip_newlines()?;
            if self.eat(")") {
                break;
            }
            let rest = self.eat("...");
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let name = self.text(start, self.pos);
            if !is_name(&name) {
                return self.err("fn: expected parameter name");
            }
            let mut default = None;
            if self.eat("=") {
                self.extra_stop = Some(',');
                let w = self.parse_word();
                self.extra_stop = None;
                default = Some(w?);
            }
            params.push(FnParam { name, default, rest });
            self.skip_newlines()?;
            if self.eat(",") {
                continue;
            }
            if self.eat(")") {
                break;
            }
            return self.err("fn: expected `,' or `)' in parameter list");
        }
        Ok(params)
    }

    fn try_parse_assign(&mut self) -> PR<Option<Assign>> {
        let mut i = self.pos;
        match self.src.get(i) {
            Some(c) if c.is_ascii_alphabetic() || *c == '_' => {}
            _ => return Ok(None),
        }
        while let Some(&c) = self.src.get(i) {
            if c.is_ascii_alphanumeric() || c == '_' {
                i += 1;
            } else {
                break;
            }
        }
        let name = self.text(self.pos, i);
        let mut index = None;
        if self.src.get(i) == Some(&'[') {
            let mut depth = 0;
            let mut j = i + 1;
            let mut found = false;
            while let Some(&c) = self.src.get(j) {
                if c == '[' {
                    depth += 1;
                } else if c == ']' {
                    if depth == 0 {
                        found = true;
                        break;
                    }
                    depth -= 1;
                } else if c == '\n' {
                    break;
                }
                j += 1;
            }
            if !found {
                return Ok(None);
            }
            index = Some(self.text(i + 1, j));
            i = j + 1;
        }
        let mut append = false;
        if self.src.get(i) == Some(&'+') {
            append = true;
            i += 1;
        }
        if self.src.get(i) != Some(&'=') {
            return Ok(None);
        }
        self.pos = i + 1;
        if self.peek() == Some('(') && index.is_none() {
            self.pos += 1;
            let mut elems = Vec::new();
            loop {
                self.skip_newlines()?;
                if self.eat(")") {
                    break;
                }
                if self.at_end() {
                    return self.err("unterminated array literal");
                }
                let mut key = None;
                if self.peek() == Some('[') {
                    let save = self.pos;
                    self.pos += 1;
                    match self.read_balanced('[', ']') {
                        Ok(k) if self.peek() == Some('=') => {
                            self.pos += 1;
                            key = Some(k);
                        }
                        _ => self.pos = save,
                    }
                }
                let w = self.parse_word()?;
                if w.is_empty() && key.is_none() {
                    return self.unexpected();
                }
                elems.push((key, w));
            }
            return Ok(Some(Assign { name, index, append, value: AssignVal::Array(elems) }));
        }
        let w = self.parse_word_opt(true)?;
        Ok(Some(Assign { name, index, append, value: AssignVal::Scalar(w) }))
    }

    fn parse_simple(&mut self) -> PR<Cmd> {
        let line = self.line();
        let mut sc = SimpleCmd { assigns: Vec::new(), words: Vec::new(), redirs: Vec::new(), line };
        let mut decl = false;
        self.alias_guard.clear();
        loop {
            self.skip_ws();
            if let Some(r) = self.parse_redir()? {
                sc.redirs.push(r);
                continue;
            }
            match self.peek() {
                None | Some('\n') | Some(';') | Some('&') | Some('|') | Some(')') => break,
                Some('(') => return self.unexpected(),
                _ => {}
            }
            if sc.words.is_empty() || decl {
                if let Some(a) = self.try_parse_assign()? {
                    if sc.words.is_empty() {
                        sc.assigns.push(a);
                    } else {
                        sc.words.push(vec![WordPart::Assign(Box::new(a))]);
                    }
                    continue;
                }
            }
            if sc.words.is_empty() && !self.aliases.is_empty() {
                let tok = self.peek_token();
                if !self.alias_guard.contains(&tok) {
                    if let Some(val) = self.aliases.get(&tok) {
                        let val: Vec<char> = val.chars().collect();
                        let n = tok.chars().count();
                        self.src.splice(self.pos..self.pos + n, val);
                        self.alias_guard.push(tok);
                        // an alias may expand to a compound command or reserved word
                        self.skip_ws();
                        let t = self.peek_token();
                        if sc.assigns.is_empty() && sc.redirs.is_empty() && (RESERVED.contains(&t.as_str()) || self.peek() == Some('(')) && !CLOSERS.contains(&t.as_str()) {
                            return self.parse_pipeline();
                        }
                        continue;
                    }
                }
            }
            let w = self.parse_word()?;
            if w.is_empty() {
                return self.unexpected();
            }
            if sc.words.is_empty() {
                if let Some(l) = word_literal(&w) {
                    decl = DECL.contains(&l.as_str());
                }
            }
            sc.words.push(w);
        }
        if sc.words.is_empty() && sc.assigns.is_empty() && sc.redirs.is_empty() {
            return self.unexpected();
        }
        Ok(Cmd::Simple(sc))
    }

    // ---------- [[ ... ]] ----------

    fn cond_ws(&mut self) {
        loop {
            self.skip_ws();
            if self.peek() == Some('\n') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn cond_or(&mut self) -> PR<CondExpr> {
        let mut l = self.cond_and()?;
        loop {
            self.cond_ws();
            if self.eat("||") {
                let r = self.cond_and()?;
                l = CondExpr::Or(Box::new(l), Box::new(r));
            } else {
                return Ok(l);
            }
        }
    }

    fn cond_and(&mut self) -> PR<CondExpr> {
        let mut l = self.cond_not()?;
        loop {
            self.cond_ws();
            if self.eat("&&") {
                let r = self.cond_not()?;
                l = CondExpr::And(Box::new(l), Box::new(r));
            } else {
                return Ok(l);
            }
        }
    }

    fn cond_not(&mut self) -> PR<CondExpr> {
        self.cond_ws();
        if self.peek_token() == "!" {
            self.pos += 1;
            return Ok(CondExpr::Not(Box::new(self.cond_not()?)));
        }
        if self.peek() == Some('(') {
            self.pos += 1;
            let e = self.cond_or()?;
            self.cond_ws();
            if !self.eat(")") {
                return self.err("expected `)' in conditional expression");
            }
            return Ok(e);
        }
        self.cond_primary()
    }

    fn cond_primary(&mut self) -> PR<CondExpr> {
        self.cond_ws();
        if self.starts("]]") || self.at_end() {
            return self.err("unexpected end of conditional expression");
        }
        let w = self.parse_word()?;
        if w.is_empty() {
            return self.unexpected();
        }
        if let Some(s) = word_literal(&w) {
            let cs: Vec<char> = s.chars().collect();
            if cs.len() == 2 && cs[0] == '-' && "abcdefghknoprstuvwxzGLNOS".contains(cs[1]) {
                self.cond_ws();
                if !self.starts("]]") {
                    let a = self.parse_word()?;
                    if a.is_empty() {
                        return self.unexpected();
                    }
                    return Ok(CondExpr::Unary(s, a));
                }
            }
        }
        self.cond_ws();
        if self.starts("]]") || self.starts("&&") || self.starts("||") || self.peek() == Some(')') {
            return Ok(CondExpr::Word(w));
        }
        let op = if self.peek() == Some('<') || self.peek() == Some('>') {
            let c = self.peek().unwrap();
            self.pos += 1;
            c.to_string()
        } else {
            let t = self.peek_token();
            const OPS: &[&str] = &["==", "=", "!=", "=~", "-eq", "-ne", "-lt", "-le", "-gt", "-ge", "-nt", "-ot", "-ef"];
            if !OPS.contains(&t.as_str()) {
                return self.err(&format!("conditional binary operator expected, found `{}'", t));
            }
            self.pos += t.chars().count();
            t
        };
        self.cond_ws();
        if op == "=~" {
            self.regex_mode = true;
        }
        let r = self.parse_word();
        self.regex_mode = false;
        let r = r?;
        if r.is_empty() {
            return self.unexpected();
        }
        Ok(CondExpr::Binary(w, op, r))
    }
}

/// Check whether `src` is a syntactically complete command (for the line editor).
pub fn check_complete(src: &str, aliases: &Rc<HashMap<String, String>>) -> Result<(), ParseError> {
    let mut p = Parser::new(src);
    p.interactive = true;
    p.aliases = aliases.clone();
    loop {
        match p.parse_program_command() {
            Ok(None) => return Ok(()),
            Ok(Some(_)) => {}
            Err(e) => return Err(e),
        }
    }
}
