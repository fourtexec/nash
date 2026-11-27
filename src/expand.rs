//! Word expansion: braces, tilde, parameters, command substitution, arithmetic,
//! field splitting and pathname expansion.

use crate::arith::{self, Num};
use crate::ast::*;
use crate::glob;
use crate::parser::Parser;
use crate::shell::{Flow, Shell, Val};
use crate::sys;
use std::borrow::Cow;
use std::fmt::Write as _;

const MAX_BRACE_ITEMS: usize = 100_000;
const MAX_BRACE_DEPTH: u32 = 64;

type Field = Vec<(char, bool)>;

enum IfsMode {
    Default,
    Empty,
    Custom(Vec<char>),
}

/// Field builder: accumulates expansion results and performs IFS splitting.
pub struct FB {
    done: Vec<Field>,
    cur: Field,
    active: bool,
    ws_delim: bool,
    ifs: IfsMode,
    nosplit: bool,
}

impl FB {
    fn new(ifs: &str, nosplit: bool) -> FB {
        let mode = if nosplit || ifs.is_empty() {
            IfsMode::Empty
        } else if ifs == " \t\n" {
            IfsMode::Default
        } else {
            IfsMode::Custom(ifs.chars().collect())
        };
        FB { done: Vec::new(), cur: Vec::new(), active: false, ws_delim: false, ifs: mode, nosplit }
    }
    #[inline]
    fn is_ifs(&self, c: char) -> bool {
        match &self.ifs {
            IfsMode::Default => c == ' ' || c == '\t' || c == '\n',
            IfsMode::Empty => false,
            IfsMode::Custom(v) => v.contains(&c),
        }
    }
    fn push_str(&mut self, s: &str, quoted: bool) {
        self.cur.reserve(s.len());
        for c in s.chars() {
            self.cur.push((c, quoted));
        }
        if quoted || !s.is_empty() {
            self.active = true;
        }
        self.ws_delim = false;
    }
    fn brk(&mut self) {
        if self.active {
            self.done.push(std::mem::take(&mut self.cur));
            self.active = false;
        }
    }
    /// Separator between elements of `$@`-like lists.
    fn sep(&mut self) {
        if self.nosplit {
            self.cur.push((' ', true));
        } else {
            self.brk();
        }
    }
    fn split(&mut self, s: &str) {
        if self.nosplit || matches!(self.ifs, IfsMode::Empty) {
            self.push_str(s, false);
            return;
        }
        for c in s.chars() {
            if self.is_ifs(c) {
                if c == ' ' || c == '\t' || c == '\n' {
                    if self.active {
                        self.brk();
                        self.ws_delim = true;
                    }
                } else {
                    if self.active || !self.ws_delim {
                        self.done.push(std::mem::take(&mut self.cur));
                        self.active = false;
                    }
                    self.ws_delim = false;
                }
            } else {
                self.cur.push((c, false));
                self.active = true;
                self.ws_delim = false;
            }
        }
    }
    fn finish(mut self) -> Vec<Field> {
        self.brk();
        self.done
    }
    fn into_string(self) -> String {
        let mut s = String::new();
        for f in self.done.iter().chain(std::iter::once(&self.cur)) {
            s.extend(f.iter().map(|(c, _)| *c));
        }
        s
    }
    fn into_pattern(self) -> String {
        let mut s = String::new();
        for f in self.done.iter().chain(std::iter::once(&self.cur)) {
            s.push_str(&to_pattern(f));
        }
        s
    }
}

fn needs_glob(f: &Field) -> bool {
    let mut prev = ' ';
    for (i, (c, q)) in f.iter().enumerate() {
        if !*q {
            if matches!(c, '*' | '?')
                || (*c == '[' && f[i + 1..].iter().any(|(cc, qq)| !*qq && *cc == ']'))
                || (*c == '(' && matches!(prev, '@' | '+' | '!'))
            {
                return true;
            }
            prev = *c;
        } else {
            prev = ' ';
        }
    }
    false
}

#[inline]
fn str_needs_glob(s: &str) -> bool {
    let b = s.as_bytes();
    let mut prev = b' ';
    for (i, &c) in b.iter().enumerate() {
        if c == b'*' || c == b'?' {
            return true;
        }
        if c == b'[' && b[i + 1..].contains(&b']') {
            return true;
        }
        if c == b'(' && matches!(prev, b'@' | b'+' | b'!') {
            return true;
        }
        prev = c;
    }
    false
}

fn to_pattern(f: &Field) -> String {
    let mut s = String::with_capacity(f.len());
    for (c, q) in f {
        if *q && matches!(c, '*' | '?' | '[' | ']' | '\\' | '(' | ')' | '|' | '!' | '+' | '@' | '^' | '-') {
            s.push('\\');
        }
        s.push(*c);
    }
    s
}

// ---------- brace expansion ----------

#[derive(Clone)]
enum It {
    C(char),
    P(WordPart),
}

