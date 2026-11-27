//! Arithmetic evaluation: C-like integer arithmetic (as in POSIX/bash) extended
//! with floating point numbers and a small math library (as in zsh).

use std::borrow::Cow;

const MAX_EXPR_DEPTH: u32 = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    I(i64),
    F(f64),
}

impl Num {
    pub fn as_f(self) -> f64 {
        match self {
            Num::I(i) => i as f64,
            Num::F(f) => f,
        }
    }
    pub fn as_i(self) -> i64 {
        match self {
            Num::I(i) => i,
            Num::F(f) => f as i64,
        }
    }
    pub fn truthy(self) -> bool {
        match self {
            Num::I(i) => i != 0,
            Num::F(f) => f != 0.0,
        }
    }
    pub fn fmt(self) -> String {
        match self {
            Num::I(i) => i.to_string(),
            Num::F(f) => {
                if !f.is_finite() {
                    return if f.is_nan() { "nan".into() } else if f > 0.0 { "inf".into() } else { "-inf".into() };
                }
                if f.abs() >= 1e15 || (f != 0.0 && f.abs() < 1e-6) {
                    return format!("{:e}", f);
                }
                let s = format!("{:.10}", f);
                let s = s.trim_end_matches('0');
                if s.ends_with('.') {
                    format!("{}0", s)
                } else {
                    s.to_string()
                }
            }
        }
    }
}

pub trait Env {
    fn get(&mut self, name: &str, idx: Option<&str>) -> Option<String>;
    fn set(&mut self, name: &str, idx: Option<&str>, val: String) -> Result<(), String>;
    fn get_fast(&mut self, name: &str, idx: Option<&str>) -> Option<Cow<'_, str>> {
        self.get(name, idx).map(Cow::Owned)
    }
    fn set_num(&mut self, name: &str, idx: Option<&str>, val: Num) -> Result<(), String> {
        self.set(name, idx, val.fmt())
    }
    fn strict_arith(&self) -> bool { false }
    fn nounset(&self) -> bool { false }
}

type Lv<'a> = Option<(&'a str, Option<&'a str>)>;

struct Ev<'a, 'e> {
    s: &'a str,
    pos: usize,
    env: &'e mut dyn Env,
    noeval: u32,
    depth: u32,
    expr_depth: u32,
    strict_arith: bool,
    nounset: bool,
}

pub fn eval(expr: &str, env: &mut dyn Env) -> Result<Num, String> {
    eval_depth(expr, env, 0)
}

fn eval_depth(expr: &str, env: &mut dyn Env, depth: u32) -> Result<Num, String> {
    if depth > 64 {
        return Err("expression recursion level exceeded".into());
    }
    let strict = env.strict_arith();
    let nounset = env.nounset();
    let mut ev = Ev {
        s: expr,
        pos: 0,
        env,
        noeval: 0,
        depth,
        expr_depth: 0,
        strict_arith: strict,
        nounset,
    };
    ev.ws();
    if ev.pos >= ev.s.len() {
        return Ok(Num::I(0));
    }
    let (v, _) = ev.comma()?;
    ev.ws();
    if ev.pos < ev.s.len() {
        let rest = &ev.s[ev.pos..];
        return Err(format!("{}: syntax error in expression (error token is \"{}\")", expr.trim(), rest));
    }
    Ok(v)
}

