//! `test`, `[` and `[[ ... ]]` evaluation.

use crate::ast::CondExpr;
use crate::shell::{Flow, Shell, Val};
use crate::sys;
use std::ffi::CString;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

const BINOPS: &[&str] = &["=", "==", "!=", "<", ">", "-eq", "-ne", "-lt", "-le", "-gt", "-ge", "-nt", "-ot", "-ef", "=~"];

fn is_unop(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    cs.len() == 2 && cs[0] == '-' && "abcdefghknoprstuvwxzGLNOS".contains(cs[1])
}

pub fn regex_match(text: &str, pat: &str) -> Result<Option<Vec<String>>, String> {
    let cpat = CString::new(pat).map_err(|_| "invalid regex".to_string())?;
    let ctext = CString::new(text).map_err(|_| "invalid text".to_string())?;
    unsafe {
        let mut re: libc::regex_t = std::mem::zeroed();
        if libc::regcomp(&mut re, cpat.as_ptr(), libc::REG_EXTENDED) != 0 {
            return Err(format!("invalid regular expression: {}", pat));
        }
        let n = pat.matches('(').count() + 1;
        let mut m: Vec<libc::regmatch_t> = vec![std::mem::zeroed(); n];
        let r = libc::regexec(&re, ctext.as_ptr(), n, m.as_mut_ptr(), 0);
        libc::regfree(&mut re);
        if r != 0 {
            return Ok(None);
        }
        let bytes = text.as_bytes();
        Ok(Some(
            m.iter()
                .map(|g| {
                    if g.rm_so >= 0 {
                        String::from_utf8_lossy(&bytes[g.rm_so as usize..g.rm_eo as usize]).into_owned()
                    } else {
                        String::new()
                    }
                })
                .collect(),
        ))
    }
}

struct TestParser<'a> {
    a: &'a [String],
    i: usize,
    err: Option<String>,
}

impl Shell {
    pub fn test_unary(&mut self, op: &str, a: &str) -> bool {
        let meta = || std::fs::metadata(a).ok();
        match op {
            "-z" => a.is_empty(),
            "-n" => !a.is_empty(),
            "-e" | "-a" => meta().is_some(),
            "-f" => meta().map(|m| m.is_file()).unwrap_or(false),
            "-d" => meta().map(|m| m.is_dir()).unwrap_or(false),
            "-s" => meta().map(|m| m.len() > 0).unwrap_or(false),
            "-L" | "-h" => std::fs::symlink_metadata(a).map(|m| m.file_type().is_symlink()).unwrap_or(false),
            "-p" => meta().map(|m| m.file_type().is_fifo()).unwrap_or(false),
            "-S" => meta().map(|m| m.file_type().is_socket()).unwrap_or(false),
            "-b" => meta().map(|m| m.file_type().is_block_device()).unwrap_or(false),
            "-c" => meta().map(|m| m.file_type().is_char_device()).unwrap_or(false),
            "-r" => sys::access(a, libc::R_OK),
            "-w" => sys::access(a, libc::W_OK),
            "-x" => sys::access(a, libc::X_OK),
            "-u" => meta().map(|m| m.mode() & 0o4000 != 0).unwrap_or(false),
            "-g" => meta().map(|m| m.mode() & 0o2000 != 0).unwrap_or(false),
            "-k" => meta().map(|m| m.mode() & 0o1000 != 0).unwrap_or(false),
            "-O" => meta().map(|m| m.uid() == sys::geteuid()).unwrap_or(false),
            "-G" => meta().map(|m| m.gid() == sys::getegid()).unwrap_or(false),
            "-N" => meta().map(|m| m.mtime() > m.atime()).unwrap_or(false),
            "-t" => a.parse::<i32>().map(sys::isatty).unwrap_or(false),
            "-o" => self.opts.get(a).unwrap_or(false),
            "-v" => {
                if let Some(i) = a.find('[').filter(|_| a.ends_with(']')) {
                    let (n, idx) = (&a[..i], &a[i + 1..a.len() - 1]);
                    match self.var(n).map(|v| v.val.clone()) {
                        Some(Val::Map(m)) => m.iter().any(|(k, _)| k == idx),
                        Some(Val::Arr(arr)) => idx.parse::<usize>().map(|k| k < arr.len()).unwrap_or(false),
                        _ => false,
                    }
                } else {
                    self.var(a).is_some() || a.parse::<usize>().map(|n| n >= 1 && n <= self.pos_params.len()).unwrap_or(false)
                }
            }
            _ => false,
        }
    }