fn sequence(body: &[It]) -> Option<Vec<Vec<It>>> {
    let mut s = String::new();
    for it in body {
        match it {
            It::C(c) => s.push(*c),
            It::P(_) => return None,
        }
    }
    let parts: Vec<&str> = s.split("..").collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let step: i128 = if parts.len() == 3 {
        let raw = parts[2].parse::<i128>().ok()?;
        (raw.unsigned_abs().max(1)).min(i128::MAX as u128) as i128
    } else {
        1
    };
    let mk = |v: Vec<String>| Some(v.into_iter().map(|x| x.chars().map(It::C).collect()).collect());
    if let (Ok(a64), Ok(b64)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
        let a = a64 as i128;
        let b = b64 as i128;
        let padded = |p: &str| {
            let d = p.trim_start_matches('-');
            d.len() > 1 && d.starts_with('0')
        };
        let width = if padded(parts[0]) || padded(parts[1]) { parts[0].len().max(parts[1].len()) } else { 0 };
        let count = (a - b).unsigned_abs() / (step as u128);
        if count >= MAX_BRACE_ITEMS as u128 {
            return None;
        }
        let mut v = Vec::with_capacity((count as usize) + 1);
        let mut x = a;
        loop {
            v.push(if width > 0 {
                if x < 0 {
                    format!("-{:0w$}", x.unsigned_abs(), w = width.saturating_sub(1))
                } else {
                    format!("{:0w$}", x, w = width)
                }
            } else {
                x.to_string()
            });
            if a <= b {
                x += step;
                if x > b {
                    break;
                }
            } else {
                x -= step;
                if x < b {
                    break;
                }
            }
        }
        return mk(v);
    }
    let (ca, cb): (Vec<char>, Vec<char>) = (parts[0].chars().collect(), parts[1].chars().collect());
    if ca.len() == 1 && cb.len() == 1 && ca[0].is_ascii_alphabetic() && cb[0].is_ascii_alphabetic() {
        let (a, b) = (ca[0] as i128, cb[0] as i128);
        let mut v = Vec::new();
        let mut x = a;
        loop {
            v.push(((x as u8) as char).to_string());
            if a <= b {
                x += step;
                if x > b {
                    break;
                }
            } else {
                x -= step;
                if x < b {
                    break;
                }
            }
        }
        return mk(v);
    }
    None
}

fn expand_items_depth(items: &[It], depth: u32) -> Option<Vec<Vec<It>>> {
    if depth >= MAX_BRACE_DEPTH {
        return None;
    }
    let mut i = 0;
    while i < items.len() {
        if matches!(items[i], It::C('{')) {
            let mut bdepth = 0;
            let mut j = i + 1;
            let mut commas = Vec::new();
            let mut close = None;
            while j < items.len() {
                match items[j] {
                    It::C('{') => bdepth += 1,
                    It::C('}') => {
                        if bdepth == 0 {
                            close = Some(j);
                            break;
                        }
                        bdepth -= 1;
                    }
                    It::C(',') if bdepth == 0 => commas.push(j),
                    _ => {}
                }
                j += 1;
            }
            if let Some(close) = close {
                let alts: Option<Vec<Vec<It>>> = if !commas.is_empty() {
                    let mut out_alts = Vec::new();
                    let mut st = i + 1;
                    for &c in &commas {
                        let sub = expand_items_depth(&items[st..c], depth + 1)?;
                        if out_alts.len().saturating_add(sub.len()) > MAX_BRACE_ITEMS {
                            return None;
                        }
                        out_alts.extend(sub);
                        st = c + 1;
                    }
                    let sub = expand_items_depth(&items[st..close], depth + 1)?;
                    if out_alts.len().saturating_add(sub.len()) > MAX_BRACE_ITEMS {
                        return None;
                    }
                    out_alts.extend(sub);
                    Some(out_alts)
                } else {
                    sequence(&items[i + 1..close])
                };
                if let Some(alts) = alts {
                    let suffixes = expand_items_depth(&items[close + 1..], depth + 1)?;
                    let total = alts.len().saturating_mul(suffixes.len());
                    if total > MAX_BRACE_ITEMS {
                        return None;
                    }
                    let mut out = Vec::with_capacity(total);
                    for a in &alts {
                        for s in &suffixes {
                            let mut v = Vec::with_capacity(i + a.len() + s.len());
                            v.extend_from_slice(&items[..i]);
                            v.extend(a.iter().cloned());
                            v.extend(s.iter().cloned());
                            out.push(v);
                        }
                    }
                    return Some(out);
                }
            }
        }
        i += 1;
    }
    Some(vec![items.to_vec()])
}

#[inline]
fn has_brace(w: &Word) -> bool {
    w.iter().any(|p| matches!(p, WordPart::Lit(s) if s.contains('{')))
}

pub fn brace_expand(w: &Word) -> Vec<Word> {
    if !has_brace(w) {
        return vec![w.clone()];
    }
    let mut items = Vec::new();
    for p in w {
        match p {
            WordPart::Lit(s) => items.extend(s.chars().map(It::C)),
            o => items.push(It::P(o.clone())),
        }
    }
    let Some(expanded) = expand_items_depth(&items, 0) else {
        return vec![w.clone()];
    };
    expanded
        .into_iter()
        .map(|its| {
            let mut w: Word = Vec::new();
            let mut lit = String::new();
            for it in its {
                match it {
                    It::C(c) => lit.push(c),
                    It::P(p) => {
                        if !lit.is_empty() {
                            w.push(WordPart::Lit(std::mem::take(&mut lit)));
                        }
                        w.push(p);
                    }
                }
            }
            if !lit.is_empty() {
                w.push(WordPart::Lit(lit));
            }
            w
        })
        .collect()
}

