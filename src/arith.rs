//! Arithmetic evaluation: C-like integer arithmetic (as in POSIX/bash) extended
//! with floating point numbers and a small math library (as in zsh).

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
}

type Lv = Option<(String, Option<String>)>;

struct Ev<'a> {
    s: Vec<char>,
    pos: usize,
    env: &'a mut dyn Env,
    noeval: u32,
    depth: u32,
}

pub fn eval(expr: &str, env: &mut dyn Env) -> Result<Num, String> {
    eval_depth(expr, env, 0)
}

fn eval_depth(expr: &str, env: &mut dyn Env, depth: u32) -> Result<Num, String> {
    if depth > 64 {
        return Err("expression recursion level exceeded".into());
    }
    let mut ev = Ev { s: expr.chars().collect(), pos: 0, env, noeval: 0, depth };
    ev.ws();
    if ev.pos >= ev.s.len() {
        return Ok(Num::I(0));
    }
    let (v, _) = ev.comma()?;
    ev.ws();
    if ev.pos < ev.s.len() {
        let rest: String = ev.s[ev.pos..].iter().collect();
        return Err(format!("{}: syntax error in expression (error token is \"{}\")", expr.trim(), rest));
    }
    Ok(v)
}

fn parse_number(t: &str) -> Option<Num> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Num::I(i64::from_str_radix(h, 16).ok()?)
    } else if let Some(i) = body.find('#') {
        let base: u32 = body[..i].parse().ok()?;
        if !(2..=36).contains(&base) {
            return None;
        }
        Num::I(i64::from_str_radix(&body[i + 1..], base).ok()?)
    } else if body.len() > 1 && body.starts_with('0') && body.chars().all(|c| c.is_ascii_digit()) {
        Num::I(i64::from_str_radix(body, 8).ok()?)
    } else if body.chars().all(|c| c.is_ascii_digit()) {
        match body.parse::<i64>() {
            Ok(i) => Num::I(i),
            Err(_) => Num::I(body.parse::<u64>().ok()? as i64),
        }
    } else if body.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        Num::F(body.parse::<f64>().ok()?)
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
        "<<" => return Ok(Num::I(a.as_i().wrapping_shl(b.as_i() as u32))),
        ">>" => return Ok(Num::I(a.as_i().wrapping_shr(b.as_i() as u32))),
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
                Num::F((x as f64).powf(y as f64))
            } else {
                Num::I(x.wrapping_pow(y as u32))
            }
        }
        _ => return Err(format!("bad operator {}", op)),
    })
}

const BINOPS: &[(&str, u8)] = &[
    ("||", 1),
    ("&&", 2),
    ("**", 11),
    ("==", 6),
    ("!=", 6),
    ("<<", 8),
    (">>", 8),
    ("<=", 7),
    (">=", 7),
    ("|", 3),
    ("^", 4),
    ("&", 5),
    ("<", 7),
    (">", 7),
    ("+", 9),
    ("-", 9),
    ("*", 10),
    ("/", 10),
    ("%", 10),
];

const ASSIGN_OPS: &[&str] = &["<<=", ">>=", "**=", "+=", "-=", "*=", "/=", "%=", "&=", "^=", "|=", "="];