    fn test_file_binary(&self, a: &str, op: &str, b: &str) -> bool {
        let (ma, mb) = (std::fs::metadata(a).ok(), std::fs::metadata(b).ok());
        match op {
            "-nt" => match (ma, mb) {
                (Some(x), Some(y)) => (x.mtime(), x.mtime_nsec()) > (y.mtime(), y.mtime_nsec()),
                (Some(_), None) => true,
                _ => false,
            },
            "-ot" => match (ma, mb) {
                (Some(x), Some(y)) => (x.mtime(), x.mtime_nsec()) < (y.mtime(), y.mtime_nsec()),
                (None, Some(_)) => true,
                _ => false,
            },
            _ => match (ma, mb) {
                (Some(x), Some(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
                _ => false,
            },
        }
    }

    fn test_binary(&mut self, a: &str, op: &str, b: &str) -> Result<bool, String> {
        let int = |s: &str| s.trim().parse::<i64>().map_err(|_| format!("{}: integer expression expected", s));
        Ok(match op {
            "=" | "==" => a == b,
            "!=" => a != b,
            "<" => a < b,
            ">" => a > b,
            "-eq" => int(a)? == int(b)?,
            "-ne" => int(a)? != int(b)?,
            "-lt" => int(a)? < int(b)?,
            "-le" => int(a)? <= int(b)?,
            "-gt" => int(a)? > int(b)?,
            "-ge" => int(a)? >= int(b)?,
            "=~" => regex_match(a, b)?.is_some(),
            _ => self.test_file_binary(a, op, b),
        })
    }

    fn t_or(&mut self, p: &mut TestParser) -> bool {
        let mut v = self.t_and(p);
        while p.i < p.a.len() && p.a[p.i] == "-o" {
            p.i += 1;
            let r = self.t_and(p);
            v = v || r;
        }
        v
    }

    fn t_and(&mut self, p: &mut TestParser) -> bool {
        let mut v = self.t_not(p);
        while p.i < p.a.len() && p.a[p.i] == "-a" {
            p.i += 1;
            let r = self.t_not(p);
            v = v && r;
        }
        v
    }

    fn t_not(&mut self, p: &mut TestParser) -> bool {
        let rem = p.a.len() - p.i;
        if rem >= 2 && p.a[p.i] == "!" && !(rem >= 3 && BINOPS.contains(&p.a[p.i + 1].as_str()) && rem != 4) {
            p.i += 1;
            return !self.t_not(p);
        }
        self.t_primary(p)
    }

    fn t_primary(&mut self, p: &mut TestParser) -> bool {
        let rem = p.a.len() - p.i;
        if rem == 0 {
            p.err = Some("argument expected".into());
            return false;
        }
        if rem >= 3 && BINOPS.contains(&p.a[p.i + 1].as_str()) {
            let (a, op, b) = (p.a[p.i].clone(), p.a[p.i + 1].clone(), p.a[p.i + 2].clone());
            p.i += 3;
            return match self.test_binary(&a, &op, &b) {
                Ok(v) => v,
                Err(e) => {
                    p.err = Some(e);
                    false
                }
            };
        }
        if p.a[p.i] == "(" && rem >= 3 {
            p.i += 1;
            let v = self.t_or(p);
            if p.i < p.a.len() && p.a[p.i] == ")" {
                p.i += 1;
            } else {
                p.err = Some("`)' expected".into());
            }
            return v;
        }
        if rem >= 2 && is_unop(&p.a[p.i]) {
            let (op, a) = (p.a[p.i].clone(), p.a[p.i + 1].clone());
            p.i += 2;
            return self.test_unary(&op, &a);
        }
        let v = !p.a[p.i].is_empty();
        p.i += 1;
        v
    }

    pub fn test_main(&mut self, args: &[String]) -> i32 {
        if args.is_empty() {
            return 1;
        }
        let mut p = TestParser { a: args, i: 0, err: None };
        let v = self.t_or(&mut p);
        if p.err.is_none() && p.i < args.len() {
            p.err = Some(format!("{}: unexpected argument", args[p.i]));
        }
        if let Some(e) = p.err {
            sherr!("test: {}", e);
            return 2;
        }
        !v as i32
    }

    pub fn eval_cond(&mut self, e: &CondExpr) -> Result<bool, Flow> {
        Ok(match e {
            CondExpr::Not(x) => !self.eval_cond(x)?,
            CondExpr::And(a, b) => self.eval_cond(a)? && self.eval_cond(b)?,
            CondExpr::Or(a, b) => self.eval_cond(a)? || self.eval_cond(b)?,
            CondExpr::Word(w) => !self.expand_word_str(w)?.is_empty(),
            CondExpr::Unary(op, w) => {
                let a = self.expand_word_str(w)?;
                self.test_unary(op, &a)
            }
            CondExpr::Binary(l, op, r) => {
                let a = self.expand_word_str(l)?;
                match op.as_str() {
                    "==" | "=" | "!=" => {
                        let pat = self.expand_pattern(r)?;
                        crate::glob::matches(&pat, &a) == (op != "!=")
                    }
                    "=~" => {
                        let re = self.expand_word_str(r)?;
                        match regex_match(&a, &re) {
                            Ok(Some(groups)) => {
                                let _ = self.set("BASH_REMATCH", Val::Arr(groups.clone()));
                                let _ = self.set("MATCH", Val::Arr(groups));
                                true
                            }
                            Ok(None) => {
                                let _ = self.set("BASH_REMATCH", Val::Arr(Vec::new()));
                                false
                            }
                            Err(msg) => {
                                sherr!("{}", msg);
                                return Err(Flow::Abort(2));
                            }
                        }
                    }
                    "<" => a < self.expand_word_str(r)?,
                    ">" => a > self.expand_word_str(r)?,
                    "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                        let b = self.expand_word_str(r)?;
                        let x = self.arith(&a)?;
                        let y = self.arith(&b)?;
                        let (x, y) = (x.as_f(), y.as_f());
                        match op.as_str() {
                            "-eq" => x == y,
                            "-ne" => x != y,
                            "-lt" => x < y,
                            "-le" => x <= y,
                            "-gt" => x > y,
                            _ => x >= y,
                        }
                    }
                    _ => {
                        let b = self.expand_word_str(r)?;
                        self.test_file_binary(&a, op, &b)
                    }
                }
            }
        })
    }
}