fn parse_number(t: &str, strict_arith: bool) -> Option<Num> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        let n = match i64::from_str_radix(h, 16) {
            Ok(i) => i,
            Err(_) => u64::from_str_radix(h, 16).ok()? as i64,
        };
        Num::I(n)
    } else if let Some(i) = body.find('#') {
        let base_str = &body[..i];
        // Base must not have leading zeros (except "0" itself which is invalid anyway)
        if base_str.len() > 1 && base_str.starts_with('0') {
            return None;
        }
        let base: u32 = base_str.parse().ok()?;
        if !(2..=64).contains(&base) {
            return None;
        }
        let digits = &body[i + 1..];
        let mut val: i64 = 0;
        for c in digits.bytes() {
            let d = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'A'..=b'Z' => {
                    if base <= 36 {
                        10 + (c - b'A') as u32
                    } else {
                        36 + (c - b'A') as u32
                    }
                }
                b'a'..=b'z' => 10 + (c - b'a') as u32,
                b'@' => 62,
                b'_' => 63,
                _ => return None,
            };
            if d >= base {
                return None;
            }
            val = val.wrapping_mul(base as i64).wrapping_add(d as i64);
        }
        Num::I(val)
    } else if body.len() > 1 && body.starts_with('0') && body.bytes().all(|b| b.is_ascii_digit()) {
        let n = match i64::from_str_radix(body, 8) {
            Ok(i) => i,
            Err(_) => u64::from_str_radix(body, 8).ok()? as i64,
        };
        Num::I(n)
    } else if body.bytes().all(|b| b.is_ascii_digit()) {
        match body.parse::<i64>() {
            Ok(i) => Num::I(i),
            Err(_) => Num::I(body.parse::<u64>().ok()? as i64),
        }
    } else if body.as_bytes().first().map(|b| b.is_ascii_digit() || *b == b'.').unwrap_or(false) {
        if body.contains('.') || body.contains('e') || body.contains('E') {
            if strict_arith {
                return None;
            }
            Num::F(body.parse::<f64>().ok()?)
        } else {
            Num::I(body.parse::<i64>().ok()?)
        }
    } else {
        return None;
    };
    Some(if neg {
        match v {
            Num::I(i) => Num::I(i.wrapping_neg()),
            Num::F(f) => Num::F(-f),
        }
    } else {
        v
    })
}

fn ipow(mut base: i64, mut exp: u64) -> i64 {
    let mut acc: i64 = 1;
    while exp > 0 {
        if (exp & 1) != 0 {
            acc = acc.wrapping_mul(base);
        }
        exp >>= 1;
        if exp > 0 {
            base = base.wrapping_mul(base);
        }
    }
    acc
}

fn binop(op: &str, a: Num, b: Num) -> Result<Num, String> {
    let float = matches!(a, Num::F(_)) || matches!(b, Num::F(_));
    let bool_ = |x: bool| Ok(Num::I(x as i64));
    match op {
        "==" => return bool_(if float { a.as_f() == b.as_f() } else { a.as_i() == b.as_i() }),
        "!=" => return bool_(if float { a.as_f() != b.as_f() } else { a.as_i() != b.as_i() }),
        "<" => return bool_(if float { a.as_f() < b.as_f() } else { a.as_i() < b.as_i() }),
        "<=" => return bool_(if float { a.as_f() <= b.as_f() } else { a.as_i() <= b.as_i() }),
        ">" => return bool_(if float { a.as_f() > b.as_f() } else { a.as_i() > b.as_i() }),
        ">=" => return bool_(if float { a.as_f() >= b.as_f() } else { a.as_i() >= b.as_i() }),
        "|" => return Ok(Num::I(a.as_i() | b.as_i())),
        "^" => return Ok(Num::I(a.as_i() ^ b.as_i())),
        "&" => return Ok(Num::I(a.as_i() & b.as_i())),
        "<<" => {
            let y = b.as_i();
            if y < 0 {
                return Err("negative shift".into());
            }
            if y >= 64 {
                return Ok(Num::I(0));
            }
            return Ok(Num::I(a.as_i().wrapping_shl(y as u32)));
        }
        ">>" => {
            let y = b.as_i();
            if y < 0 {
                return Err("negative shift".into());
            }
            let x = a.as_i();
            if y >= 64 {
                return Ok(Num::I(if x < 0 { -1 } else { 0 }));
            }
            return Ok(Num::I(x.wrapping_shr(y as u32)));
        }
        _ => {}
    }
    if float {
        let (x, y) = (a.as_f(), b.as_f());
        return Ok(Num::F(match op {
            "+" => x + y,
            "-" => x - y,
            "*" => x * y,
            "/" => x / y,
            "%" => x % y,
            "**" => x.powf(y),
            _ => return Err(format!("bad operator {}", op)),
        }));
    }
    let (x, y) = (a.as_i(), b.as_i());
    Ok(match op {
        "+" => Num::I(x.wrapping_add(y)),
        "-" => Num::I(x.wrapping_sub(y)),
        "*" => Num::I(x.wrapping_mul(y)),
        "/" | "%" => {
            if y == 0 {
                return Err("division by 0".into());
            }
            Num::I(if op == "/" { x.wrapping_div(y) } else { x.wrapping_rem(y) })
        }
        "**" => {
            if y < 0 {
                return Err("negative exponent".into());
            }
            Num::I(ipow(x, y as u64))
        }
        _ => return Err(format!("bad operator {}", op)),
    })
}