// ---------- parameters ----------

pub enum PV {
    Unset,
    Str(String),
    List(Vec<String>, bool),
}

fn map_pv(v: PV, f: impl Fn(&str) -> String) -> PV {
    match v {
        PV::Unset => PV::Unset,
        PV::Str(s) => PV::Str(f(&s)),
        PV::List(l, at) => PV::List(l.iter().map(|s| f(s)).collect(), at),
    }
}

pub fn shell_quote(s: &str) -> String {
    // Always quote for alias output compatibility with bash
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn is_at_expansion(p: &WordPart) -> bool {
    match p {
        WordPart::Param(pe) => (pe.name == "@" && pe.index.is_none()) || pe.index.as_deref() == Some("@"),
        _ => false,
    }
}

#[inline]
fn is_special_name(name: &str) -> bool {
    matches!(
        name.as_bytes().first(),
        Some(b'?' | b'#' | b'$' | b'!' | b'-' | b'@' | b'*' | b'0'..=b'9')
    )
}

impl arith::Env for Shell {
    fn get(&mut self, name: &str, idx: Option<&str>) -> Option<String> {
        match idx {
            None => Shell::get(self, name),
            Some(i) => self.get_elem(name, i).ok().flatten(),
        }
    }
    fn get_fast(&mut self, name: &str, idx: Option<&str>) -> Option<Cow<'_, str>> {
        match idx {
            None => self.get_cow(name),
            Some(i) => self.get_elem(name, i).ok().flatten().map(Cow::Owned),
        }
    }
    fn set(&mut self, name: &str, idx: Option<&str>, val: String) -> Result<(), String> {
        match idx {
            None => self.set_str(name, val),
            Some(i) => self.set_elem(name, i, val, false).map_err(|_| format!("{}: bad array subscript", name)),
        }
    }
    fn set_num(&mut self, name: &str, idx: Option<&str>, val: Num) -> Result<(), String> {
        match (idx, val) {
            (None, Num::I(n)) => self.set_i64(name, n),
            _ => <Self as arith::Env>::set(self, name, idx, val.fmt()),
        }
    }
    fn strict_arith(&self) -> bool {
        self.opts.strict_arith
    }
    fn nounset(&self) -> bool {
        self.opts.nounset
    }
}

impl Shell {
    pub fn fail<T>(&mut self, msg: String) -> Result<T, Flow> {
        sherr!("{}", msg);
        Err(Flow::Abort(1))
    }

    pub fn arith(&mut self, expr: &str) -> Result<Num, Flow> {
        let res = if !expr.contains(['$', '`', '\\']) {
            arith::eval(expr, self)
        } else {
            let s = self.expand_string(expr)?;
            arith::eval(&s, self)
        };
        match res {
            Ok(n) => Ok(n),
            Err(e) => {
                sherr!("{}", e);
                Err(Flow::Abort(1))
            }
        }
    }

    /// Expand a string as if it were double-quoted (parameters, command
    /// substitution, arithmetic) at run time.
    pub fn expand_string(&mut self, s: &str) -> Result<String, Flow> {
        if !s.contains(['$', '`', '\\']) {
            return Ok(s.to_string());
        }
        let mut p = Parser::new(s);
        p.aliases = self.aliases.clone();
        let parts = match p.parse_dquote_body(None) {
            Ok(p) => p,
            Err(e) => return self.fail(e.msg),
        };
        let mut out = String::with_capacity(s.len());
        self.expand_parts_str(&parts, &mut out)?;
        Ok(out)
    }

    pub fn expand_words(&mut self, words: &[Word]) -> Result<Vec<String>, Flow> {
        let mut out = Vec::with_capacity(words.len());
        for w in words {
            self.expand_word_into(w, &mut out)?;
        }
        Ok(out)
    }