impl<'a> Ev<'a> {
    fn ws(&mut self) {
        while self.pos < self.s.len() && self.s[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }
    fn starts(&self, t: &str) -> bool {
        let mut i = self.pos;
        for c in t.chars() {
            if self.s.get(i) != Some(&c) {
                return false;
            }
            i += 1;
        }
        true
    }

    fn comma(&mut self) -> Result<(Num, Lv), String> {
        let mut v = self.assign()?;
        loop {
            self.ws();
            if self.starts(",") {
                self.pos += 1;
                v = self.assign()?;
            } else {
                return Ok(v);
            }
        }
    }

    fn store(&mut self, lv: &(String, Option<String>), n: Num) -> Result<(), String> {
        if self.noeval == 0 {
            self.env.set(&lv.0, lv.1.as_deref(), n.fmt())?;
        }
        Ok(())
    }

    fn assign(&mut self) -> Result<(Num, Lv), String> {
        let (v, lv) = self.ternary()?;
        self.ws();
        if let Some(lv) = lv.clone() {
            for op in ASSIGN_OPS {
                if self.starts(op) && !(*op == "=" && self.starts("==")) {
                    self.pos += op.len();
                    let (rhs, _) = self.assign()?;
                    let nv = if *op == "=" {
                        rhs
                    } else if self.noeval > 0 {
                        Num::I(0)
                    } else {
                        binop(&op[..op.len() - 1], v, rhs)?
                    };
                    self.store(&lv, nv)?;
                    return Ok((nv, None));
                }
            }
        }
        Ok((v, lv))
    }

    fn ternary(&mut self) -> Result<(Num, Lv), String> {
        let (c, lv) = self.binary(1)?;
        self.ws();
        if !self.starts("?") {
            return Ok((c, lv));
        }
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
        if !self.starts(":") {
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
    }

    fn peek_binop(&mut self) -> Option<(&'static str, u8)> {
        self.ws();
        for (op, prec) in BINOPS {
            if self.starts(op) {
                // not an assignment operator such as += or <<=
                let after = self.s.get(self.pos + op.len());
                if after == Some(&'=') && !matches!(*op, "==" | "!=" | "<=" | ">=") {
                    return None;
                }
                return Some((op, *prec));
            }
        }
        None
    }

    fn binary(&mut self, min: u8) -> Result<(Num, Lv), String> {
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

    fn unary(&mut self) -> Result<(Num, Lv), String> {
        self.ws();
        if self.starts("++") || self.starts("--") {
            let inc = self.starts("++");
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
        match self.s.get(self.pos) {
            Some('!') => {
                self.pos += 1;
                let (v, _) = self.unary()?;
                Ok((Num::I(!v.truthy() as i64), None))
            }
            Some('~') => {
                self.pos += 1;
                let (v, _) = self.unary()?;
                Ok((Num::I(!v.as_i()), None))
            }
            Some('-') => {
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
            Some('+') => {
                self.pos += 1;
                let (v, _) = self.unary()?;
                Ok((v, None))
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<(Num, Lv), String> {
        let (v, lv) = self.primary()?;
        self.ws();
        if let Some(l) = &lv {
            if self.starts("++") || self.starts("--") {
                let inc = self.starts("++");
                self.pos += 2;
                let nv = binop(if inc { "+" } else { "-" }, v, Num::I(1))?;
                self.store(l, nv)?;
                return Ok((v, None));
            }
        }
        Ok((v, lv))
    }

    fn primary(&mut self) -> Result<(Num, Lv), String> {
        self.ws();
        let c = match self.s.get(self.pos) {
            Some(c) => *c,
            None => return Err("syntax error: operand expected".into()),
        };
        if c == '(' {
            self.pos += 1;
            let (v, _) = self.comma()?;
            self.ws();
            if !self.starts(")") {
                return Err("missing `)'".into());
            }
            self.pos += 1;
            return Ok((v, None));
        }
        if c.is_ascii_digit() || c == '.' {
            let start = self.pos;
            while let Some(&ch) = self.s.get(self.pos) {
                if ch.is_ascii_alphanumeric() || ch == '.' || ch == '#' || ch == '_' || ch == '@' {
                    self.pos += 1;
                } else if (ch == '-' || ch == '+') && self.pos > start && matches!(self.s[self.pos - 1], 'e' | 'E') && !self.s[start..self.pos].iter().collect::<String>().starts_with("0x") && self.s[start..self.pos - 1].iter().all(|x| x.is_ascii_digit() || *x == '.') && self.s.get(self.pos + 1).map(|x| x.is_ascii_digit()).unwrap_or(false) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let t: String = self.s[start..self.pos].iter().collect();
            return match parse_number(&t) {
                Some(n) => Ok((n, None)),
                None => Err(format!("{}: invalid number", t)),
            };
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = self.pos;
            while let Some(&ch) = self.s.get(self.pos) {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let name: String = self.s[start..self.pos].iter().collect();
            if self.s.get(self.pos) == Some(&'(') {
                self.pos += 1;
                let mut args = Vec::new();
                self.ws();
                if self.starts(")") {
                    self.pos += 1;
                } else {
                    loop {
                        let (a, _) = self.assign()?;
                        args.push(a);
                        self.ws();
                        if self.starts(",") {
                            self.pos += 1;
                        } else if self.starts(")") {
                            self.pos += 1;
                            break;
                        } else {
                            return Err(format!("{}: missing `)'", name));
                        }
                    }
                }
                return Ok((call(&name, &args)?, None));
            }
            let mut idx = None;
            if self.s.get(self.pos) == Some(&'[') {
                let mut depth = 0;
                let st = self.pos + 1;
                let mut j = st;
                loop {
                    match self.s.get(j) {
                        None => return Err("missing `]'".into()),
                        Some('[') => depth += 1,
                        Some(']') => {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                        }
                        _ => {}
                    }
                    j += 1;
                }
                idx = Some(self.s[st..j].iter().collect::<String>());
                self.pos = j + 1;
            }
            self.ws();
            let plain_assign = self.starts("=") && !self.starts("==");
            let v = if self.noeval > 0 || plain_assign {
                Num::I(0)
            } else {
                match self.env.get(&name, idx.as_deref()) {
                    None => Num::I(0),
                    Some(s) => match parse_number(&s) {
                        Some(n) => n,
                        None if s.trim().is_empty() => Num::I(0),
                        None => eval_depth(&s, self.env, self.depth + 1)?,
                    },
                }
            };
            return Ok((v, Some((name, idx))));
        }
        let rest: String = self.s[self.pos..].iter().collect();
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
