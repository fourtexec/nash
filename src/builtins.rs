//! Builtin commands.

use crate::exec::ERR_TRAP;
use crate::expand::shell_quote;
use crate::shell::*;
use crate::sys;
use std::rc::Rc;

pub const BUILTINS: &[&str] = &[
    ".", ":", "[", "abbr", "alias", "bg", "break", "builtin", "cd", "command", "complete", "contains", "continue", "count", "declare", "defer", "dirs", "disown", "echo", "eval", "exec", "exit", "export", "false", "fg", "functions", "getopts", "hash", "help", "history", "jobs", "kill", "let", "local", "mapfile", "math", "path", "popd", "printf", "pushd", "pwd", "read", "readarray", "readonly", "realpath", "return", "set", "set_color", "setopt", "shift", "shopt", "source", "status", "string", "test", "trap", "true", "type", "typeset", "ulimit", "umask", "unalias", "unset", "unsetopt", "wait", "which", "z", "zi",
];

pub fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(&name)
}

/// Interpret backslash escapes (echo -e, printf %b). Returns (text, stop) where
/// `stop` is set by `\c`.
pub fn unescape(s: &str) -> (String, bool) {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] != '\\' || i + 1 >= cs.len() {
            out.push(cs[i]);
            i += 1;
            continue;
        }
        i += 1;
        let c = cs[i];
        i += 1;
        match c {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'e' | 'E' => out.push('\x1b'),
            'f' => out.push('\x0c'),
            'v' => out.push('\x0b'),
            '\\' => out.push('\\'),
            'c' => return (out, true),
            '0'..='7' => {
                let mut v = if c == '0' { 0 } else { c.to_digit(8).unwrap() };
                let mut n = if c == '0' { 0 } else { 1 };
                while n < 3 && i < cs.len() {
                    match cs[i].to_digit(8) {
                        Some(d) => {
                            v = v * 8 + d;
                            i += 1;
                            n += 1;
                        }
                        None => break,
                    }
                }
                if let Some(ch) = char::from_u32(v) {
                    out.push(ch);
                }
            }
            'x' | 'u' | 'U' => {
                let max = match c {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let mut v = 0u32;
                let mut n = 0;
                while n < max && i < cs.len() {
                    match cs[i].to_digit(16) {
                        Some(d) => {
                            v = v * 16 + d;
                            i += 1;
                            n += 1;
                        }
                        None => break,
                    }
                }
                if n == 0 {
                    out.push('\\');
                    out.push(c);
                } else if let Some(ch) = char::from_u32(v) {
                    out.push(ch);
                }
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    (out, false)
}

fn pad(s: String, width: usize, left: bool, zero: bool) -> String {
    let n = s.chars().count();
    if n >= width {
        return s;
    }
    let fill = width - n;
    if left {
        format!("{}{}", s, " ".repeat(fill))
    } else if zero {
        let (sign, digits) = if s.starts_with('-') || s.starts_with('+') { s.split_at(1) } else { ("", s.as_str()) };
        format!("{}{}{}", sign, "0".repeat(fill), digits)
    } else {
        format!("{}{}", " ".repeat(fill), s)
    }
}

fn fmt_exp(x: f64, prec: usize, upper: bool) -> String {
    let s = format!("{:.*e}", prec, x);
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    let r = format!("{}e{}{:02}", mant, if e < 0 { '-' } else { '+' }, e.abs());
    if upper {
        r.to_uppercase()
    } else {
        r
    }
}

fn fmt_g(x: f64, prec: usize, upper: bool, alt: bool) -> String {
    let p = if prec == 0 { 1 } else { prec };
    if x == 0.0 {
        return "0".into();
    }
    let e = x.abs().log10().floor() as i32;
    let strip = |s: String| {
        if alt || !s.contains('.') {
            return s;
        }
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    if e < -4 || e >= p as i32 {
        let s = fmt_exp(x, p - 1, upper);
        let (m, ex) = s.split_once(if upper { 'E' } else { 'e' }).unwrap();
        format!("{}{}{}", strip(m.to_string()), if upper { 'E' } else { 'e' }, ex)
    } else {
        strip(format!("{:.*}", (p as i32 - 1 - e).max(0) as usize, x))
    }
}

fn parse_int_arg(a: &str, ok: &mut bool) -> i64 {
    let t = a.trim();
    if t.is_empty() {
        return 0;
    }
    if let Some(r) = t.strip_prefix('\'').or_else(|| t.strip_prefix('"')) {
        return r.chars().next().map(|c| c as i64).unwrap_or(0);
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(h, 16)
    } else if body.len() > 1 && body.starts_with('0') {
        i64::from_str_radix(body, 8)
    } else {
        body.parse::<i64>()
    };
    match v {
        Ok(v) => {
            if neg {
                -v
            } else {
                v
            }
        }
        Err(_) => {
            sherr!("printf: {}: invalid number", a);
            *ok = false;
            0
        }
    }
}

/// Format according to a printf format string. Returns (output, ok).
pub fn printf_format(fmt: &str, args: &[String]) -> (String, bool) {
    let f: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut ai = 0;
    let mut ok = true;
    loop {
        let start_ai = ai;
        let mut i = 0;
        while i < f.len() {
            let c = f[i];
            if c == '\\' {
                let mut j = i + 1;
                // take the escape sequence
                let mut seq = String::from("\\");
                if j < f.len() {
                    seq.push(f[j]);
                    j += 1;
                    if matches!(seq.chars().last(), Some('0'..='7') | Some('x') | Some('u') | Some('U')) {
                        let lim = match seq.chars().last() {
                            Some('x') => 2,
                            Some('u') => 4,
                            Some('U') => 8,
                            _ => 2,
                        };
                        let mut n = 0;
                        while j < f.len() && n < lim && f[j].is_ascii_hexdigit() {
                            seq.push(f[j]);
                            j += 1;
                            n += 1;
                        }
                    }
                }
                if seq == "\\\"" {
                    out.push('"');
                } else if seq == "\\'" {
                    out.push('\'');
                } else {
                    let (s, stop) = unescape(&seq);
                    out.push_str(&s);
                    if stop {
                        return (out, ok);
                    }
                }
                i = j;
                continue;
            }
            if c != '%' {
                out.push(c);
                i += 1;
                continue;
            }
            i += 1;
            if i >= f.len() {
                out.push('%');
                break;
            }
            if f[i] == '%' {
                out.push('%');
                i += 1;
                continue;
            }
            let (mut left, mut zero, mut plus, mut space, mut alt) = (false, false, false, false, false);
            while i < f.len() {
                match f[i] {
                    '-' => left = true,
                    '0' => zero = true,
                    '+' => plus = true,
                    ' ' => space = true,
                    '#' => alt = true,
                    _ => break,
                }
                i += 1;
            }
            let next = |ai: &mut usize| -> Option<String> {
                let a = args.get(*ai).cloned();
                *ai += 1;
                a
            };
            let mut width = 0usize;
            if i < f.len() && f[i] == '*' {
                let w = next(&mut ai).map(|a| parse_int_arg(&a, &mut ok)).unwrap_or(0);
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
                    p = next(&mut ai).map(|a| parse_int_arg(&a, &mut ok)).unwrap_or(0).max(0) as usize;
                    i += 1;
                } else {
                    while i < f.len() && f[i].is_ascii_digit() {
                        p = p * 10 + f[i].to_digit(10).unwrap() as usize;
                        i += 1;
                    }
                }
                prec = Some(p);
            }
            while i < f.len() && matches!(f[i], 'l' | 'h' | 'j' | 'z' | 'L') {
                i += 1;
            }
            if i >= f.len() {
                break;
            }
            let conv = f[i];
            i += 1;
            let sign = |s: String, neg: bool| {
                if neg {
                    s
                } else if plus {
                    format!("+{}", s)
                } else if space {
                    format!(" {}", s)
                } else {
                    s
                }
            };
            match conv {
                's' | 'b' | 'q' => {
                    let a = next(&mut ai).unwrap_or_default();
                    let mut s = match conv {
                        'b' => {
                            let (s, stop) = unescape(&a);
                            if stop {
                                out.push_str(&s);
                                return (out, ok);
                            }
                            s
                        }
                        'q' => shell_quote(&a),
                        _ => a,
                    };
                    if let Some(p) = prec {
                        s = s.chars().take(p).collect();
                    }
                    out.push_str(&pad(s, width, left, false));
                }
                'c' => {
                    let a = next(&mut ai).unwrap_or_default();
                    out.push_str(&pad(a.chars().next().map(|c| c.to_string()).unwrap_or_default(), width, left, false));
                }
                'd' | 'i' => {
                    let v = next(&mut ai).map(|a| parse_int_arg(&a, &mut ok)).unwrap_or(0);
                    let mut s = v.unsigned_abs().to_string();
                    if let Some(p) = prec {
                        while s.len() < p {
                            s.insert(0, '0');
                        }
                    }
                    let s = if v < 0 { format!("-{}", s) } else { sign(s, false) };
                    out.push_str(&pad(s, width, left, zero && prec.is_none()));
                }
                'u' | 'o' | 'x' | 'X' => {
                    let v = next(&mut ai).map(|a| parse_int_arg(&a, &mut ok)).unwrap_or(0) as u64;
                    let mut s = match conv {
                        'u' => v.to_string(),
                        'o' => format!("{:o}", v),
                        'x' => format!("{:x}", v),
                        _ => format!("{:X}", v),
                    };
                    if let Some(p) = prec {
                        while s.len() < p {
                            s.insert(0, '0');
                        }
                    }
                    if alt && v != 0 {
                        s = match conv {
                            'o' => format!("0{}", s),
                            'x' => format!("0x{}", s),
                            'X' => format!("0X{}", s),
                            _ => s,
                        };
                    }
                    out.push_str(&pad(s, width, left, zero && prec.is_none()));
                }
                'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                    let a = next(&mut ai).unwrap_or_default();
                    let v: f64 = if a.trim().is_empty() {
                        0.0
                    } else {
                        match a.trim().parse::<f64>() {
                            Ok(v) => v,
                            Err(_) => parse_int_arg(&a, &mut ok) as f64,
                        }
                    };
                    let p = prec.unwrap_or(6);
                    let body = match conv {
                        'f' | 'F' => format!("{:.*}", p, v.abs()),
                        'e' => fmt_exp(v.abs(), p, false),
                        'E' => fmt_exp(v.abs(), p, true),
                        'g' => fmt_g(v.abs(), p, false, alt),
                        _ => fmt_g(v.abs(), p, true, alt),
                    };
                    let s = if v < 0.0 || (v == 0.0 && v.is_sign_negative()) { format!("-{}", body) } else { sign(body, false) };
                    out.push_str(&pad(s, width, left, zero));
                }
                other => {
                    sherr!("printf: `{}': invalid format character", other);
                    return (out, false);
                }
            }
        }
        if ai >= args.len() || ai == start_ai {
            break;
        }
    }
    (out, ok)
}

pub fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

pub fn fmt_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    } else {
        format!("{}h{}m", ms / 3_600_000, (ms % 3_600_000) / 60_000)
    }
}