    pub fn expand_word_into(&mut self, w: &Word, out: &mut Vec<String>) -> Result<(), Flow> {
        match w.as_slice() {
            [] => return Ok(()),
            [WordPart::Lit(s)] => {
                if !s.contains('{') {
                    if self.opts.noglob || !str_needs_glob(s) {
                        out.push(s.clone());
                        return Ok(());
                    }
                    let ms = glob::glob(s, self.opts.dotglob);
                    if !ms.is_empty() {
                        out.extend(ms);
                    } else if !self.opts.nullglob {
                        out.push(s.clone());
                    }
                    return Ok(());
                }
            }
            [WordPart::Quoted(s)] => {
                out.push(s.clone());
                return Ok(());
            }
            [WordPart::DQuote(inner)] if !inner.iter().any(is_at_expansion) => {
                let mut s = String::new();
                self.expand_parts_str(inner, &mut s)?;
                out.push(s);
                return Ok(());
            }
            [WordPart::Param(pe)]
                if !pe.indirect
                    && pe.index.is_none()
                    && matches!(pe.op, ParamOp::Plain)
                    && !is_special_name(&pe.name) =>
            {
                let default_ifs = self.ifs_str() == " \t\n";
                if default_ifs {
                    let val = match self.get_cow(&pe.name) {
                        Some(v) => v.into_owned(),
                        None => {
                            if self.opts.nounset {
                                return self.fail(format!("{}: unbound variable", pe.name));
                            }
                            return Ok(());
                        }
                    };
                    let noglob = self.opts.noglob;
                    let dotglob = self.opts.dotglob;
                    let nullglob = self.opts.nullglob;
                    for tok in val.split(|c: char| c == ' ' || c == '\t' || c == '\n') {
                        if tok.is_empty() {
                            continue;
                        }
                        if !noglob && str_needs_glob(tok) {
                            let ms = glob::glob(tok, dotglob);
                            if !ms.is_empty() {
                                out.extend(ms);
                                continue;
                            }
                            if nullglob {
                                continue;
                            }
                        }
                        out.push(tok.to_string());
                    }
                    return Ok(());
                }
            }
            _ => {}
        }

        let expanded;
        let words_slice: &[Word] = if has_brace(w) {
            expanded = brace_expand(w);
            &expanded
        } else {
            std::slice::from_ref(w)
        };
        for w in words_slice {
            let mut fb = FB::new(self.ifs_str(), false);
            self.expand_parts(w, false, false, &mut fb)?;
            for f in fb.finish() {
                if !self.opts.noglob && needs_glob(&f) {
                    let ms = glob::glob(&to_pattern(&f), self.opts.dotglob);
                    if !ms.is_empty() {
                        out.extend(ms);
                        continue;
                    }
                    if self.opts.nullglob {
                        continue;
                    }
                }
                let mut s = String::with_capacity(f.len());
                for (c, _) in f {
                    s.push(c);
                }
                out.push(s);
            }
        }
        Ok(())
    }

    /// Expand without field splitting or globbing (assignments, redirections, case).
    pub fn expand_word_str(&mut self, w: &Word) -> Result<String, Flow> {
        match w.as_slice() {
            [] => Ok(String::new()),
            [WordPart::Lit(s)] | [WordPart::Quoted(s)] => Ok(s.clone()),
            _ => {
                let mut out = String::new();
                self.expand_parts_str(w, &mut out)?;
                Ok(out)
            }
        }
    }

    /// Fast direct-to-String expansion without building intermediate `Field` buffers.
    fn expand_parts_str(&mut self, parts: &[WordPart], out: &mut String) -> Result<(), Flow> {
        for p in parts {
            match p {
                WordPart::Lit(s) | WordPart::Quoted(s) => out.push_str(s),
                WordPart::DQuote(inner) => self.expand_parts_str(inner, out)?,
                WordPart::Tilde(u) => {
                    let t = self.expand_tilde(u);
                    out.push_str(&t);
                }
                WordPart::Arith(raw) => match self.arith(raw)? {
                    Num::I(i) => {
                        let _ = write!(out, "{}", i);
                    }
                    n => out.push_str(&n.fmt()),
                },
                WordPart::CmdSubst(cmd) => {
                    let v = self.cmd_subst(cmd)?;
                    out.push_str(&v);
                }
                WordPart::ProcSubst(input, cmd) => {
                    let path = self.proc_subst(*input, cmd)?;
                    out.push_str(&path);
                }
                WordPart::Param(pe) => {
                    if !pe.indirect && pe.index.is_none() && matches!(pe.op, ParamOp::Plain) && !is_special_name(&pe.name) {
                        let nounset = self.opts.nounset;
                        match self.get_cow(&pe.name) {
                            Some(s) => out.push_str(&s),
                            None if nounset => return self.fail(format!("{}: unbound variable", pe.name)),
                            None => {}
                        }
                    } else {
                        let mut fb = FB::new("", true);
                        self.expand_param(pe, true, &mut fb)?;
                        out.push_str(&fb.into_string());
                    }
                }
                WordPart::Assign(_) => {}
            }
        }
        Ok(())
    }

    /// Expand to a glob pattern in which quoted characters are escaped.
    pub fn expand_pattern(&mut self, w: &Word) -> Result<String, Flow> {
        if let [WordPart::Lit(s)] = w.as_slice() {
            return Ok(s.clone());
        }
        let mut fb = FB::new("", true);
        self.expand_parts(w, false, false, &mut fb)?;
        Ok(fb.into_pattern())
    }

    fn expand_tilde(&mut self, user: &str) -> String {
        match user {
            "" => self.home(),
            "+" => self.cwd(),
            "-" => self.get("OLDPWD").unwrap_or_else(|| "~-".into()),
            u => sys::home_of(u).unwrap_or_else(|| format!("~{}", u)),
        }
    }