impl<'a, 'e> Ev<'a, 'e> {
    #[inline]
    fn bytes(&self) -> &'a [u8] {
        self.s.as_bytes()
    }

    #[inline]
    fn peek_byte(&self) -> Option<u8> {
        self.bytes().get(self.pos).copied()
    }

    #[inline]
    fn ws(&mut self) {
        let bytes = self.bytes();
        while self.pos < bytes.len() {
            let b = bytes[self.pos];
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else if b >= 0x80 {
                if let Some(c) = self.s[self.pos..].chars().next() {
                    if c.is_whitespace() {
                        self.pos += c.len_utf8();
                        continue;
                    }
                }
                break;
            } else {
                break;
            }
        }
    }

    #[inline]
    fn starts(&self, t: &str) -> bool {
        self.bytes()[self.pos..].starts_with(t.as_bytes())
    }

    #[inline]
    fn enter(&mut self) -> Result<(), String> {
        if self.expr_depth >= MAX_EXPR_DEPTH {
            return Err("expression recursion level exceeded".into());
        }
        self.expr_depth += 1;
        Ok(())
    }

    #[inline]
    fn leave(&mut self) {
        self.expr_depth -= 1;
    }

    fn comma(&mut self) -> Result<(Num, Lv<'a>), String> {
        let mut v = self.assign()?;
        loop {
            self.ws();
            if self.peek_byte() == Some(b',') {
                self.pos += 1;
                v = self.assign()?;
            } else {
                return Ok(v);
            }
        }
    }

    fn store(&mut self, lv: &(&'a str, Option<&'a str>), n: Num) -> Result<(), String> {
        if self.noeval == 0 {
            self.env.set_num(lv.0, lv.1, n)?;
        }
        Ok(())
    }

    fn peek_assign_op(&self) -> Option<(&'static str, usize)> {
        let rem = &self.bytes()[self.pos..];
        let b0 = *rem.first()?;
        let b1 = rem.get(1).copied();
        let b2 = rem.get(2).copied();
        match (b0, b1, b2) {
            (b'<', Some(b'<'), Some(b'=')) => Some(("<<", 3)),
            (b'>', Some(b'>'), Some(b'=')) => Some((">>", 3)),
            (b'*', Some(b'*'), Some(b'=')) => Some(("**", 3)),
            (b'+', Some(b'='), _) => Some(("+", 2)),
            (b'-', Some(b'='), _) => Some(("-", 2)),
            (b'*', Some(b'='), _) => Some(("*", 2)),
            (b'/', Some(b'='), _) => Some(("/", 2)),
            (b'%', Some(b'='), _) => Some(("%", 2)),
            (b'&', Some(b'='), _) => Some(("&", 2)),
            (b'^', Some(b'='), _) => Some(("^", 2)),
            (b'|', Some(b'='), _) => Some(("|", 2)),
            (b'=', b, _) if b != Some(b'=') => Some(("=", 1)),
            _ => None,
        }
    }

    fn assign(&mut self) -> Result<(Num, Lv<'a>), String> {
        self.enter()?;
        let res = (|| {
            let (v, lv) = self.ternary()?;
            self.ws();
            if let Some(lv) = lv {
                if let Some((op, len)) = self.peek_assign_op() {
                    self.pos += len;
                    let (rhs, _) = self.assign()?;
                    let nv = if op == "=" {
                        rhs
                    } else if self.noeval > 0 {
                        Num::I(0)
                    } else {
                        binop(op, v, rhs)?
                    };
                    self.store(&lv, nv)?;
                    return Ok((nv, None));
                }
            }
            Ok((v, lv))
        })();
        self.leave();
        res
    }

    fn ternary(&mut self) -> Result<(Num, Lv<'a>), String> {
        let (c, lv) = self.binary(1)?;
        self.ws();
        if self.peek_byte() != Some(b'?') {
            return Ok((c, lv));
        }
        self.enter()?;
        let res = (|| {
            self.pos += 1;
            let t = c.truthy();
            if !t {
                self.noeval += 1;
            }
            let a = self.assign();
            if !t {
                self.noeval -= 1;
            }
            let (a, _) = a?;
            self.ws();
            if self.peek_byte() != Some(b':') {
                return Err("`:' expected for conditional expression".into());
            }
            self.pos += 1;
            if t {
                self.noeval += 1;
            }
            let b = self.ternary();
            if t {
                self.noeval -= 1;
            }
            let (b, _) = b?;
            Ok((if t { a } else { b }, None))
        })();
        self.leave();
        res
    }

    #[inline]
    fn peek_binop(&mut self) -> Option<(&'static str, u8)> {
        self.ws();
        let rem = &self.bytes()[self.pos..];
        let b0 = *rem.first()?;
        let b1 = rem.get(1).copied();
        let b2 = rem.get(2).copied();
        let (op, prec, len, allow_eq) = match (b0, b1) {
            (b'|', Some(b'|')) => ("||", 1, 2, false),
            (b'&', Some(b'&')) => ("&&", 2, 2, false),
            (b'*', Some(b'*')) => ("**", 11, 2, false),
            (b'=', Some(b'=')) => ("==", 6, 2, true),
            (b'!', Some(b'=')) => ("!=", 6, 2, true),
            (b'<', Some(b'<')) => ("<<", 8, 2, false),
            (b'>', Some(b'>')) => (">>", 8, 2, false),
            (b'<', Some(b'=')) => ("<=", 7, 2, true),
            (b'>', Some(b'=')) => (">=", 7, 2, true),
            (b'|', _) => ("|", 3, 1, false),
            (b'^', _) => ("^", 4, 1, false),
            (b'&', _) => ("&", 5, 1, false),
            (b'<', _) => ("<", 7, 1, false),
            (b'>', _) => (">", 7, 1, false),
            (b'+', _) => ("+", 9, 1, false),
            (b'-', _) => ("-", 9, 1, false),
            (b'*', _) => ("*", 10, 1, false),
            (b'/', _) => ("/", 10, 1, false),
            (b'%', _) => ("%", 10, 1, false),
            _ => return None,
        };
        let after = if len == 1 { b1 } else { b2 };
        if after == Some(b'=') && !allow_eq {
            return None;
        }
        Some((op, prec))
    }

    fn binary(&mut self, min: u8) -> Result<(Num, Lv<'a>), String> {
        let (mut l, mut lv) = self.unary()?;
        while let Some((op, prec)) = self.peek_binop() {
            if prec < min {
                break;
            }
            self.pos += op.len();
            lv = None;
            let short = (op == "&&" && !l.truthy()) || (op == "||" && l.truthy());
            if short {
                self.noeval += 1;
            }
            let r = self.binary(if op == "**" { prec } else { prec + 1 });
            if short {
                self.noeval -= 1;
            }
            let (r, _) = r?;
            l = match op {
                "&&" => Num::I((l.truthy() && r.truthy()) as i64),
                "||" => Num::I((l.truthy() || r.truthy()) as i64),
                _ => {
                    if self.noeval > 0 {
                        Num::I(0)
                    } else {
                        binop(op, l, r)?
                    }
                }
            };
        }
        Ok((l, lv))
    }

    fn unary(&mut self) -> Result<(Num, Lv<'a>), String> {
        self.ws();
        let b0 = self.peek_byte();
        if matches!(b0, Some(b'+') | Some(b'-') | Some(b'!') | Some(b'~')) {
            self.enter()?;
            let res = (|| {
                if self.starts("++") || self.starts("--") {
                    let inc = b0 == Some(b'+');
                    self.pos += 2;
                    let (v, lv) = self.unary()?;
                    return match lv {
                        Some(lv) => {
                            let nv = binop(if inc { "+" } else { "-" }, v, Num::I(1))?;
                            self.store(&lv, nv)?;
                            Ok((nv, None))
                        }
                        None => Err("++/--: lvalue required".into()),
                    };
                }
                match b0 {
                    Some(b'!') => {
                        self.pos += 1;
                        let (v, _) = self.unary()?;
                        Ok((Num::I(!v.truthy() as i64), None))
                    }
                    Some(b'~') => {
                        self.pos += 1;
                        let (v, _) = self.unary()?;
                        Ok((Num::I(!v.as_i()), None))
                    }
                    Some(b'-') => {
                        self.pos += 1;
                        let (v, _) = self.unary()?;
                        Ok((
                            match v {
                                Num::I(i) => Num::I(i.wrapping_neg()),
                                Num::F(f) => Num::F(-f),
                            },
                            None,
                        ))
                    }
                    Some(b'+') => {
                        self.pos += 1;
                        let (v, _) = self.unary()?;
                        Ok((v, None))
                    }
                    _ => unreachable!(),
                }
            })();
            self.leave();
            return res;
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<(Num, Lv<'a>), String> {
        let (v, lv) = self.primary()?;
        self.ws();
        if let Some(l) = lv {
            if self.starts("++") || self.starts("--") {
                let inc = self.peek_byte() == Some(b'+');
                self.pos += 2;
                let nv = binop(if inc { "+" } else { "-" }, v, Num::I(1))?;
                self.store(&l, nv)?;
                return Ok((v, None));
            }
        }
        Ok((v, lv))
    }

    fn primary(&mut self) -> Result<(Num, Lv<'a>), String> {
        self.ws();
        let b = match self.peek_byte() {
            Some(b) => b,
            None => return Err("syntax error: operand expected".into()),
        };
        if b == b'(' {
            self.enter()?;
            let res = (|| {
                self.pos += 1;
                let (v, _) = self.comma()?;
                self.ws();
                if self.peek_byte() != Some(b')') {
                    return Err("missing `)'".into());
                }
                self.pos += 1;
                Ok((v, None))
            })();
            self.leave();
            return res;
        }
        if b.is_ascii_digit() || b == b'.' {
            let start = self.pos;
            let bytes = self.bytes();
            while let Some(&ch) = bytes.get(self.pos) {
                if ch.is_ascii_alphanumeric() || ch == b'.' || ch == b'#' || ch == b'_' || ch == b'@' {
                    self.pos += 1;
                } else if (ch == b'-' || ch == b'+')
                    && self.pos > start
                    && matches!(bytes[self.pos - 1], b'e' | b'E')
                    && !bytes[start..self.pos].starts_with(b"0x")
                    && !bytes[start..self.pos].starts_with(b"0X")
                    && bytes[start..self.pos - 1].iter().all(|x| x.is_ascii_digit() || *x == b'.')
                    && bytes.get(self.pos + 1).map(|x| x.is_ascii_digit()).unwrap_or(false)
                {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let t = &self.s[start..self.pos];
            return match parse_number(t, self.strict_arith) {
                Some(n) => Ok((n, None)),
                None => Err(format!("{}: invalid number", t)),
            };
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = self.pos;
            let bytes = self.bytes();
            while let Some(&ch) = bytes.get(self.pos) {
                if ch.is_ascii_alphanumeric() || ch == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let name = &self.s[start..self.pos];
            if self.peek_byte() == Some(b'(') {
                self.enter()?;
                let res = (|| {
                    self.pos += 1;
                    let mut args = Vec::new();
                    self.ws();
                    if self.peek_byte() == Some(b')') {
                        self.pos += 1;
                    } else {
                        loop {
                            let (a, _) = self.assign()?;
                            args.push(a);
                            self.ws();
                            if self.peek_byte() == Some(b',') {
                                self.pos += 1;
                            } else if self.peek_byte() == Some(b')') {
                                self.pos += 1;
                                break;
                            } else {
                                return Err(format!("{}: missing `)'", name));
                            }
                        }
                    }
                    Ok((call(name, &args)?, None))
                })();
                self.leave();
                return res;
            }
            let mut idx: Option<&'a str> = None;
            if self.peek_byte() == Some(b'[') {
                let mut depth = 0u32;
                let st = self.pos + 1;
                let mut j = st;
                let bytes = self.bytes();
                loop {
                    match bytes.get(j) {
                        None => return Err("missing `]'".into()),
                        Some(b'[') => depth += 1,
                        Some(b']') => {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                        }
                        _ => {}
                    }
                    j += 1;
                }
                idx = Some(&self.s[st..j]);
                self.pos = j + 1;
            }
            self.ws();
            let plain_assign = self.peek_byte() == Some(b'=') && self.bytes().get(self.pos + 1) != Some(&b'=');
            let v = if self.noeval > 0 || plain_assign {
                Num::I(0)
            } else {
                let eval_str: Option<String> = match self.env.get_fast(name, idx) {
                    None => {
                        if self.nounset {
                            return Err(format!("{}: unbound variable", name));
                        }
                        return Ok((Num::I(0), Some((name, idx))));
                    }
                    Some(s) => match parse_number(&s, self.strict_arith) {
                        Some(n) => return Ok((n, Some((name, idx)))),
                        None if s.trim().is_empty() => return Ok((Num::I(0), Some((name, idx)))),
                        None => {
                            if self.strict_arith {
                                return Err(format!("{}: invalid number", s));
                            }
                            Some(s.into_owned())
                        }
                    },
                };
                eval_depth(&eval_str.unwrap(), self.env, self.depth + 1)?
            };
            return Ok((v, Some((name, idx))));
        }
        let rest = &self.s[self.pos..];
        Err(format!("syntax error: operand expected (error token is \"{}\")", rest))
    }
}

fn call(name: &str, args: &[Num]) -> Result<Num, String> {
    let need = |n: usize| if args.len() == n { Ok(()) } else { Err(format!("{}: expected {} argument(s)", name, n)) };
    let f1 = |f: fn(f64) -> f64| -> Result<Num, String> {
        need(1)?;
        Ok(Num::F(f(args[0].as_f())))
    };
    match name {
        "sqrt" => f1(f64::sqrt),
        "sin" => f1(f64::sin),
        "cos" => f1(f64::cos),
        "tan" => f1(f64::tan),
        "atan" => f1(f64::atan),
        "exp" => f1(f64::exp),
        "log" => f1(f64::ln),
        "log2" => f1(f64::log2),
        "log10" => f1(f64::log10),
        "floor" => {
            need(1)?;
            Ok(Num::I(args[0].as_f().floor() as i64))
        }
        "ceil" => {
            need(1)?;
            Ok(Num::I(args[0].as_f().ceil() as i64))
        }
        "round" => {
            need(1)?;
            Ok(Num::I(args[0].as_f().round() as i64))
        }
        "int" => {
            need(1)?;
            Ok(Num::I(args[0].as_i()))
        }
        "float" => {
            need(1)?;
            Ok(Num::F(args[0].as_f()))
        }
        "abs" => {
            need(1)?;
            Ok(match args[0] {
                Num::I(i) => Num::I(i.wrapping_abs()),
                Num::F(f) => Num::F(f.abs()),
            })
        }
        "pow" => {
            need(2)?;
            binop("**", args[0], args[1])
        }
        "min" | "max" => {
            if args.is_empty() {
                return Err(format!("{}: expected at least 1 argument", name));
            }
            let mut best = args[0];
            for a in &args[1..] {
                let less = a.as_f() < best.as_f();
                if less == (name == "min") && a.as_f() != best.as_f() {
                    best = *a;
                }
            }
            Ok(best)
        }
        _ => Err(format!("{}: unknown function", name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct DummyEnv(HashMap<String, String>);
    impl Env for DummyEnv {
        fn get(&mut self, name: &str, _idx: Option<&str>) -> Option<String> {
            self.0.get(name).cloned()
        }
        fn set(&mut self, name: &str, _idx: Option<&str>, val: String) -> Result<(), String> {
            self.0.insert(name.to_string(), val);
            Ok(())
        }
    }

    #[test]
    fn overflow_and_depth_safety() {
        let mut env = DummyEnv(HashMap::new());
        // Base#digits overflow must wrap cleanly without panicking.
        assert!(eval("36#zzzzzzzzzzzzzzzzzzzz", &mut env).is_ok());
        // Deep parentheses must error cleanly instead of stack overflow.
        let deep = "(".repeat(300) + "1" + &")".repeat(300);
        assert!(eval(&deep, &mut env).is_err());
        // Large shifts and exponents do not truncate silently.
        assert_eq!(eval("1 << 64", &mut env).unwrap(), Num::I(0));
        assert_eq!(eval("2 ** 64", &mut env).unwrap(), Num::I(0));
        assert_eq!(eval("(-1) ** 3", &mut env).unwrap(), Num::I(-1));
    }
}