const HELP: &str = "\
nash - a modern sh-compatible shell

Everything you know from sh/bash works: pipelines, redirections, functions,
arrays, [[ ]], (( )), ${var//pat/rep}, brace expansion, here-docs, job control.

nash additions:
  fn name(a, b=default, ...rest) { ... }   functions with named parameters
  try { cmds; } catch { echo $ERROR; }     structured error handling
  defer cmd args...                        run a command when the function exits
  $(( 1.5 * 2 )), sqrt(), min(), ...       floating point arithmetic
  set -o strict                            errexit + nounset + pipefail
  cmd | read var                           the last pipeline stage runs in the shell
  **/*.rs                                  recursive globbing, always on
  z <part of dir name>                     jump to a frequently used directory
  .. / ... / dirname                       cd without typing cd
  history --here | --failed | --long       history with context
  syntax highlighting, autosuggestions, menu completion, `did you mean' hints

Prompt escapes (PS1/RPROMPT): bash's \\u \\h \\w \\W \\$ \\t ... plus
  \\g git branch, \\? last exit status, \\L duration of the last command.

Builtins:";

impl Shell {
    pub fn run_builtin(&mut self, name: &str, argv: &[String]) -> R {
        let args = &argv[1..];
        match name {
            ":" | "true" => Ok(0),
            "false" => Ok(1),
            "echo" => {
                let mut nl = true;
                let mut esc = false;
                let mut i = 0;
                while i < args.len() {
                    let a = &args[i];
                    if a.len() > 1 && a.starts_with('-') && a[1..].chars().all(|c| matches!(c, 'n' | 'e' | 'E')) {
                        for c in a[1..].chars() {
                            match c {
                                'n' => nl = false,
                                'e' => esc = true,
                                _ => esc = false,
                            }
                        }
                        i += 1;
                    } else {
                        break;
                    }
                }
                let mut s = args[i..].join(" ");
                if esc {
                    let (t, stop) = unescape(&s);
                    s = t;
                    if stop {
                        nl = false;
                    }
                }
                if nl {
                    s.push('\n');
                }
                Ok(if sys::write_fd(1, s.as_bytes()) { 0 } else { 1 })
            }
            "printf" => {
                let mut a = args;
                let mut var = None;
                if a.len() >= 2 && a[0] == "-v" {
                    var = Some(a[1].clone());
                    a = &a[2..];
                }
                if !a.is_empty() && a[0] == "--" {
                    a = &a[1..];
                }
                if a.is_empty() {
                    sherr!("printf: usage: printf [-v var] format [arguments]");
                    return Ok(2);
                }
                let (s, ok) = printf_format(&a[0], &a[1..]);
                match var {
                    Some(v) => {
                        if let Err(e) = self.assign_ref(&v, s) {
                            sherr!("printf: {}", e);
                            return Ok(1);
                        }
                    }
                    None => {
                        if !sys::write_fd(1, s.as_bytes()) {
                            return Ok(1);
                        }
                    }
                }
                Ok(!ok as i32)
            }
            "cd" => self.builtin_cd(args),
            "pwd" => {
                let p = if args.first().map(|a| a == "-P").unwrap_or(false) { std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default() } else { self.cwd() };
                out!("{}\n", p);
                Ok(0)
            }
            "pushd" => {
                let old = self.cwd();
                let target = match args.first() {
                    Some(a) => a.clone(),
                    None => match self.dir_stack.pop() {
                        Some(t) => t,
                        None => {
                            sherr!("pushd: no other directory");
                            return Ok(1);
                        }
                    },
                };
                let r = self.builtin_cd(&[target])?;
                if r == 0 {
                    self.dir_stack.push(old);
                    self.print_dirs();
                }
                Ok(r)
            }
            "popd" => match self.dir_stack.pop() {
                Some(t) => {
                    let r = self.builtin_cd(&[t])?;
                    if r == 0 {
                        self.print_dirs();
                    }
                    Ok(r)
                }
                None => {
                    sherr!("popd: directory stack empty");
                    Ok(1)
                }
            },
            "dirs" => {
                if args.first().map(|a| a == "-c").unwrap_or(false) {
                    self.dir_stack.clear();
                } else {
                    self.print_dirs();
                }
                Ok(0)
            }
            "exit" => {
                let n = match args.first() {
                    Some(a) => match a.parse::<i64>() {
                        Ok(n) => (n & 0xff) as i32,
                        Err(_) => {
                            sherr!("exit: {}: numeric argument required", a);
                            2
                        }
                    },
                    None => self.last_status,
                };
                Err(Flow::Exit(n))
            }
            "return" => {
                let n = args.first().and_then(|a| a.parse::<i64>().ok()).map(|n| (n & 0xff) as i32).unwrap_or(self.last_status);
                if self.func_depth == 0 && self.source_depth == 0 {
                    sherr!("return: can only `return' from a function or sourced script");
                    return Ok(1);
                }
                Err(Flow::Return(n))
            }
            "break" | "continue" => {
                let n = args.first().and_then(|a| a.parse::<u32>().ok()).unwrap_or(1).max(1);
                if self.loop_depth == 0 {
                    return Ok(0);
                }
                let n = n.min(self.loop_depth);
                Err(if name == "break" { Flow::Break(n) } else { Flow::Continue(n) })
            }
            "shift" => {
                let n = args.first().and_then(|a| a.parse::<usize>().ok()).unwrap_or(1);
                if n > self.pos_params.len() {
                    return Ok(1);
                }
                self.pos_params.drain(..n);
                Ok(0)
            }
            "set" => self.builtin_set(args),
            "setopt" | "unsetopt" => {
                let on = name == "setopt";
                if args.is_empty() {
                    for n in OPTION_NAMES {
                        if self.opts.get(n) == Some(on) {
                            out!("{}\n", n);
                        }
                    }
                    return Ok(0);
                }
                let mut st = 0;
                for a in args {
                    let n = a.to_lowercase();
                    if !self.opts.set(&n, on) && !self.opts.set(&n.replace('_', ""), on) {
                        sherr!("{}: no such option: {}", name, a);
                        st = 1;
                    }
                }
                Ok(st)
            }
            "shopt" => {
                let mut on = None;
                let mut quiet = false;
                let mut st = 0;
                for a in args {
                    match a.as_str() {
                        "-s" => on = Some(true),
                        "-u" => on = Some(false),
                        "-q" => quiet = true,
                        "-o" | "-p" => {}
                        n => match on {
                            Some(v) => {
                                self.opts.set(n, v);
                            }
                            None => {
                                let v = self.opts.get(n).unwrap_or(false);
                                if !quiet {
                                    out!("{:<20}{}\n", n, if v { "on" } else { "off" });
                                }
                                if !v {
                                    st = 1;
                                }
                            }
                        },
                    }
                }
                Ok(st)
            }
            "unset" => {
                let mut funcs = false;
                let mut st = 0;
                for a in args {
                    match a.as_str() {
                        "-f" => funcs = true,
                        "-v" => funcs = false,
                        n => {
                            if funcs {
                                self.funcs.remove(n);
                            } else if let Some(i) = n.find('[').filter(|_| n.ends_with(']')) {
                                let (vn, idx) = (&n[..i], &n[i + 1..n.len() - 1]);
                                match self.var(vn).map(|v| v.val.clone()) {
                                    Some(Val::Arr(mut arr)) => {
                                        let mut k = self.arith(idx)?.as_i();
                                        if k < 0 {
                                            k += arr.len() as i64;
                                        }
                                        if k >= 0 && (k as usize) < arr.len() {
                                            arr.remove(k as usize);
                                            let _ = self.set(vn, Val::Arr(arr));
                                        }
                                    }
                                    Some(Val::Map(mut m)) => {
                                        let key = self.expand_string(idx)?;
                                        m.retain(|(k, _)| *k != key);
                                        let _ = self.set(vn, Val::Map(m));
                                    }
                                    _ => {}
                                }
                            } else if self.var(n).is_some() || !self.funcs.contains_key(n) {
                                if let Err(e) = self.unset_var(n) {
                                    sherr!("unset: {}", e);
                                    st = 1;
                                }
                            } else {
                                self.funcs.remove(n);
                            }
                        }
                    }
                }
                Ok(st)
            }
            "eval" => {
                let src = args.join(" ");
                let name = self.script_name.clone();
                self.run_source(&src, &name)
            }
            "." | "source" => {
                let file = match args.first() {
                    Some(f) => f.clone(),
                    None => {
                        sherr!("{}: filename argument required", name);
                        return Ok(2);
                    }
                };
                let mut path = file.clone();
                if !file.contains('/') && !std::path::Path::new(&file).is_file() {
                    let p = self.get("PATH").unwrap_or_default();
                    if let Some(f) = p.split(':').map(|d| format!("{}/{}", d, file)).find(|f| std::path::Path::new(f).is_file()) {
                        path = f;
                    }
                }
                self.source_file(&path, &args[1..])
            }
            "alias" => {
                if args.is_empty() {
                    let mut v: Vec<_> = self.aliases.iter().collect();
                    v.sort();
                    for (k, val) in v {
                        out!("alias {}={}\n", k, shell_quote(val));
                    }
                    return Ok(0);
                }
                let mut st = 0;
                for a in args {
                    if a == "-p" || a == "--" {
                        continue;
                    }
                    match a.find('=') {
                        Some(i) if i > 0 => {
                            Rc::make_mut(&mut self.aliases).insert(a[..i].to_string(), a[i + 1..].to_string());
                        }
                        _ => match self.aliases.get(a) {
                            Some(v) => {
                                out!("alias {}={}\n", a, shell_quote(v));
                            }
                            None => {
                                sherr!("alias: {}: not found", a);
                                st = 1;
                            }
                        },
                    }
                }
                Ok(st)
            }
            "unalias" => {
                let mut st = 0;
                for a in args {
                    if a == "-a" {
                        Rc::make_mut(&mut self.aliases).clear();
                    } else if Rc::make_mut(&mut self.aliases).remove(a).is_none() {
                        sherr!("unalias: {}: not found", a);
                        st = 1;
                    }
                }
                Ok(st)
            }
            "type" | "which" | "command" => self.builtin_type(name, args),
            "builtin" => match args.first() {
                None => Ok(0),
                Some(b) if is_builtin(b) => self.run_builtin(&b.clone(), args),
                Some(b) => {
                    sherr!("builtin: {}: not a shell builtin", b);
                    Ok(1)
                }
            },
            "hash" => {
                if args.first().map(|a| a == "-r").unwrap_or(false) {
                    self.hash.clear();
                    self.path_cmds = None;
                } else if args.is_empty() {
                    let mut v: Vec<_> = self.hash.iter().collect();
                    v.sort();
                    for (k, p) in v {
                        out!("{}\t{}\n", k, p);
                    }
                } else {
                    for a in args {
                        if self.find_command(a).is_none() {
                            sherr!("hash: {}: not found", a);
                            return Ok(1);
                        }
                    }
                }
                Ok(0)
            }
            "umask" => {
                match args.first() {
                    None => {
                        let m = unsafe {
                            let m = libc::umask(0);
                            libc::umask(m);
                            m
                        };
                        out!("{:04o}\n", m);
                    }
                    Some(a) => match u32::from_str_radix(a, 8) {
                        Ok(m) => unsafe {
                            libc::umask(m as libc::mode_t);
                        },
                        Err(_) => {
                            sherr!("umask: {}: invalid octal number", a);
                            return Ok(1);
                        }
                    },
                }
                Ok(0)
            }
            "trap" => self.builtin_trap(args),
            "read" => self.builtin_read(args),
            "mapfile" | "readarray" => {
                let mut trim = false;
                let mut var = "MAPFILE".to_string();
                for a in args {
                    if a == "-t" {
                        trim = true;
                    } else if !a.starts_with('-') {
                        var = a.clone();
                    }
                }
                let data = String::from_utf8_lossy(&sys::read_all(0)).into_owned();
                let mut lines: Vec<String> = data.split_inclusive('\n').map(|l| if trim { l.trim_end_matches('\n').to_string() } else { l.to_string() }).collect();
                if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                    lines.pop();
                }
                let _ = self.set(&var, Val::Arr(lines));
                Ok(0)
            }
            "test" => Ok(self.test_main(args)),
            "[" => {
                if args.last().map(|a| a != "]").unwrap_or(true) {
                    sherr!("[: missing `]'");
                    return Ok(2);
                }
                Ok(self.test_main(&args[..args.len() - 1]))
            }
            "let" => {
                let mut last = 0;
                for a in args {
                    last = self.arith(a)?.truthy() as i32;
                }
                Ok(1 - last)
            }
            "getopts" => self.builtin_getopts(args),
            "wait" => self.builtin_wait(args),
            "jobs" => {
                self.reap();
                let last = self.jobs.last().map(|j| j.id).unwrap_or(0);
                for j in &self.jobs {
                    let state = if j.done() {
                        "Done"
                    } else if j.stopped() {
                        "Stopped"
                    } else {
                        "Running"
                    };
                    if args.first().map(|a| a == "-p").unwrap_or(false) {
                        out!("{}\n", j.procs[0].pid);
                    } else if args.first().map(|a| a == "-l").unwrap_or(false) {
                        out!("[{}]{} {} {:<10} {}\n", j.id, if j.id == last { '+' } else { ' ' }, j.procs[0].pid, state, j.text);
                    } else {
                        out!("[{}]{}  {:<10} {}\n", j.id, if j.id == last { '+' } else { ' ' }, state, j.text);
                    }
                }
                self.jobs.retain(|j| !j.done());
                Ok(0)
            }
            "fg" | "bg" => {
                if !self.job_control {
                    sherr!("{}: no job control", name);
                    return Ok(1);
                }
                self.reap();
                let idx = match self.find_job(args.first().map(|s| s.as_str())) {
                    Some(i) => i,
                    None => {
                        sherr!("{}: {}: no such job", name, args.first().map(|s| s.as_str()).unwrap_or("current"));
                        return Ok(1);
                    }
                };
                if name == "bg" {
                    let j = &mut self.jobs[idx];
                    for p in &mut j.procs {
                        p.stopped = false;
                    }
                    unsafe {
                        libc::kill(-j.pgid, libc::SIGCONT);
                    }
                    err!("[{}] {} &\n", j.id, j.text);
                    return Ok(0);
                }
                let j = self.jobs.remove(idx);
                err!("{}\n", j.text);
                unsafe {
                    libc::tcsetpgrp(self.tty, j.pgid);
                    libc::kill(-j.pgid, libc::SIGCONT);
                }
                let pids: Vec<i32> = j.procs.iter().filter(|p| p.status.is_none()).map(|p| p.pid).collect();
                let sts = self.wait_job(j.pgid, pids, &j.text)?;
                Ok(sts.last().copied().unwrap_or(0))
            }
            "disown" => {
                if args.is_empty() {
                    self.jobs.pop();
                } else if args[0] == "-a" {
                    self.jobs.clear();
                } else {
                    for a in args {
                        if let Some(i) = self.find_job(Some(a)) {
                            self.jobs.remove(i);
                        }
                    }
                }
                Ok(0)
            }
            "kill" => self.builtin_kill(args),
            "history" => self.builtin_history(args),
            "complete" => {
                let mut words: Option<Vec<String>> = None;
                let mut func: Option<String> = None;
                let mut remove = false;
                let mut names = Vec::new();
                let mut i = 0;
                while i < args.len() {
                    match args[i].as_str() {
                        "-W" => {
                            i += 1;
                            words = args.get(i).map(|w| w.split_whitespace().map(|s| s.to_string()).collect());
                        }
                        "-F" => {
                            i += 1;
                            func = args.get(i).cloned();
                        }
                        "-r" => remove = true,
                        "-o" | "-A" | "-C" | "-X" | "-P" | "-S" | "-G" => i += 1,
                        a if a.starts_with('-') => {}
                        a => names.push(a.to_string()),
                    }
                    i += 1;
                }
                if names.is_empty() && !remove {
                    let mut v: Vec<_> = self.completions.iter().collect();
                    v.sort();
                    for (k, w) in v {
                        out!("complete -W {} {}\n", shell_quote(&w.join(" ")), k);
                    }
                    for (k, f) in &self.completion_funcs {
                        out!("complete -F {} {}\n", f, k);
                    }
                    return Ok(0);
                }
                for n in names {
                    if remove {
                        self.completions.remove(&n);
                        self.completion_funcs.remove(&n);
                    } else {
                        if let Some(w) = &words {
                            self.completions.insert(n.clone(), w.clone());
                        }
                        if let Some(f) = &func {
                            self.completion_funcs.insert(n, f.clone());
                        }
                    }
                }
                Ok(0)
            }
            "defer" => {
                if args.is_empty() {
                    sherr!("defer: usage: defer command [args...]");
                    return Ok(2);
                }
                if let Some(d) = self.defers.last_mut() {
                    d.push(args.to_vec());
                }
                Ok(0)
            }
            "z" => self.builtin_z(args),
            "zi" => {
                if self.find_command("zoxide").is_some() {
                    let cwd = self.cwd();
                    let mut cmd = std::process::Command::new("zoxide");
                    cmd.arg("query").arg("--interactive").arg("--exclude").arg(&cwd);
                    cmd.args(args);
                    match cmd.output() {
                        Ok(o) if o.status.success() => {
                            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            if path.is_empty() {
                                sherr!("zi: no match");
                                return Ok(1);
                            }
                            return self.builtin_cd(&[path]);
                        }
                        _ => {
                            sherr!("zi: no match");
                            return Ok(1);
                        }
                    }
                }
                sherr!("zi: zoxide not found");
                Ok(1)
            }
            "abbr" => {
                // fish-style abbreviations, mapped onto nash aliases.
                let mut erase = false;
                let mut i = 0;
                while i < args.len() && args[i].starts_with('-') {
                    match args[i].as_str() {
                        "-e" | "--erase" => erase = true,
                        "-a" | "--add" => {}
                        "--" => { i += 1; break; }
                        _ => {}
                    }
                    i += 1;
                }
                let rest = &args[i..];
                if erase {
                    for n in rest {
                        Rc::make_mut(&mut self.aliases).remove(n);
                    }
                    return Ok(0);
                }
                if rest.len() < 2 {
                    sherr!("abbr: usage: abbr name expansion");
                    return Ok(2);
                }
                let name = rest[0].clone();
                let expansion = rest[1..].join(" ");
                Rc::make_mut(&mut self.aliases).insert(name, expansion);
                Ok(0)
            }
            "contains" => {
                let mut print_index = false;
                let mut i = 0;
                while i < args.len() && args[i].starts_with('-') {
                    match args[i].as_str() {
                        "-i" | "--index" => print_index = true,
                        "--" => { i += 1; break; }
                        _ => {}
                    }
                    i += 1;
                }
                let rest = &args[i..];
                if rest.len() < 2 {
                    sherr!("contains: usage: contains needle haystack...");
                    return Ok(2);
                }
                let needle = &rest[0];
                for (j, v) in rest[1..].iter().enumerate() {
                    if v == needle {
                        if print_index {
                            out!("{}\n", j + 1);
                        }
                        return Ok(0);
                    }
                }
                Ok(1)
            }
            "count" => {
                let mut n = args.len();
                if !sys::isatty(0) {
                    let data = sys::read_all(0);
                    n += data.iter().filter(|b| **b == b'\n').count();
                }
                out!("{}\n", n);
                Ok(if n == 0 { 1 } else { 0 })
            }
            "functions" => {
                if args.is_empty() {
                    let mut names: Vec<&String> = self.funcs.keys().collect();
                    names.sort();
                    for n in names {
                        out!("{}\n", n);
                    }
                    return Ok(0);
                }
                let mut i = 0;
                let mut erase = false;
                let mut query = false;
                while i < args.len() && args[i].starts_with('-') {
                    match args[i].as_str() {
                        "-e" | "--erase" => erase = true,
                        "-q" | "--query" => query = true,
                        "--" => { i += 1; break; }
                        _ => {}
                    }
                    i += 1;
                }
                let names = &args[i..];
                if query {
                    let n = names.first().cloned().unwrap_or_default();
                    return Ok(if self.funcs.contains_key(&n) { 0 } else { 1 });
                }
                if erase {
                    for n in names {
                        self.funcs.remove(n);
                    }
                    return Ok(0);
                }
                if let Some(n) = names.first() {
                    if let Some(f) = self.funcs.get(n).cloned() {
                        if !f.text.is_empty() {
                            out!("{}\n", f.text);
                        } else {
                            out!("function {}\\n    (body)\\nend\\n", n);
                        }
                        return Ok(0);
                    }
                    return Ok(1);
                }
                Ok(0)
            }
            "math" => {
                let expr = args.join(" ");
                if expr.is_empty() {
                    sherr!("math: missing expression");
                    return Ok(2);
                }
                match self.arith(&expr) {
                    Ok(n) => {
                        out!("{}\n", n.fmt());
                        Ok(0)
                    }
                    Err(_) => Ok(1),
                }
            }
            "path" => self.builtin_path(args),
            "realpath" => {
                let mut require = false;
                let mut logical = false;
                let mut paths = Vec::new();
                for a in args {
                    match a.as_str() {
                        "-e" | "--canonicalize-existing" => require = true,
                        "-m" | "--canonicalize-missing" => logical = true,
                        "--" => {}
                        a if a.starts_with('-') => {}
                        _ => paths.push(a.clone()),
                    }
                }
                if paths.is_empty() {
                    sherr!("realpath: missing operand");
                    return Ok(1);
                }
                let mut st = 0;
                for p in paths {
                    let resolved = if logical {
                        // lexical normalisation
                        let base = if p.starts_with('/') { String::new() } else { self.cwd() };
                        let mut comps: Vec<String> = Vec::new();
                        for part in format!("{}/{}", base, p).split('/') {
                            match part {
                                "" | "." => {}
                                ".." => { comps.pop(); }
                                c => comps.push(c.to_string()),
                            }
                        }
                        format!("/{}", comps.join("/"))
                    } else {
                        match std::fs::canonicalize(&p) {
                            Ok(pb) => pb.to_string_lossy().into_owned(),
                            Err(_) => {
                                if require {
                                    sherr!("realpath: {}: No such file or directory", p);
                                    st = 1;
                                    continue;
                                }
                                // fall back to lexical
                                let base = if p.starts_with('/') { String::new() } else { self.cwd() };
                                let mut comps: Vec<String> = Vec::new();
                                for part in format!("{}/{}", base, p).split('/') {
                                    match part {
                                        "" | "." => {}
                                        ".." => { comps.pop(); }
                                        c => comps.push(c.to_string()),
                                    }
                                }
                                format!("/{}", comps.join("/"))
                            }
                        }
                    };
                    out!("{}\n", resolved);
                }
                Ok(st)
            }
            "set_color" => {
                // fish-style: set_color [options] color
                let mut bg = false;
                let mut bold = false;
                let mut color = None;
                for a in args {
                    match a.as_str() {
                        "-b" => bg = true,
                        "-o" | "--bold" => bold = true,
                        "--background" => bg = true,
                        c => color = Some(c.to_string()),
                    }
                }
                let code = match color.as_deref() {
                    Some("normal") | None => "0".to_string(),
                    Some("black") => "30".to_string(),
                    Some("red") => "31".to_string(),
                    Some("green") => "32".to_string(),
                    Some("yellow") => "33".to_string(),
                    Some("blue") => "34".to_string(),
                    Some("magenta") => "35".to_string(),
                    Some("cyan") => "36".to_string(),
                    Some("white") => "37".to_string(),
                    Some(c) if c.chars().all(|x| x.is_ascii_digit()) => {
                        let n: u8 = c.parse().unwrap_or(7);
                        if bg { format!("48;5;{}", n) } else { format!("38;5;{}", n) }
                    }
                    Some(_) => "0".to_string(),
                };
                let mut seq = String::from("\x1b[");
                if bg {
                    // background color already encoded
                }
                if bold {
                    seq.push_str("1;");
                }
                seq.push_str(&code);
                seq.push('m');
                out!("{}", seq);
                Ok(0)
            }
            "status" => {
                if args.is_empty() {
                    out!("{}\n", self.last_status);
                    return Ok(0);
                }
                match args[0].as_str() {
                    "is-interactive" => Ok(if self.interactive { 0 } else { 1 }),
                    "is-login" => Ok(if self.login { 0 } else { 1 }),
                    "current-command" | "current-filename" => {
                        out!("{}\n", self.script_name);
                        Ok(0)
                    }
                    "job-control" => Ok(if self.job_control { 0 } else { 1 }),
                    _ => {
                        sherr!("status: unknown subcommand `{}'", args[0]);
                        Ok(1)
                    }
                }
            }
            "string" => self.builtin_string(args),
            "ulimit" => self.builtin_ulimit(args),
            "help" => {
                out!("{}\n", HELP);
                let mut line = String::from(" ");
                for b in BUILTINS {
                    if line.len() + b.len() > 76 {
                        out!("{}\n", line);
                        line = String::from(" ");
                    }
                    line.push(' ');
                    line.push_str(b);
                }
                out!("{}\n", line);
                Ok(0)
            }
            _ => {
                sherr!("{}: builtin not implemented", name);
                Ok(1)
            }
        }
    }

    /// Assign to `name` or `name[index]`.
    pub fn assign_ref(&mut self, target: &str, val: String) -> Result<(), String> {
        if let Some(i) = target.find('[').filter(|_| target.ends_with(']')) {
            let (n, idx) = (&target[..i], &target[i + 1..target.len() - 1]);
            return self.set_elem(n, idx, val, false).map_err(|_| format!("{}: bad array subscript", target));
        }
        if !crate::parser::is_name(target) {
            return Err(format!("`{}': not a valid identifier", target));
        }
        self.set_str(target, val)
    }

    pub fn source_file(&mut self, path: &str, args: &[String]) -> R {
        let src = match std::fs::read(path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                sherr!("{}: {}", path, sys::strerror(e.raw_os_error().unwrap_or(0)));
                return Ok(1);
            }
        };
        let saved = if args.is_empty() { None } else { Some(std::mem::replace(&mut self.pos_params, args.to_vec())) };
        let saved_name = std::mem::replace(&mut self.script_name, path.to_string());
        self.source_depth += 1;
        let r = self.run_source(&src, path);
        self.source_depth -= 1;
        self.script_name = saved_name;
        if let Some(p) = saved {
            self.pos_params = p;
        }
        match r {
            Err(Flow::Return(s)) => Ok(s),
            other => other,
        }
    }

    fn tilde_path(&mut self, p: &str) -> String {
        let home = self.home();
        if home.len() > 1 && (p == home || p.starts_with(&format!("{}/", home))) {
            format!("~{}", &p[home.len()..])
        } else {
            p.to_string()
        }
    }

    fn print_dirs(&mut self) {
        let mut v = vec![self.cwd()];
        v.extend(self.dir_stack.iter().rev().cloned());
        let v: Vec<String> = v.iter().map(|d| self.tilde_path(d)).collect();
        out!("{}\n", v.join(" "));
    }

    fn builtin_cd(&mut self, args: &[String]) -> R {
        let mut physical = false;
        let mut rest: Vec<&String> = Vec::new();
        for a in args {
            match a.as_str() {
                "-P" => physical = true,
                "-L" | "--" => {}
                _ => rest.push(a),
            }
        }
        if rest.len() > 1 {
            sherr!("cd: too many arguments");
            return Ok(1);
        }
        let old = self.cwd();
        let mut print = false;
        let target = match rest.first().map(|s| s.as_str()) {
            None => self.home(),
            Some("-") => match self.get("OLDPWD") {
                Some(o) => {
                    print = true;
                    o
                }
                None => {
                    sherr!("cd: OLDPWD not set");
                    return Ok(1);
                }
            },
            Some(t) => t.to_string(),
        };
        let mut candidates = vec![target.clone()];
        if !target.starts_with('/') && !target.starts_with('.') {
            if let Some(cdpath) = self.get("CDPATH") {
                for d in cdpath.split(':').filter(|d| !d.is_empty()) {
                    candidates.push(format!("{}/{}", d, target));
                    print = true;
                }
            }
        }
        let mut last_err = String::new();
        for (ci, cand) in candidates.iter().enumerate() {
            let logical = if cand.starts_with('/') { normalize_path(cand) } else { normalize_path(&format!("{}/{}", old, cand)) };
            let ok = if !physical && std::env::set_current_dir(&logical).is_ok() {
                Some(logical)
            } else {
                match std::env::set_current_dir(cand) {
                    Ok(()) => Some(std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| cand.clone())),
                    Err(e) => {
                        if ci == 0 {
                            last_err = sys::strerror(e.raw_os_error().unwrap_or(0));
                        }
                        None
                    }
                }
            };
            if let Some(mut newpwd) = ok {
                if physical {
                    newpwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or(newpwd);
                }
                let _ = self.set_str("OLDPWD", old);
                let _ = self.set_str("PWD", newpwd.clone());
                if print && (ci > 0 || rest.first().map(|s| s.as_str()) == Some("-")) {
                    out!("{}\n", newpwd);
                }
                if self.interactive {
                    self.z_add(&newpwd);
                    self.save_last_dir();
                    if self.find_command("zoxide").is_some() {
                        let _ = std::process::Command::new("zoxide").arg("add").arg(&newpwd).status();
                    }
                }
                if let Some(f) = self.funcs.get("chpwd").cloned() {
                    let st = self.last_status;
                    self.call_function(&f, &[])?;
                    self.last_status = st;
                }
                return Ok(0);
            }
        }
        sherr!("cd: {}: {}", target, last_err);
        Ok(1)
    }

    fn builtin_set(&mut self, args: &[String]) -> R {
        if args.is_empty() {
            for (n, v) in self.all_vars() {
                match &v.val {
                    Val::Str(s) => out!("{}={}\n", n, shell_quote(s)),
                    Val::Arr(a) => out!("{}=({})\n", n, a.iter().map(|s| shell_quote(s)).collect::<Vec<_>>().join(" ")),
                    Val::Map(m) => out!("{}=({})\n", n, m.iter().map(|(k, s)| format!("[{}]={}", shell_quote(k), shell_quote(s))).collect::<Vec<_>>().join(" ")),
                    Val::Unset => true,
                };
            }
            return Ok(0);
        }
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--" {
                self.pos_params = args[i + 1..].to_vec();
                return Ok(0);
            }
            if a == "-" {
                self.opts.xtrace = false;
                self.opts.verbose = false;
                self.pos_params = args[i + 1..].to_vec();
                return Ok(0);
            }
            let on = if a.starts_with('-') {
                true
            } else if a.starts_with('+') {
                false
            } else {
                self.pos_params = args[i..].to_vec();
                return Ok(0);
            };
            if &a[1..] == "o" {
                i += 1;
                match args.get(i) {
                    Some(n) => {
                        if !self.opts.set(n, on) {
                            sherr!("set: {}: invalid option name", n);
                            return Ok(2);
                        }
                    }
                    None => {
                        for n in OPTION_NAMES {
                            let v = self.opts.get(n).unwrap_or(false);
                            if on {
                                out!("{:<16}{}\n", n, if v { "on" } else { "off" });
                            } else {
                                out!("set {}o {}\n", if v { '-' } else { '+' }, n);
                            }
                        }
                    }
                }
            } else {
                for c in a[1..].chars() {
                    match c {
                        'e' => self.opts.errexit = on,
                        'u' => self.opts.nounset = on,
                        'x' => self.opts.xtrace = on,
                        'f' => self.opts.noglob = on,
                        'C' => self.opts.noclobber = on,
                        'a' => self.opts.allexport = on,
                        'v' => self.opts.verbose = on,
                        'h' | 'b' | 'm' | 'B' | 'H' | 'E' | 'T' | 'P' => {}
                        _ => {
                            sherr!("set: -{}: invalid option", c);
                            return Ok(2);
                        }
                    }
                }
            }
            i += 1;
        }
        Ok(0)
    }

    fn builtin_type(&mut self, cmd: &str, args: &[String]) -> R {
        let mut mode = cmd.to_string();
        let mut names: Vec<String> = Vec::new();
        let mut i = 0;
        if cmd == "command" {
            let mut v = false;
            while i < args.len() && args[i].starts_with('-') && args[i].len() > 1 {
                match args[i].as_str() {
                    "-v" => {
                        v = true;
                        mode = "command-v".into();
                    }
                    "-V" => {
                        v = true;
                        mode = "type".into();
                    }
                    "-p" => {}
                    "--" => {
                        i += 1;
                        break;
                    }
                    _ => break,
                }
                i += 1;
            }
            if !v {
                return self.run_argv(args[i..].to_vec(), true);
            }
        }
        while i < args.len() {
            match args[i].as_str() {
                "-t" if cmd == "type" => mode = "type-t".into(),
                "-p" | "-P" if cmd == "type" => mode = "type-p".into(),
                "-a" | "-f" | "-s" | "--" => {}
                a => names.push(a.to_string()),
            }
            i += 1;
        }
        let mut st = 0;
        for n in names {
            let alias = self.aliases.get(&n).cloned();
            let keyword = crate::parser::RESERVED.contains(&n.as_str());
            let func = self.funcs.get(&n).cloned();
            let builtin = is_builtin(&n);
            let path = self.find_command(&n).filter(|p| sys::is_executable(p));
            let line = match mode.as_str() {
                "type-t" => {
                    if alias.is_some() {
                        Some("alias".to_string())
                    } else if keyword {
                        Some("keyword".into())
                    } else if func.is_some() {
                        Some("function".into())
                    } else if builtin {
                        Some("builtin".into())
                    } else if path.is_some() {
                        Some("file".into())
                    } else {
                        None
                    }
                }
                "type-p" => path.clone(),
                "command-v" => {
                    if let Some(a) = &alias {
                        Some(format!("alias {}={}", n, shell_quote(a)))
                    } else if keyword || func.is_some() || builtin {
                        Some(n.clone())
                    } else {
                        path.clone()
                    }
                }
                "which" => {
                    if let Some(a) = &alias {
                        Some(format!("{}: aliased to {}", n, a))
                    } else if let Some(f) = &func {
                        Some(f.text.clone())
                    } else if builtin {
                        Some(format!("{}: shell built-in command", n))
                    } else if keyword {
                        Some(format!("{}: shell reserved word", n))
                    } else {
                        path.clone()
                    }
                }
                _ => {
                    if let Some(a) = &alias {
                        Some(format!("{} is aliased to `{}'", n, a))
                    } else if keyword {
                        Some(format!("{} is a shell keyword", n))
                    } else if let Some(f) = &func {
                        Some(format!("{} is a function\n{}", n, f.text))
                    } else if builtin {
                        Some(format!("{} is a shell builtin", n))
                    } else {
                        path.as_ref().map(|p| format!("{} is {}", n, p))
                    }
                }
            };
            match line {
                Some(l) => {
                    out!("{}\n", l);
                }
                None => {
                    st = 1;
                    match mode.as_str() {
                        "type" => sherr!("type: {}: not found", n),
                        "which" => {
                            out!("{} not found\n", n);
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(st)
    }

    fn builtin_trap(&mut self, args: &[String]) -> R {
        let mut args = args;
        if !args.is_empty() && args[0] == "--" {
            args = &args[1..];
        }
        if args.is_empty() || args[0] == "-p" {
            let mut v: Vec<_> = self.traps.iter().collect();
            v.sort();
            for (sig, cmd) in v {
                let n = match *sig {
                    0 => "EXIT".to_string(),
                    ERR_TRAP => "ERR".to_string(),
                    s => format!("SIG{}", sys::sig_name(s)),
                };
                out!("trap -- {} {}\n", shell_quote(cmd), n);
            }
            return Ok(0);
        }
        if args[0] == "-l" {
            let v: Vec<String> = sys::SIGNALS.iter().map(|(n, k)| format!("{}) SIG{}", k, n)).collect();
            out!("{}\n", v.join("\n"));
            return Ok(0);
        }
        let (action, sigs): (Option<&str>, &[String]) = if args.len() == 1 || args[0] == "-" || args[0].parse::<u32>().is_ok() {
            (None, if args[0] == "-" { &args[1..] } else { args })
        } else {
            (Some(args[0].as_str()), &args[1..])
        };
        let mut st = 0;
        for s in sigs {
            let sig = if s.eq_ignore_ascii_case("ERR") {
                ERR_TRAP
            } else {
                match sys::sig_number(s) {
                    Some(n) => n,
                    None => {
                        sherr!("trap: {}: invalid signal specification", s);
                        st = 1;
                        continue;
                    }
                }
            };
            match action {
                Some(a) => {
                    self.traps.insert(sig, a.to_string());
                    if sig > 0 && sig != libc::SIGKILL && sig != libc::SIGSTOP {
                        if a.is_empty() {
                            sys::ignore_signal(sig);
                        } else {
                            sys::catch_signal(sig);
                        }
                    }
                }
                None => {
                    self.traps.remove(&sig);
                    if sig > 0 {
                        if self.interactive && sig == libc::SIGINT {
                            sys::catch_signal(sig);
                        } else if self.interactive && [libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU, libc::SIGQUIT, libc::SIGTERM].contains(&sig) {
                            sys::ignore_signal(sig);
                        } else {
                            sys::default_signal(sig);
                        }
                    }
                }
            }
        }
        Ok(st)
    }

    fn builtin_read(&mut self, args: &[String]) -> R {
        let mut raw = false;
        let mut silent = false;
        let mut prompt = None;
        let mut array = None;
        let mut nchars: Option<usize> = None;
        let mut delim = '\n';
        let mut timeout: Option<f64> = None;
        let mut fd = 0;
        let mut names: Vec<String> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--" {
                names.extend(args[i + 1..].iter().cloned());
                break;
            }
            if a.starts_with('-') && a.len() > 1 && names.is_empty() {
                let cs: Vec<char> = a[1..].chars().collect();
                let mut k = 0;
                while k < cs.len() {
                    let c = cs[k];
                    let takes = matches!(c, 'p' | 'a' | 'n' | 'N' | 'd' | 't' | 'u');
                    let mut val = String::new();
                    if takes {
                        val = cs[k + 1..].iter().collect();
                        if val.is_empty() {
                            i += 1;
                            val = args.get(i).cloned().unwrap_or_default();
                        }
                        k = cs.len();
                    }
                    match c {
                        'r' => raw = true,
                        's' => silent = true,
                        'e' => {}
                        'p' => prompt = Some(val),
                        'a' => array = Some(val),
                        'n' | 'N' => nchars = val.parse().ok(),
                        'd' => delim = val.chars().next().unwrap_or('\0'),
                        't' => timeout = val.parse().ok(),
                        'u' => fd = val.parse().unwrap_or(0),
                        _ => {
                            sherr!("read: -{}: invalid option", c);
                            return Ok(2);
                        }
                    }
                    k += 1;
                }
            } else {
                names.push(a.clone());
            }
            i += 1;
        }
        let tty = sys::isatty(fd);
        if let Some(p) = &prompt {
            if tty {
                err!("{}", p);
            }
        }
        let mut old_term: Option<libc::termios> = None;
        if tty && (silent || nchars.is_some() || delim != '\n') {
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut t) == 0 {
                    old_term = Some(t);
                    if silent {
                        t.c_lflag &= !libc::ECHO;
                    }
                    if nchars.is_some() || delim != '\n' {
                        t.c_lflag &= !libc::ICANON;
                        t.c_cc[libc::VMIN] = 1;
                        t.c_cc[libc::VTIME] = 0;
                    }
                    libc::tcsetattr(fd, libc::TCSANOW, &t);
                }
            }
        }
        let deadline = timeout.map(|t| std::time::Instant::now() + std::time::Duration::from_secs_f64(t));
        let mut bytes: Vec<u8> = Vec::new();
        let mut escaped: Vec<usize> = Vec::new(); // byte offsets of escaped chars
        let mut status = 0;
        let mut flow = None;
        let mut count = 0;
        let mut pending_bs = false;
        loop {
            if let Some(n) = nchars {
                if count >= n {
                    break;
                }
            }
            if let Some(d) = deadline {
                let now = std::time::Instant::now();
                let ms = if d > now { (d - now).as_millis() as i32 } else { 0 };
                let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                let r = unsafe { libc::poll(&mut pfd, 1, ms) };
                if r == 0 {
                    status = 142;
                    break;
                }
            }
            match sys::read_byte(fd) {
                Ok(Some(b)) => {
                    if pending_bs {
                        pending_bs = false;
                        if b == b'\n' {
                            continue;
                        }
                        escaped.push(bytes.len());
                        bytes.push(b);
                        if b & 0xC0 != 0x80 {
                            count += 1;
                        }
                        continue;
                    }
                    if b == b'\\' && !raw {
                        pending_bs = true;
                        continue;
                    }
                    if (b as char) == delim && b < 0x80 {
                        break;
                    }
                    bytes.push(b);
                    if b & 0xC0 != 0x80 {
                        count += 1;
                    }
                }
                Ok(None) => {
                    status = 1;
                    break;
                }
                Err(e) => {
                    if e == libc::EINTR {
                        if let Err(f) = self.check_signals() {
                            flow = Some(f);
                            break;
                        }
                        continue;
                    }
                    status = 1;
                    break;
                }
            }
        }
        if let Some(t) = old_term {
            unsafe {
                libc::tcsetattr(fd, libc::TCSANOW, &t);
            }
            if silent && tty {
                err!("\n");
            }
        }
        if let Some(f) = flow {
            return Err(f);
        }
        // decode to chars with escape flags
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut chars: Vec<(char, bool)> = Vec::new();
        let mut off = 0;
        for c in text.chars() {
            chars.push((c, escaped.contains(&off)));
            off += c.len_utf8();
        }
        let ifs: Vec<char> = self.ifs().chars().collect();
        let is_ws = |c: char| ifs.contains(&c) && (c == ' ' || c == '\t' || c == '\n');
        let is_sep = |(c, e): (char, bool)| !e && ifs.contains(&c);
        let collect = |s: &[(char, bool)]| s.iter().map(|(c, _)| *c).collect::<String>();
        let mut r = Ok(());
        if let Some(arr) = array {
            let mut fields = Vec::new();
            let mut pos = 0;
            while pos < chars.len() && is_sep(chars[pos]) && is_ws(chars[pos].0) {
                pos += 1;
            }
            while pos < chars.len() {
                let st = pos;
                while pos < chars.len() && !is_sep(chars[pos]) {
                    pos += 1;
                }
                fields.push(collect(&chars[st..pos]));
                while pos < chars.len() && is_sep(chars[pos]) && is_ws(chars[pos].0) {
                    pos += 1;
                }
                if pos < chars.len() && is_sep(chars[pos]) {
                    pos += 1;
                    while pos < chars.len() && is_sep(chars[pos]) && is_ws(chars[pos].0) {
                        pos += 1;
                    }
                }
            }
            r = self.set(&arr, Val::Arr(fields));
        } else if names.is_empty() {
            r = self.set_str("REPLY", collect(&chars));
        } else {
            let mut pos = 0;
            while pos < chars.len() && is_sep(chars[pos]) && is_ws(chars[pos].0) {
                pos += 1;
            }
            for (k, n) in names.iter().enumerate() {
                let val = if k == names.len() - 1 {
                    let mut end = chars.len();
                    while end > pos && is_sep(chars[end - 1]) && is_ws(chars[end - 1].0) {
                        end -= 1;
                    }
                    collect(&chars[pos..end])
                } else {
                    let st = pos;
                    while pos < chars.len() && !is_sep(chars[pos]) {
                        pos += 1;
                    }
                    let v = collect(&chars[st..pos]);
                    while pos < chars.len() && is_sep(chars[pos]) && is_ws(chars[pos].0) {
                        pos += 1;
                    }
                    if pos < chars.len() && is_sep(chars[pos]) && !is_ws(chars[pos].0) {
                        pos += 1;
                        while pos < chars.len() && is_sep(chars[pos]) && is_ws(chars[pos].0) {
                            pos += 1;
                        }
                    }
                    v
                };
                if let Err(e) = self.assign_ref(n, val) {
                    r = Err(e);
                    break;
                }
            }
        }
        if let Err(e) = r {
            sherr!("read: {}", e);
            return Ok(1);
        }
        Ok(status)
    }

    fn builtin_getopts(&mut self, args: &[String]) -> R {
        if args.len() < 2 {
            sherr!("getopts: usage: getopts optstring name [arg ...]");
            return Ok(2);
        }
        let optstring: Vec<char> = args[0].chars().collect();
        let silent = optstring.first() == Some(&':');
        let name = args[1].clone();
        let params: Vec<String> = if args.len() > 2 { args[2..].to_vec() } else { self.pos_params.clone() };
        let mut optind = self.get("OPTIND").and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).max(1);
        if optind == 1 && self.getopts_off > 0 && params.first().map(|p| p.chars().count() <= self.getopts_off).unwrap_or(true) {
            self.getopts_off = 0;
        }
        let finish = |sh: &mut Shell, optind: usize| {
            let _ = sh.set_str(&name, "?");
            let _ = sh.set_str("OPTIND", optind.to_string());
            sh.getopts_off = 0;
            Ok(1)
        };
        if optind > params.len() {
            return finish(self, optind);
        }
        let cur: Vec<char> = params[optind - 1].chars().collect();
        if self.getopts_off == 0 {
            if cur.len() < 2 || cur[0] != '-' {
                return finish(self, optind);
            }
            if cur.len() == 2 && cur[1] == '-' {
                return finish(self, optind + 1);
            }
            self.getopts_off = 1;
        }
        let ch = cur[self.getopts_off];
        self.getopts_off += 1;
        let at_end = self.getopts_off >= cur.len();
        let pos = optstring.iter().position(|c| *c == ch && ch != ':');
        let mut result = ch.to_string();
        let _ = self.unset_var("OPTARG");
        match pos {
            None => {
                result = "?".into();
                if silent {
                    let _ = self.set_str("OPTARG", ch.to_string());
                } else {
                    sherr!("{}: illegal option -- {}", self.arg0, ch);
                }
                if at_end {
                    optind += 1;
                    self.getopts_off = 0;
                }
            }
            Some(p) => {
                if optstring.get(p + 1) == Some(&':') {
                    if !at_end {
                        let _ = self.set_str("OPTARG", cur[self.getopts_off..].iter().collect::<String>());
                        optind += 1;
                    } else if optind < params.len() {
                        let _ = self.set_str("OPTARG", params[optind].clone());
                        optind += 2;
                    } else {
                        optind += 1;
                        if silent {
                            result = ":".into();
                            let _ = self.set_str("OPTARG", ch.to_string());
                        } else {
                            result = "?".into();
                            sherr!("{}: option requires an argument -- {}", self.arg0, ch);
                        }
                    }
                    self.getopts_off = 0;
                } else if at_end {
                    optind += 1;
                    self.getopts_off = 0;
                }
            }
        }
        let _ = self.set_str(&name, result);
        let _ = self.set_str("OPTIND", optind.to_string());
        Ok(0)
    }

    fn find_job(&self, spec: Option<&str>) -> Option<usize> {
        if self.jobs.is_empty() {
            return None;
        }
        let spec = match spec {
            None | Some("%%") | Some("%+") | Some("%") => return Some(self.jobs.len() - 1),
            Some("%-") => return if self.jobs.len() >= 2 { Some(self.jobs.len() - 2) } else { Some(0) },
            Some(s) => s,
        };
        let body = spec.strip_prefix('%').unwrap_or(spec);
        if let Ok(n) = body.parse::<usize>() {
            if spec.starts_with('%') {
                return self.jobs.iter().position(|j| j.id == n);
            }
            return self.jobs.iter().position(|j| j.procs.iter().any(|p| p.pid == n as i32)).or_else(|| self.jobs.iter().position(|j| j.id == n));
        }
        if let Some(sub) = body.strip_prefix('?') {
            return self.jobs.iter().position(|j| j.text.contains(sub));
        }
        self.jobs.iter().position(|j| j.text.starts_with(body))
    }

    fn wait_job_blocking(&mut self, idx: usize) -> Result<i32, Flow> {
        let pids: Vec<i32> = self.jobs[idx].procs.iter().filter(|p| p.status.is_none()).map(|p| p.pid).collect();
        for pid in pids {
            loop {
                let mut st = 0;
                let r = unsafe { libc::waitpid(pid, &mut st, 0) };
                if r < 0 {
                    if sys::errno() == libc::EINTR {
                        self.check_signals()?;
                        if self.traps.keys().any(|k| *k > 0) {
                            return Ok(129);
                        }
                        continue;
                    }
                    break;
                }
                let code = match sys::decode_status(st) {
                    sys::WaitStatus::Exited(c) => c,
                    sys::WaitStatus::Signaled(s) => 128 + s,
                    _ => continue,
                };
                for p in &mut self.jobs[idx].procs {
                    if p.pid == pid {
                        p.status = Some(code);
                    }
                }
                break;
            }
        }
        for p in &mut self.jobs[idx].procs {
            if p.status.is_none() {
                p.status = Some(127);
            }
        }
        Ok(self.jobs[idx].status())
    }

    fn builtin_wait(&mut self, args: &[String]) -> R {
        if args.is_empty() {
            for i in 0..self.jobs.len() {
                let st = self.wait_job_blocking(i)?;
                if st == 129 && !self.jobs[i].done() {
                    return Ok(st);
                }
            }
            self.jobs.clear();
            return Ok(0);
        }
        let mut st = 0;
        for a in args {
            match self.find_job(Some(a)) {
                Some(i) => {
                    st = self.wait_job_blocking(i)?;
                    if self.jobs[i].done() {
                        self.jobs.remove(i);
                    }
                }
                None => {
                    st = match a.parse::<i32>() {
                        Ok(pid) => self.wait_pid(pid),
                        Err(_) => {
                            sherr!("wait: {}: no such job", a);
                            127
                        }
                    }
                }
            }
        }
        Ok(st)
    }

    fn builtin_kill(&mut self, args: &[String]) -> R {
        let mut sig = libc::SIGTERM;
        let mut i = 0;
        if let Some(a) = args.first() {
            if a == "-l" || a == "-L" {
                match args.get(1) {
                    Some(n) => {
                        let n: i32 = n.parse().unwrap_or(0);
                        out!("{}\n", sys::sig_name(if n > 128 { n - 128 } else { n }));
                    }
                    None => {
                        let v: Vec<String> = sys::SIGNALS.iter().map(|(n, _)| n.to_string()).collect();
                        out!("{}\n", v.join(" "));
                    }
                }
                return Ok(0);
            }
            if a == "-s" || a == "-n" {
                match args.get(1).and_then(|s| sys::sig_number(s)) {
                    Some(s) => sig = s,
                    None => {
                        sherr!("kill: {}: invalid signal specification", args.get(1).cloned().unwrap_or_default());
                        return Ok(1);
                    }
                }
                i = 2;
            } else if a == "--" {
                i = 1;
            } else if a.starts_with('-') && a.len() > 1 {
                match sys::sig_number(&a[1..]) {
                    Some(s) => sig = s,
                    None => {
                        sherr!("kill: {}: invalid signal specification", &a[1..]);
                        return Ok(1);
                    }
                }
                i = 1;
            }
        }
        if i >= args.len() {
            sherr!("kill: usage: kill [-s sigspec | -sigspec] pid | %job ...");
            return Ok(2);
        }
        let mut st = 0;
        for a in &args[i..] {
            let target = if a.starts_with('%') {
                match self.find_job(Some(a)) {
                    Some(j) => {
                        let job = &self.jobs[j];
                        if job.pgid != 0 {
                            -job.pgid
                        } else {
                            job.procs[0].pid
                        }
                    }
                    None => {
                        sherr!("kill: {}: no such job", a);
                        st = 1;
                        continue;
                    }
                }
            } else {
                match a.parse::<i32>() {
                    Ok(p) => p,
                    Err(_) => {
                        sherr!("kill: {}: arguments must be process or job IDs", a);
                        st = 1;
                        continue;
                    }
                }
            };
            unsafe {
                if libc::kill(target, sig) != 0 {
                    sherr!("kill: ({}) - {}", a, sys::last_error());
                    st = 1;
                } else if a.starts_with('%') && sig != libc::SIGCONT && sig != libc::SIGSTOP {
                    libc::kill(target, libc::SIGCONT);
                }
            }
        }
        Ok(st)
    }

    fn builtin_history(&mut self, args: &[String]) -> R {
        let mut here = false;
        let mut failed = false;
        let mut long = false;
        let mut limit: Option<usize> = None;
        let mut pattern: Option<String> = None;
        for a in args {
            match a.as_str() {
                "-c" | "--clear" => {
                    self.history.clear();
                    return Ok(0);
                }
                "--here" => here = true,
                "--failed" => failed = true,
                "--long" | "-l" => long = true,
                a => match a.parse::<usize>() {
                    Ok(n) => limit = Some(n),
                    Err(_) => pattern = Some(a.to_string()),
                },
            }
        }
        let cwd = self.cwd();
        let rows: Vec<(usize, &crate::history::Entry)> = self.history.entries.iter().enumerate().filter(|(_, e)| (!here || e.cwd == cwd) && (!failed || e.status != 0) && pattern.as_ref().map(|p| e.cmd.contains(p.as_str())).unwrap_or(true)).collect();
        let skip = limit.map(|l| rows.len().saturating_sub(l)).unwrap_or(0);
        let mut outbuf = String::new();
        for (i, e) in &rows[skip..] {
            let cmd = e.cmd.replace('\n', "\n        ");
            if long {
                outbuf.push_str(&format!("{:5}  {}  {:>7}  {:>3}  {}  {}\n", i + 1, sys::strftime("%Y-%m-%d %H:%M:%S", e.ts), fmt_duration(e.dur_ms), e.status, e.cwd, cmd));
            } else {
                outbuf.push_str(&format!("{:5}  {}\n", i + 1, cmd));
            }
        }
        sys::write_fd(1, outbuf.as_bytes());
        Ok(0)
    }

    // ---------- z: frecency directory jumping ----------

    pub fn data_dir(&mut self) -> String {
        let dir = match self.get("NASH_DATA_DIR") {
            Some(d) => d,
            None => match self.get("XDG_DATA_HOME") {
                Some(x) if !x.is_empty() => format!("{}/nash", x),
                _ => format!("{}/.local/share/nash", self.home()),
            },
        };
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn z_load(&mut self) -> Vec<(String, f64, i64)> {
        let path = format!("{}/z", self.data_dir());
        let mut v = Vec::new();
        if let Ok(s) = std::fs::read_to_string(path) {
            for line in s.lines() {
                let mut it = line.splitn(3, '\t');
                if let (Some(score), Some(last), Some(p)) = (it.next(), it.next(), it.next()) {
                    if let (Ok(score), Ok(last)) = (score.parse::<f64>(), last.parse::<i64>()) {
                        v.push((p.to_string(), score, last));
                    }
                }
            }
        }
        v
    }

    fn z_save(&mut self, mut v: Vec<(String, f64, i64)>) {
        let total: f64 = v.iter().map(|e| e.1).sum();
        if total > 5000.0 {
            for e in &mut v {
                e.1 *= 0.9;
            }
            v.retain(|e| e.1 >= 1.0);
        }
        let path = format!("{}/z", self.data_dir());
        let body: String = v.iter().map(|(p, s, l)| format!("{:.2}\t{}\t{}\n", s, l, p)).collect();
        let tmp = format!("{}.{}", path, unsafe { libc::getpid() });
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Remember the current directory so the next session can start there.
    pub fn save_last_dir(&mut self) {
        let cwd = self.cwd();
        let _ = std::fs::write(format!("{}/last_dir", self.data_dir()), format!("{}\n", cwd));
    }

    pub fn z_add(&mut self, dir: &str) {
        if dir == self.home() || dir == "/" {
            return;
        }
        let mut v = self.z_load();
        let now = sys::now_secs();
        match v.iter_mut().find(|e| e.0 == dir) {
            Some(e) => {
                e.1 += 1.0;
                e.2 = now;
            }
            None => v.push((dir.to_string(), 1.0, now)),
        }
        self.z_save(v);
    }

    fn builtin_z(&mut self, args: &[String]) -> R {
        // Prefer zoxide when available.
        if self.find_command("zoxide").is_some() {
            if args.is_empty() {
                let home = self.home();
                return self.builtin_cd(&[home]);
            }
            if args.len() == 1 && args[0] == "-" {
                return self.builtin_cd(&["-".to_string()]);
            }
            if args.len() == 1 && (args[0].starts_with('/') || args[0].starts_with('.') || sys::is_dir(&args[0])) {
                return self.builtin_cd(&[args[0].clone()]);
            }
            let cwd = self.cwd();
            let mut cmd = std::process::Command::new("zoxide");
            cmd.arg("query").arg("--exclude").arg(&cwd);
            cmd.args(args);
            match cmd.output() {
                Ok(o) if o.status.success() => {
                    let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if path.is_empty() {
                        sherr!("z: no match");
                        return Ok(1);
                    }
                    let r = self.builtin_cd(&[path.clone()])?;
                    if r == 0 && self.get("_ZO_ECHO").as_deref() == Some("1") {
                        err!("{}\n", path);
                    }
                    return Ok(r);
                }
                _ => {
                    sherr!("z: no match");
                    return Ok(1);
                }
            }
        }
        let now = sys::now_secs();
        let frecency = |score: f64, last: i64| {
            let age = now - last;
            score * if age < 3600 {
                4.0
            } else if age < 86400 {
                2.0
            } else if age < 604800 {
                0.5
            } else {
                0.25
            }
        };
        let mut list = false;
        let mut terms: Vec<String> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-l" => list = true,
                "-x" => {
                    let target = args.get(i + 1).cloned().unwrap_or_else(|| self.cwd());
                    let mut v = self.z_load();
                    v.retain(|e| e.0 != target);
                    self.z_save(v);
                    return Ok(0);
                }
                "-h" | "--help" => {
                    out!("usage: z [-l] [terms...]   jump to the most `frecent' directory matching all terms\n       z -x [dir]          forget a directory\n");
                    return Ok(0);
                }
                a => terms.push(a.to_lowercase()),
            }
            i += 1;
        }
        let cwd = self.cwd();
        let mut v = self.z_load();
        v.sort_by(|a, b| frecency(b.1, b.2).partial_cmp(&frecency(a.1, a.2)).unwrap_or(std::cmp::Ordering::Equal));
        if terms.is_empty() || list {
            for (p, s, l) in v.iter().rev() {
                let lp = p.to_lowercase();
                if terms.iter().all(|t| lp.contains(t.as_str())) {
                    out!("{:>8.1}  {}\n", frecency(*s, *l), p);
                }
            }
            return Ok(0);
        }
        if terms.len() == 1 && sys::is_dir(&args[args.len() - 1]) {
            return self.builtin_cd(&[args[args.len() - 1].clone()]);
        }
        let matches_in_order = |p: &str| {
            let lp = p.to_lowercase();
            let mut from = 0;
            for t in &terms {
                match lp[from..].find(t.as_str()) {
                    Some(k) => from += k + t.len(),
                    None => return false,
                }
            }
            true
        };
        let last_comp = |p: &str| {
            let base = p.rsplit('/').next().unwrap_or("").to_lowercase();
            base.contains(terms.last().unwrap().as_str())
        };
        let pick = v.iter().find(|e| e.0 != cwd && matches_in_order(&e.0) && last_comp(&e.0) && sys::is_dir(&e.0)).or_else(|| v.iter().find(|e| e.0 != cwd && matches_in_order(&e.0) && sys::is_dir(&e.0))).map(|e| e.0.clone());
        match pick {
            Some(p) => {
                let r = self.builtin_cd(&[p.clone()])?;
                if r == 0 {
                    let shown = self.tilde_path(&p);
                    err!("{}\n", shown);
                }
                Ok(r)
            }
            None => {
                sherr!("z: no match for `{}'", terms.join(" "));
                Ok(1)
            }
        }
    }

    fn builtin_path(&mut self, args: &[String]) -> R {
        if args.is_empty() {
            if !sys::isatty(0) {
                let data = sys::read_all(0);
                let s = String::from_utf8_lossy(&data);
                for line in s.lines() {
                    out!("{}\n", line);
                }
            }
            return Ok(0);
        }
        let sub = args[0].as_str();
        match sub {
            "basename" | "dirname" | "extension" | "normalize" | "realpath" => {
                let rest = &args[1..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for p in inputs {
                    match sub {
                        "basename" => {
                            out!("{}\n", p.rsplit('/').next().unwrap_or("").trim_end_matches('/'));
                        }
                        "dirname" => {
                            let t = p.trim_end_matches('/');
                            match t.rfind('/') {
                                Some(i) if i > 0 => { out!("{}\n", &t[..i]); }
                                _ => { out!(".\n"); }
                            }
                        }
                        "extension" => {
                            let base = p.rsplit('/').next().unwrap_or("");
                            match base.rfind('.') {
                                Some(i) if i > 0 => { out!("{}\n", &base[i..]); }
                                _ => { out!("\n"); }
                            }
                        }
                        "normalize" | "realpath" => {
                            let base = if p.starts_with('/') { String::new() } else { self.cwd() };
                            let mut comps: Vec<String> = Vec::new();
                            for part in format!("{}/{}", base, p).split('/') {
                                match part {
                                    "" | "." => {}
                                    ".." => { comps.pop(); }
                                    c => comps.push(c.to_string()),
                                }
                            }
                            out!("/{}\n", comps.join("/"));
                        }
                        _ => {}
                    }
                }
                Ok(0)
            }
            "change-extension" | "change_extension" => {
                let ext = args.get(1).cloned().unwrap_or_default();
                let rest = if args.len() > 2 { &args[2..] } else { &[] as &[String] };
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for p in inputs {
                    let (dir, base) = match p.rfind('/') {
                        Some(i) => (&p[..=i], &p[i + 1..]),
                        None => ("", p.as_str()),
                    };
                    let stem = match base.rfind('.') {
                        Some(i) if i > 0 => &base[..i],
                        _ => base,
                    };
                    let newbase = if ext.is_empty() { stem.to_string() } else { format!("{}.{}", stem, ext.trim_start_matches('.')) };
                    out!("{}{}\n", dir, newbase);
                }
                Ok(0)
            }
            "is-dir" | "is_dir" => {
                let rest = &args[1..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for p in inputs {
                    if sys::is_dir(&p) { return Ok(0); }
                }
                Ok(1)
            }
            "filter" => {
                let rest = &args[1..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for p in inputs {
                    out!("{}\n", p);
                }
                Ok(0)
            }
            other => {
                // treat whole args as paths to print (fish `path` with no subcommand)
                for p in args {
                    out!("{}\n", p);
                }
                let _ = other;
                Ok(0)
            }
        }
    }

    fn builtin_string(&mut self, args: &[String]) -> R {
        if args.is_empty() {
            if !sys::isatty(0) {
                let data = sys::read_all(0);
                sys::write_fd(1, &data);
            }
            return Ok(0);
        }
        let sub = args[0].as_str();
        match sub {
            "length" => {
                let rest = &args[1..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for s in inputs {
                    out!("{}\n", s.chars().count());
                }
                Ok(0)
            }
            "upper" => {
                let rest = &args[1..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for s in inputs {
                    out!("{}\n", s.to_uppercase());
                }
                Ok(0)
            }
            "lower" => {
                let rest = &args[1..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for s in inputs {
                    out!("{}\n", s.to_lowercase());
                }
                Ok(0)
            }
            "trim" => {
                let mut left = true;
                let mut right = true;
                let mut chars: Option<String> = None;
                let mut i = 1;
                while i < args.len() && args[i].starts_with('-') {
                    match args[i].as_str() {
                        "-l" => { right = false; }
                        "-r" => { left = false; }
                        "-c" => {
                            i += 1;
                            chars = args.get(i).cloned();
                        }
                        "--" => { i += 1; break; }
                        _ => {}
                    }
                    i += 1;
                }
                let rest = &args[i..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for s in inputs {
                    let mut t = s.clone();
                    let trimset = chars.clone().unwrap_or_else(|| " \t".to_string());
                    if left {
                        t = t.trim_start_matches(|c: char| trimset.contains(c)).to_string();
                    }
                    if right {
                        t = t.trim_end_matches(|c: char| trimset.contains(c)).to_string();
                    }
                    out!("{}\n", t);
                }
                Ok(0)
            }
            "replace" => {
                let mut all = false;
                let mut i = 1;
                while i < args.len() && args[i].starts_with('-') {
                    if args[i] == "-a" || args[i] == "--all" { all = true; }
                    if args[i] == "--" { i += 1; break; }
                    i += 1;
                }
                if i + 2 > args.len() {
                    sherr!("string replace: usage: string replace [-a] pattern replacement [strings...]");
                    return Ok(2);
                }
                let pat = &args[i];
                let rep = &args[i + 1];
                let rest = &args[i + 2..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for s in inputs {
                    let outstr = if all {
                        s.replace(pat, rep)
                    } else {
                        // replace first occurrence
                        if let Some(pos) = s.find(pat) {
                            let mut r = String::new();
                            r.push_str(&s[..pos]);
                            r.push_str(rep);
                            r.push_str(&s[pos + pat.len()..]);
                            r
                        } else {
                            s.clone()
                        }
                    };
                    out!("{}\n", outstr);
                }
                Ok(0)
            }
            "split" => {
                let mut max: Option<usize> = None;
                let mut i = 1;
                while i < args.len() && args[i].starts_with('-') {
                    if args[i] == "-n" || args[i] == "--max" {
                        i += 1;
                        max = args.get(i).and_then(|s| s.parse().ok());
                    }
                    if args[i] == "--" { i += 1; break; }
                    i += 1;
                }
                if i >= args.len() {
                    sherr!("string split: missing separator");
                    return Ok(2);
                }
                let sep = &args[i];
                let rest = &args[i + 1..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for s in inputs {
                    let parts: Vec<String> = if sep.is_empty() {
                        s.chars().map(|_| String::new()).collect()
                    } else {
                        s.split(sep).map(|x| x.to_string()).collect()
                    };
                    let parts = if let Some(m) = max {
                        // join tail
                        if parts.len() > m {
                            let mut v: Vec<String> = parts[..m - 1].iter().map(|x| x.to_string()).collect();
                            v.push(parts[m - 1..].join(sep));
                            v
                        } else {
                            parts
                        }
                    } else {
                        parts
                    };
                    for p in parts {
                        out!("{}\n", p);
                    }
                }
                Ok(0)
            }
            "join" => {
                let sep = args.get(1).cloned().unwrap_or_default();
                let rest = &args[2..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                out!("{}\n", inputs.join(&sep));
                Ok(0)
            }
            "sub" => {
                let mut start: isize = 1;
                let mut length: Option<usize> = None;
                let mut i = 1;
                while i < args.len() && args[i].starts_with('-') {
                    if args[i] == "-s" || args[i] == "--start" {
                        i += 1;
                        start = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1);
                    } else if args[i] == "-l" || args[i] == "--length" {
                        i += 1;
                        length = args.get(i).and_then(|s| s.parse().ok());
                    }
                    if args[i] == "--" { i += 1; break; }
                    i += 1;
                }
                let rest = &args[i..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                for s in inputs {
                    let chars: Vec<char> = s.chars().collect();
                    let n = chars.len() as isize;
                    let mut st = if start < 0 { (n + start).max(0) as usize } else { (start - 1).max(0) as usize };
                    if st > chars.len() { st = chars.len(); }
                    let end = match length {
                        Some(l) => (st + l).min(chars.len()),
                        None => chars.len(),
                    };
                    let outstr: String = chars[st..end].iter().collect();
                    out!("{}\n", outstr);
                }
                Ok(0)
            }
            "match" => {
                let pat = args.get(1).cloned().unwrap_or_default();
                let rest = &args[2..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                // simple glob match
                for s in inputs {
                    if crate::glob::matches(&pat, &s) {
                        out!("{}\n", s);
                    }
                }
                Ok(0)
            }
            "collect" => {
                let rest = &args[1..];
                let inputs = if rest.is_empty() && !sys::isatty(0) {
                    String::from_utf8_lossy(&sys::read_all(0)).lines().map(|s| s.to_string()).collect()
                } else {
                    rest.to_vec()
                };
                out!("{}\n", inputs.join(""));
                Ok(0)
            }
            other => {
                sherr!("string: unknown subcommand `{}'", other);
                Ok(1)
            }
        }
    }

    fn builtin_ulimit(&mut self, args: &[String]) -> R {
        use libc::{RLIM_INFINITY, rlimit};
        #[derive(Clone, Copy)]
        struct Res { flag: char, name: &'static str, res: i32 }
        let mut resources: Vec<Res> = Vec::new();
        macro_rules! add {
            ($flag:expr, $name:expr, $r:expr) => {
                resources.push(Res { flag: $flag, name: $name, res: $r });
            };
        }
        add!('c', "core file size", libc::RLIMIT_CORE);
        add!('d', "data seg size", libc::RLIMIT_DATA);
        add!('f', "file size", libc::RLIMIT_FSIZE);
        add!('l', "max locked memory", libc::RLIMIT_MEMLOCK);
        add!('m', "max memory size", libc::RLIMIT_RSS);
        add!('n', "open files", libc::RLIMIT_NOFILE);
        add!('s', "stack size", libc::RLIMIT_STACK);
        add!('t', "cpu time", libc::RLIMIT_CPU);
        add!('u', "max user processes", libc::RLIMIT_NPROC);
        add!('v', "virtual memory", libc::RLIMIT_AS);
        // parse
        let mut hard_flag = false;
        let mut soft_flag = false;
        let mut all = false;
        let mut chosen: Option<char> = None;
        let mut value: Option<String> = None;
        for a in args {
            if a.starts_with('-') && a.len() > 1 && a != "--" {
                for c in a[1..].chars() {
                    match c {
                        'H' => hard_flag = true,
                        'S' => soft_flag = true,
                        'a' => all = true,
                        'c' | 'd' | 'f' | 'l' | 'm' | 'n' | 's' | 't' | 'u' | 'v' => chosen = Some(c),
                        _ => {}
                    }
                }
            } else {
                value = Some(a.clone());
            }
        }
        let hard = if hard_flag && !soft_flag { true } else { hard_flag };
        let soft = if !hard_flag && !soft_flag { true } else { soft_flag };
        let set_both = !hard_flag && !soft_flag;
        let hard = if set_both { true } else { hard };
        let soft = if set_both { true } else { soft };
        if all {
            for r in &resources {
                if r.res < 0 { continue; }
                unsafe {
                    let mut rl: rlimit = std::mem::zeroed();
                    if libc::getrlimit(r.res as _, &mut rl) == 0 {
                        let fmt = |x: libc::rlim_t| if x == RLIM_INFINITY { "unlimited".to_string() } else { x.to_string() };
                        out!("{:<24} {}\n", r.name, fmt(rl.rlim_cur));
                    }
                }
            }
            return Ok(0);
        }
        let flag = chosen.unwrap_or('f');
        let res = match resources.iter().find(|r| r.flag == flag) {
            Some(r) => r.res,
            None => {
                sherr!("ulimit: invalid option");
                return Ok(1);
            }
        };
        if res < 0 {
            sherr!("ulimit: not supported on this system");
            return Ok(1);
        }
        if value.is_none() {
            unsafe {
                let mut rl: rlimit = std::mem::zeroed();
                if libc::getrlimit(res as _, &mut rl) != 0 {
                    return Ok(1);
                }
                let v = if hard_flag && !soft_flag { rl.rlim_max } else { rl.rlim_cur };
                if v == RLIM_INFINITY {
                    out!("unlimited\n");
                } else {
                    out!("{}\n", v);
                }
            }
            return Ok(0);
        }
        let val_str = value.unwrap();
        let new = if val_str == "unlimited" {
            RLIM_INFINITY
        } else {
            match val_str.parse::<libc::rlim_t>() {
                Ok(n) => n,
                Err(_) => {
                    sherr!("ulimit: invalid number");
                    return Ok(1);
                }
            }
        };
        unsafe {
            let mut rl: rlimit = std::mem::zeroed();
            if libc::getrlimit(res as _, &mut rl) != 0 {
                return Ok(1);
            }
            if soft {
                rl.rlim_cur = new;
            }
            if hard {
                rl.rlim_max = new;
            }
            if libc::setrlimit(res as _, &rl) != 0 {
                sherr!("ulimit: {}", sys::last_error());
                return Ok(1);
            }
        }
        Ok(0)
    }
}