    fn expand_parts(&mut self, parts: &[WordPart], quoted: bool, split_lit: bool, fb: &mut FB) -> Result<(), Flow> {
        for p in parts {
            match p {
                WordPart::Lit(s) => {
                    if split_lit {
                        fb.split(s)
                    } else {
                        fb.push_str(s, quoted)
                    }
                }
                WordPart::Quoted(s) => fb.push_str(s, true),
                WordPart::DQuote(inner) => {
                    if inner.is_empty() || !inner.iter().all(is_at_expansion) {
                        fb.push_str("", true);
                    }
                    self.expand_parts(inner, true, false, fb)?;
                }
                WordPart::Tilde(u) => {
                    let t = self.expand_tilde(u);
                    fb.push_str(&t, true);
                }
                WordPart::Arith(raw) => {
                    let v = self.arith(raw)?.fmt();
                    if quoted {
                        fb.push_str(&v, true)
                    } else {
                        fb.split(&v)
                    }
                }
                WordPart::CmdSubst(cmd) => {
                    let v = self.cmd_subst(cmd)?;
                    if quoted {
                        fb.push_str(&v, true)
                    } else {
                        fb.split(&v)
                    }
                }
                WordPart::ProcSubst(input, cmd) => {
                    let path = self.proc_subst(*input, cmd)?;
                    fb.push_str(&path, true);
                }
                WordPart::Param(pe) => self.expand_param(pe, quoted, fb)?,
                WordPart::Assign(_) => {}
            }
        }
        Ok(())
    }

    fn special_param(&mut self, name: &str) -> Option<PV> {
        let c = name.chars().next()?;
        Some(match c {
            '?' => PV::Str(self.last_status.to_string()),
            '#' => PV::Str(self.pos_params.len().to_string()),
            '$' => PV::Str(self.shell_pid.to_string()),
            '!' => {
                if self.last_bg == 0 {
                    PV::Unset
                } else {
                    PV::Str(self.last_bg.to_string())
                }
            }
            '-' => {
                let mut f = self.opts.flags();
                if self.interactive {
                    f.push('i');
                }
                PV::Str(f)
            }
            '@' => PV::List(self.pos_params.clone(), true),
            '*' => PV::List(self.pos_params.clone(), false),
            '0'..='9' => {
                let n: usize = name.parse().unwrap_or(usize::MAX);
                if n == 0 {
                    PV::Str(self.arg0.clone())
                } else {
                    match self.pos_params.get(n - 1) {
                        Some(s) => PV::Str(s.clone()),
                        None => PV::Unset,
                    }
                }
            }
            _ => return None,
        })
    }

    pub fn get_elem(&mut self, name: &str, idx: &str) -> Result<Option<String>, Flow> {
        let val = match self.var(name) {
            Some(v) => v.val.clone(),
            None => return Ok(None),
        };
        Ok(match val {
            Val::Map(m) => {
                let key = self.expand_string(idx)?;
                m.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
            }
            Val::Arr(a) => {
                let mut i = self.arith(idx)?.as_i();
                if i < 0 {
                    i += a.len() as i64;
                }
                if i < 0 {
                    None
                } else {
                    a.get(i as usize).cloned()
                }
            }
            Val::Str(s) => {
                if self.arith(idx)?.as_i() == 0 {
                    Some(s)
                } else {
                    None
                }
            }
            Val::Unset => None,
        })
    }

    pub fn set_elem(&mut self, name: &str, idx: &str, val: String, append: bool) -> Result<(), Flow> {
        // Map case needs idx expansion; handle first.
        let is_map = matches!(self.var(name).map(|v| &v.val), Some(Val::Map(_)));
        if is_map {
            let key = self.expand_string(idx)?;
            if let Some(v) = self.var_mut(name) {
                if v.readonly {
                    return self.fail(format!("{}: readonly variable", name));
                }
                if let Val::Map(m) = &mut v.val {
                    match m.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, vv)) => {
                            if append {
                                vv.push_str(&val)
                            } else {
                                *vv = val
                            }
                        }
                        None => m.push((key, val)),
                    }
                    return Ok(());
                }
            }
            return self.fail(format!("{}: bad array subscript", name));
        }

        // Arithmetic index (may expand idx).
        let i = self.arith(idx)?.as_i();

        // Try to mutate an existing variable in place (no full-array clone).
        if let Some(v) = self.var_mut(name) {
            if v.readonly {
                return self.fail(format!("{}: readonly variable", name));
            }
            match &mut v.val {
                Val::Arr(a) => {
                    let mut ii = i;
                    if ii < 0 {
                        ii += a.len() as i64;
                    }
                    if !(0..=10_000_000).contains(&ii) {
                        return self.fail(format!("{}[{}]: bad array subscript", name, idx));
                    }
                    let ii = ii as usize;
                    while a.len() <= ii {
                        a.push(String::new());
                    }
                    if append {
                        a[ii].push_str(&val);
                    } else {
                        a[ii] = val;
                    }
                    return Ok(());
                }
                Val::Str(s) => {
                    let mut a = vec![s.clone()];
                    let mut ii = i;
                    if ii < 0 {
                        ii += a.len() as i64;
                    }
                    if !(0..=10_000_000).contains(&ii) {
                        return self.fail(format!("{}[{}]: bad array subscript", name, idx));
                    }
                    let ii = ii as usize;
                    while a.len() <= ii {
                        a.push(String::new());
                    }
                    if append {
                        a[ii].push_str(&val);
                    } else {
                        a[ii] = val;
                    }
                    v.val = Val::Arr(a);
                    return Ok(());
                }
                Val::Unset => {
                    let mut a = Vec::new();
                    let mut ii = i;
                    if ii < 0 {
                        ii += a.len() as i64;
                    }
                    if !(0..=10_000_000).contains(&ii) {
                        return self.fail(format!("{}[{}]: bad array subscript", name, idx));
                    }
                    let ii = ii as usize;
                    while a.len() <= ii {
                        a.push(String::new());
                    }
                    if append {
                        a[ii].push_str(&val);
                    } else {
                        a[ii] = val;
                    }
                    v.val = Val::Arr(a);
                    return Ok(());
                }
                _ => {}
            }
        }

        // Variable does not exist: create in innermost scope.
        let mut a = Vec::new();
        let mut ii = i;
        if ii < 0 {
            ii += a.len() as i64;
        }
        if !(0..=10_000_000).contains(&ii) {
            return self.fail(format!("{}[{}]: bad array subscript", name, idx));
        }
        let ii = ii as usize;
        while a.len() <= ii {
            a.push(String::new());
        }
        if append {
            a[ii].push_str(&val);
        } else {
            a[ii] = val;
        }
        match self.set(name, Val::Arr(a)) {
            Ok(()) => Ok(()),
            Err(e) => self.fail(e),
        }
    }

    pub fn param_value(&mut self, name: &str, index: Option<&str>) -> Result<PV, Flow> {
        if let Some(pv) = self.special_param(name) {
            return Ok(pv);
        }
        match index {
            Some(i) if i == "@" || i == "*" => {
                let at = i == "@";
                Ok(match self.var(name).map(|v| v.val.clone()) {
                    Some(Val::Str(s)) => PV::List(vec![s], at),
                    Some(Val::Arr(a)) => PV::List(a, at),
                    Some(Val::Map(m)) => PV::List(m.into_iter().map(|(_, v)| v).collect(), at),
                    _ => match self.get(name) {
                        Some(s) => PV::List(vec![s], at),
                        None => PV::List(Vec::new(), at),
                    },
                })
            }
            Some(i) => Ok(match self.get_elem(name, i)? {
                Some(s) => PV::Str(s),
                None => PV::Unset,
            }),
            None => Ok(match self.get(name) {
                Some(s) => PV::Str(s),
                None => PV::Unset,
            }),
        }
    }

    fn emit_pv(&mut self, val: PV, quoted: bool, fb: &mut FB) {
        match val {
            PV::Unset => {}
            PV::Str(s) => {
                if quoted {
                    fb.push_str(&s, true)
                } else {
                    fb.split(&s)
                }
            }
            PV::List(v, at) => {
                if quoted && !at {
                    let sep = self.ifs_str().chars().next().map(|c| c.to_string()).unwrap_or_default();
                    fb.push_str(&v.join(&sep), true);
                } else {
                    for (i, e) in v.iter().enumerate() {
                        if i > 0 {
                            fb.sep();
                        }
                        if quoted {
                            fb.push_str(e, true)
                        } else {
                            fb.split(e)
                        }
                    }
                }
            }
        }
    }

    fn expand_param(&mut self, p: &ParamExp, quoted: bool, fb: &mut FB) -> Result<(), Flow> {
        let (name, index): (Cow<'_, str>, Option<Cow<'_, str>>) = if p.indirect {
            let target = match self.param_value(&p.name, p.index.as_deref())? {
                PV::Str(s) => s,
                _ => String::new(),
            };
            if target.is_empty() {
                return Ok(());
            }
            match target.find('[') {
                Some(i) if target.ends_with(']') => (
                    Cow::Owned(target[..i].to_string()),
                    Some(Cow::Owned(target[i + 1..target.len() - 1].to_string())),
                ),
                _ => (Cow::Owned(target), None),
            }
        } else {
            (Cow::Borrowed(&p.name), p.index.as_deref().map(Cow::Borrowed))
        };
        if let ParamOp::Keys = p.op {
            let keys = match self.var(&name).map(|v| &v.val) {
                Some(Val::Arr(a)) => (0..a.len()).map(|i| i.to_string()).collect(),
                Some(Val::Map(m)) => m.iter().map(|(k, _)| k.clone()).collect(),
                Some(Val::Str(_)) => vec!["0".to_string()],
                _ => Vec::new(),
            };
            self.emit_pv(PV::List(keys, index.as_deref() == Some("@")), quoted, fb);
            return Ok(());
        }
        let val = self.param_value(&name, index.as_deref())?;
        let is_unset = match &val {
            PV::Unset => true,
            PV::List(v, _) => v.is_empty(),
            _ => false,
        };
        let is_null = match &val {
            PV::Unset => true,
            PV::Str(s) => s.is_empty(),
            PV::List(v, _) => v.is_empty() || (v.len() == 1 && v[0].is_empty()),
        };
        match &p.op {
            ParamOp::Default(colon, w) => {
                if is_unset || (*colon && is_null) {
                    return self.expand_parts(w, quoted, !quoted, fb);
                }
            }
            ParamOp::Assign(colon, w) => {
                if is_unset || (*colon && is_null) {
                    let s = self.expand_word_str(w)?;
                    match index.as_deref() {
                        Some(i) => self.set_elem(&name, i, s.clone(), false)?,
                        None => {
                            if let Err(e) = self.set_str(&name, s.clone()) {
                                return self.fail(e);
                            }
                        }
                    }
                    self.emit_pv(PV::Str(s), quoted, fb);
                    return Ok(());
                }
            }
            ParamOp::Alt(colon, w) => {
                if is_unset || (*colon && is_null) {
                    return Ok(());
                }
                return self.expand_parts(w, quoted, !quoted, fb);
            }
            ParamOp::Error(colon, w) => {
                if is_unset || (*colon && is_null) {
                    let mut msg = self.expand_word_str(w)?;
                    if msg.is_empty() {
                        msg = "parameter null or not set".into();
                    }
                    sherr!("{}: {}", name, msg);
                    return Err(if self.interactive { Flow::Abort(1) } else { Flow::Exit(1) });
                }
            }
            _ => {}
        }
        if matches!(val, PV::Unset) && self.opts.nounset && !matches!(p.op, ParamOp::Length if name == "#") {
            return self.fail(format!("{}: unbound variable", name));
        }
        let is_list = name == "@" || name == "*" || matches!(index.as_deref(), Some("@") | Some("*"));
        let val = match &p.op {
            ParamOp::Length => PV::Str(match &val {
                PV::Unset => "0".to_string(),
                PV::Str(s) => s.chars().count().to_string(),
                PV::List(v, _) => {
                    if is_list {
                        v.len().to_string()
                    } else {
                        v.first().map(|s| s.chars().count()).unwrap_or(0).to_string()
                    }
                }
            }),
            ParamOp::TrimPrefix(long, w) => {
                let pat = self.expand_pattern(w)?;
                map_pv(val, |s| glob::trim(s, &pat, true, *long))
            }
            ParamOp::TrimSuffix(long, w) => {
                let pat = self.expand_pattern(w)?;
                map_pv(val, |s| glob::trim(s, &pat, false, *long))
            }
            ParamOp::Replace { mode, pat, rep } => {
                let pat = self.expand_pattern(pat)?;
                let rep = self.expand_word_str(rep)?;
                map_pv(val, |s| glob::replace(s, &pat, &rep, *mode))
            }
            ParamOp::Substr(off, len) => {
                let o = self.arith(off)?.as_i();
                let l = match len {
                    Some(l) => Some(self.arith(l)?.as_i()),
                    None => None,
                };
                let range = |n: usize, mut o: i64| -> (usize, usize) {
                    let n = n as i64;
                    if o < 0 {
                        o = o.saturating_add(n);
                        if o < 0 {
                            return (0, 0);
                        }
                    }
                    let o = o.min(n);
                    let end = match l {
                        None => n,
                        Some(l) if l < 0 => n.saturating_add(l).max(o),
                        Some(l) => o.saturating_add(l).min(n),
                    };
                    (o as usize, end as usize)
                };
                match val {
                    PV::Unset => PV::Unset,
                    PV::Str(s) => {
                        let cs: Vec<char> = s.chars().collect();
                        let (a, b) = range(cs.len(), o);
                        PV::Str(cs[a..b].iter().collect())
                    }
                    PV::List(mut v, at) => {
                        if name == "@" || name == "*" {
                            if o > 0 {
                                let (a, b) = range(v.len(), o - 1);
                                PV::List(v[a..b].to_vec(), at)
                            } else if o == 0 {
                                v.insert(0, self.arg0.clone());
                                let (a, b) = range(v.len(), 0);
                                PV::List(v[a..b].to_vec(), at)
                            } else {
                                let (a, b) = range(v.len(), o);
                                PV::List(v[a..b].to_vec(), at)
                            }
                        } else {
                            let (a, b) = range(v.len(), o);
                            PV::List(v[a..b].to_vec(), at)
                        }
                    }
                }
            }
            ParamOp::Case { upper, all } => map_pv(val, |s| {
                if *all {
                    if *upper {
                        s.to_uppercase()
                    } else {
                        s.to_lowercase()
                    }
                } else {
                    let mut cs = s.chars();
                    match cs.next() {
                        Some(c) => {
                            let first: String = if *upper { c.to_uppercase().collect() } else { c.to_lowercase().collect() };
                            format!("{}{}", first, cs.as_str())
                        }
                        None => String::new(),
                    }
                }
            }),
            ParamOp::Transform(t) => match t {
                'Q' => map_pv(val, shell_quote),
                'U' => map_pv(val, |s| s.to_uppercase()),
                'L' => map_pv(val, |s| s.to_lowercase()),
                'u' => map_pv(val, |s| {
                    let mut cs = s.chars();
                    match cs.next() {
                        Some(c) => format!("{}{}", c.to_uppercase().collect::<String>(), cs.as_str()),
                        None => String::new(),
                    }
                }),
                'E' => map_pv(val, |s| crate::builtins::unescape(s).0),
                'P' => match val {
                    PV::Str(s) => PV::Str(crate::prompt::expand_prompt(self, &s)),
                    o => o,
                },
                _ => val,
            },
            _ => val,
        };
        self.emit_pv(val, quoted, fb);
        Ok(())
    }

    /// Perform a variable assignment from the AST.
    pub fn do_assign(&mut self, a: &Assign, local: bool, force_map: bool) -> Result<(), Flow> {
        match &a.value {
            AssignVal::Scalar(w) => {
                let s = self.expand_word_str(w)?;
                if let Some(idx) = &a.index {
                    if local && !self.scopes.last().unwrap().contains_key(&a.name) {
                        let _ = self.set_local(&a.name, if force_map { Val::Map(Vec::new()) } else { Val::Arr(Vec::new()) });
                    }
                    return self.set_elem(&a.name, idx, s, a.append);
                }
                let cur = if a.append { self.var(&a.name).map(|v| v.val.clone()) } else { None };
                let new = match cur {
                    Some(Val::Str(old)) => {
                        // integer-looking append stays a string, as in sh
                        Val::Str(old + &s)
                    }
                    Some(Val::Arr(mut arr)) => {
                        if arr.is_empty() {
                            arr.push(s)
                        } else {
                            arr[0].push_str(&s)
                        }
                        Val::Arr(arr)
                    }
                    Some(Val::Map(m)) => Val::Map(m),
                    _ => Val::Str(s),
                };
                self.store(&a.name, new, local)
            }
            AssignVal::Array(elems) => {
                // Fast path for simple append (a+=(...)): mutate in place instead
                // of cloning the whole array on every assignment.
                if a.append && elems.iter().all(|(k, _)| k.is_none()) {
                    if local && !self.scopes.last().unwrap().contains_key(&a.name) {
                        let _ = self.set_local(&a.name, Val::Arr(Vec::new()));
                    } else if self.var(&a.name).is_none() {
                        let _ = self.set(&a.name, Val::Arr(Vec::new()));
                    }
                    if let Some(v) = self.var_mut(&a.name) {
                        if let Val::Str(s) = &v.val {
                            let s = s.clone();
                            v.val = Val::Arr(vec![s]);
                        }
                        if !matches!(v.val, Val::Arr(_)) {
                            v.val = Val::Arr(Vec::new());
                        }
                    }
                    let mut new_items = Vec::new();
                    for (_, w) in elems {
                        self.expand_word_into(w, &mut new_items)?;
                    }
                    if let Some(v) = self.var_mut(&a.name) {
                        if let Val::Arr(arr) = &mut v.val {
                            arr.extend(new_items);
                            return Ok(());
                        }
                    }
                }
                let cur = self.var(&a.name).map(|v| v.val.clone());
                let is_map = force_map || (matches!(cur, Some(Val::Map(_))) && !local) || (matches!(cur, Some(Val::Map(_))) && self.scopes.last().unwrap().contains_key(&a.name));
                if is_map {
                    let mut m = match (a.append, cur) {
                        (true, Some(Val::Map(m))) => m,
                        _ => Vec::new(),
                    };
                    let mut pending_key: Option<String> = None;
                    for (key, w) in elems {
                        let (k, v) = match key {
                            Some(k) => (self.expand_string(k)?, self.expand_word_str(w)?),
                            None => {
                                // zsh style: (key value key value)
                                let s = self.expand_word_str(w)?;
                                match pending_key.take() {
                                    None => {
                                        pending_key = Some(s);
                                        continue;
                                    }
                                    Some(k) => (k, s),
                                }
                            }
                        };
                        match m.iter_mut().find(|(mk, _)| *mk == k) {
                            Some((_, mv)) => *mv = v,
                            None => m.push((k, v)),
                        }
                    }
                    return self.store(&a.name, Val::Map(m), local);
                }
                let mut arr = match (a.append, cur) {
                    (true, Some(Val::Arr(v))) => v,
                    (true, Some(Val::Str(s))) => vec![s],
                    _ => Vec::new(),
                };
                for (key, w) in elems {
                    match key {
                        Some(k) => {
                            let i = self.arith(k)?.as_i();
                            if !(0..=10_000_000).contains(&i) {
                                return self.fail(format!("{}[{}]: bad array subscript", a.name, k));
                            }
                            let v = self.expand_word_str(w)?;
                            while arr.len() <= i as usize {
                                arr.push(String::new());
                            }
                            arr[i as usize] = v;
                        }
                        None => self.expand_word_into(w, &mut arr)?,
                    }
                }
                self.store(&a.name, Val::Arr(arr), local)
            }
        }
    }

    fn store(&mut self, name: &str, val: Val, local: bool) -> Result<(), Flow> {
        let r = if local { self.set_local(name, val) } else { self.set(name, val) };
        match r {
            Ok(()) => Ok(()),
            Err(e) => self.fail(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brace_overflow_and_explosion_safety() {
        // i64::MIN range must not panic on negation or subtraction.
        let w = vec![WordPart::Lit("{-9223372036854775808..0}".into())];
        let out = brace_expand(&w);
        assert_eq!(out.len(), 1);

        // Zero-padded i64::MIN small range must not panic.
        let w2 = vec![WordPart::Lit("{-9223372036854775808..-9223372036854775807}".into())];
        let out2 = brace_expand(&w2);
        assert_eq!(out2.len(), 2);

        // Cartesian product bomb (2^25 = 33M) must bail out safely without OOM.
        let bomb = vec![WordPart::Lit("{a,b}".repeat(25))];
        let out_bomb = brace_expand(&bomb);
        assert_eq!(out_bomb.len(), 1);
    }
}
