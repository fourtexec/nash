//! Shell state: variables, options, functions, jobs.

use crate::ast::Func;
use crate::history::History;
use crate::sys;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::Instant;

/// Non-local control flow.
#[derive(Debug)]
pub enum Flow {
    Break(u32),
    Continue(u32),
    Return(i32),
    Exit(i32),
    /// Abort the current top-level command (expansion errors and the like).
    Abort(i32),
    /// A command failed inside a `try` block.
    Throw(i32),
    Interrupt,
}

pub type R = Result<i32, Flow>;

#[derive(Clone, Debug)]
pub enum Val {
    Unset,
    Str(String),
    Arr(Vec<String>),
    Map(Vec<(String, String)>),
}

#[derive(Clone, Debug)]
pub struct Var {
    pub val: Val,
    pub exported: bool,
    pub readonly: bool,
}

#[derive(Default, Clone)]
pub struct Opts {
    pub errexit: bool,
    pub nounset: bool,
    pub xtrace: bool,
    pub noglob: bool,
    pub noclobber: bool,
    pub pipefail: bool,
    pub autocd: bool,
    pub dotglob: bool,
    pub nullglob: bool,
    pub allexport: bool,
    pub verbose: bool,
}

pub const OPTION_NAMES: &[&str] = &["allexport", "autocd", "dotglob", "errexit", "noclobber", "noglob", "nounset", "nullglob", "pipefail", "verbose", "xtrace"];

impl Opts {
    pub fn get(&self, name: &str) -> Option<bool> {
        Some(match name {
            "errexit" => self.errexit,
            "nounset" => self.nounset,
            "xtrace" => self.xtrace,
            "noglob" => self.noglob,
            "noclobber" => self.noclobber,
            "pipefail" => self.pipefail,
            "autocd" => self.autocd,
            "dotglob" => self.dotglob,
            "nullglob" => self.nullglob,
            "allexport" => self.allexport,
            "verbose" => self.verbose,
            _ => return None,
        })
    }
    pub fn set(&mut self, name: &str, on: bool) -> bool {
        match name {
            "errexit" => self.errexit = on,
            "nounset" => self.nounset = on,
            "xtrace" => self.xtrace = on,
            "noglob" => self.noglob = on,
            "noclobber" => self.noclobber = on,
            "pipefail" => self.pipefail = on,
            "autocd" | "auto_cd" => self.autocd = on,
            "dotglob" | "globdots" | "glob_dots" => self.dotglob = on,
            "nullglob" | "null_glob" => self.nullglob = on,
            "allexport" => self.allexport = on,
            "verbose" => self.verbose = on,
            "strict" => {
                self.errexit = on;
                self.nounset = on;
                self.pipefail = on;
            }
            // accepted for compatibility, no effect
            "emacs" | "vi" | "monitor" | "notify" | "histexpand" | "braceexpand" | "hashall" | "globstar" | "extglob" | "extendedglob" | "extended_glob" | "interactive_comments" | "interactivecomments" | "posix" | "lastpipe" | "expand_aliases" | "promptsubst" | "prompt_subst" | "sharehistory" | "share_history" | "histignoredups" | "hist_ignore_dups" | "incappendhistory" | "inc_append_history" | "checkwinsize" | "histappend" | "cdspell" | "nocaseglob" | "correct" | "autopushd" | "auto_pushd" => {}
            _ => return false,
        }
        true
    }
    pub fn flags(&self) -> String {
        let mut s = String::new();
        for (c, on) in [('a', self.allexport), ('e', self.errexit), ('f', self.noglob), ('u', self.nounset), ('v', self.verbose), ('x', self.xtrace), ('C', self.noclobber)] {
            if on {
                s.push(c);
            }
        }
        s
    }
}

pub struct Proc {
    pub pid: i32,
    pub status: Option<i32>,
    pub stopped: bool,
}

pub struct Job {
    pub id: usize,
    pub pgid: i32,
    pub procs: Vec<Proc>,
    pub text: String,
    pub notified: bool,
}

impl Job {
    pub fn done(&self) -> bool {
        self.procs.iter().all(|p| p.status.is_some())
    }
    pub fn stopped(&self) -> bool {
        !self.done() && self.procs.iter().all(|p| p.status.is_some() || p.stopped)
    }
    pub fn status(&self) -> i32 {
        self.procs.last().and_then(|p| p.status).unwrap_or(0)
    }
}

pub struct Shell {
    pub scopes: Vec<HashMap<String, Var>>,
    pub pos_params: Vec<String>,
    pub arg0: String,
    pub funcs: HashMap<String, Rc<Func>>,
    pub aliases: Rc<HashMap<String, String>>,
    pub last_status: i32,
    pub last_bg: i32,
    pub opts: Opts,
    pub interactive: bool,
    pub login: bool,
    pub job_control: bool,
    pub is_child: bool,
    pub shell_pid: i32,
    pub shell_pgid: i32,
    pub jobs: Vec<Job>,
    pub traps: HashMap<i32, String>,
    pub dir_stack: Vec<String>,
    pub loop_depth: u32,
    pub func_depth: u32,
    pub cond_depth: u32,
    pub try_depth: u32,
    pub source_depth: u32,
    pub hash: HashMap<String, String>,
    pub defers: Vec<Vec<Vec<String>>>,
    pub exec_direct: bool,
    pub last_subst: Option<i32>,
    pub lineno: usize,
    pub start: Instant,
    pub history: History,
    pub completions: HashMap<String, Vec<String>>,
    pub completion_funcs: HashMap<String, String>,
    pub last_duration_ms: u64,
    pub cur_text: String,
    pub path_cmds: Option<(String, Vec<String>)>,
    pub self_exe: String,
    pub rng: u64,
    pub in_trap: bool,
    pub term_modes: Option<libc::termios>,
    pub getopts_off: usize,
    pub script_name: String,
    pub func_names: Vec<String>,
    pub tty: i32,
    pub procsubst_fds: Vec<i32>,
}

impl Shell {
    pub fn new() -> Shell {
        let mut global = HashMap::new();
        for (k, v) in std::env::vars_os() {
            let (k, v) = (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned());
            if crate::parser::is_name(&k) {
                global.insert(k, Var { val: Val::Str(v), exported: true, readonly: false });
            }
        }
        let pid = unsafe { libc::getpid() };
        let mut sh = Shell {
            scopes: vec![global],
            pos_params: Vec::new(),
            arg0: "nash".into(),
            funcs: HashMap::new(),
            aliases: Rc::new(HashMap::new()),
            last_status: 0,
            last_bg: 0,
            opts: Opts::default(),
            interactive: false,
            login: false,
            job_control: false,
            is_child: false,
            shell_pid: pid,
            shell_pgid: unsafe { libc::getpgrp() },
            jobs: Vec::new(),
            traps: HashMap::new(),
            dir_stack: Vec::new(),
            loop_depth: 0,
            func_depth: 0,
            cond_depth: 0,
            try_depth: 0,
            source_depth: 0,
            hash: HashMap::new(),
            defers: vec![Vec::new()],
            exec_direct: false,
            last_subst: None,
            lineno: 0,
            start: Instant::now(),
            history: History::new(),
            completions: HashMap::new(),
            completion_funcs: HashMap::new(),
            last_duration_ms: 0,
            cur_text: String::new(),
            path_cmds: None,
            self_exe: std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "nash".into()),
            rng: (pid as u64) ^ (sys::now_secs() as u64).wrapping_mul(0x9E3779B97F4A7C15),
            in_trap: false,
            term_modes: None,
            getopts_off: 0,
            script_name: "nash".into(),
            func_names: Vec::new(),
            tty: 0,
            procsubst_fds: Vec::new(),
        };
        // PWD
        let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
        let pwd_ok = match sh.var("PWD") {
            Some(Var { val: Val::Str(p), .. }) => {
                let p = p.clone();
                p.starts_with('/') && std::fs::canonicalize(&p).ok() == std::fs::canonicalize(&cwd).ok()
            }
            _ => false,
        };
        if !pwd_ok {
            let _ = sh.set_str("PWD", cwd);
        }
        sh.export_var("PWD");
        let (user, home) = sys::current_user();
        if sh.var("HOME").is_none() {
            if let Some(h) = home {
                let _ = sh.set_str("HOME", h);
                sh.export_var("HOME");
            }
        }
        if sh.var("USER").is_none() {
            let _ = sh.set_str("USER", user);
        }
        if sh.var("PATH").is_none() {
            let _ = sh.set_str("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
            sh.export_var("PATH");
        }
        let lvl = sh.get("SHLVL").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0) + 1;
        let _ = sh.set_str("SHLVL", lvl.to_string());
        sh.export_var("SHLVL");
        let _ = sh.set_str("PPID", unsafe { libc::getppid() }.to_string());
        let _ = sh.set_str("NASH_VERSION", env!("CARGO_PKG_VERSION"));
        let _ = sh.set_str("OPTIND", "1");
        let _ = sh.set_str("PS2", "… ");
        let _ = sh.set_str("PS4", "+ ");
        let _ = sh.set_str("HOSTNAME", sys::hostname());
        let exe = sh.self_exe.clone();
        let _ = sh.set_str("SHELL", exe);
        sh
    }

    // ---------- variables ----------

    pub fn var(&self, name: &str) -> Option<&Var> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return if matches!(v.val, Val::Unset) { None } else { Some(v) };
            }
        }
        None
    }

    fn scope_of(&self, name: &str) -> Option<usize> {
        (0..self.scopes.len()).rev().find(|&i| self.scopes[i].contains_key(name))
    }

    fn next_random(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// Scalar value of a variable (first element for arrays).
    pub fn get(&mut self, name: &str) -> Option<String> {
        match name {
            "RANDOM" => return Some((self.next_random() % 32768).to_string()),
            "SECONDS" => return Some(self.start.elapsed().as_secs().to_string()),
            "EPOCHSECONDS" => return Some(sys::now_secs().to_string()),
            "EPOCHREALTIME" => {
                let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                return Some(format!("{}.{:06}", d.as_secs(), d.subsec_micros()));
            }
            "LINENO" => return Some(self.lineno.to_string()),
            "FUNCNAME" => return self.func_names.last().cloned(),
            _ => {}
        }
        match &self.var(name)?.val {
            Val::Str(s) => Some(s.clone()),
            Val::Arr(a) => a.first().cloned(),
            Val::Map(m) => m.iter().find(|(k, _)| k == "0").map(|(_, v)| v.clone()),
            Val::Unset => None,
        }
    }

    pub fn set(&mut self, name: &str, val: Val) -> Result<(), String> {
        let idx = self.scope_of(name).unwrap_or(0);
        let allexport = self.opts.allexport;
        match self.scopes[idx].get_mut(name) {
            Some(v) => {
                if v.readonly {
                    return Err(format!("{}: readonly variable", name));
                }
                v.val = val;
                if allexport {
                    v.exported = true;
                }
            }
            None => {
                self.scopes[idx].insert(name.to_string(), Var { val, exported: allexport, readonly: false });
            }
        }
        if name == "PATH" {
            self.hash.clear();
        }
        Ok(())
    }

    pub fn set_str(&mut self, name: &str, val: impl Into<String>) -> Result<(), String> {
        self.set(name, Val::Str(val.into()))
    }

    /// Create (or overwrite) a variable in the innermost scope.
    pub fn set_local(&mut self, name: &str, val: Val) -> Result<(), String> {
        let top = self.scopes.len() - 1;
        match self.scopes[top].get_mut(name) {
            Some(v) => {
                if v.readonly {
                    return Err(format!("{}: readonly variable", name));
                }
                v.val = val;
            }
            None => {
                self.scopes[top].insert(name.to_string(), Var { val, exported: false, readonly: false });
            }
        }
        Ok(())
    }

    pub fn var_mut(&mut self, name: &str) -> Option<&mut Var> {
        let idx = self.scope_of(name)?;
        self.scopes[idx].get_mut(name)
    }

    pub fn unset_var(&mut self, name: &str) -> Result<(), String> {
        if let Some(idx) = self.scope_of(name) {
            if self.scopes[idx][name].readonly {
                return Err(format!("{}: cannot unset: readonly variable", name));
            }
            if idx == 0 {
                self.scopes[0].remove(name);
            } else {
                self.scopes[idx].get_mut(name).unwrap().val = Val::Unset;
            }
            if name == "PATH" {
                self.hash.clear();
            }
        }
        Ok(())
    }

    pub fn export_var(&mut self, name: &str) {
        match self.var_mut(name) {
            Some(v) => v.exported = true,
            None => {
                self.scopes[0].insert(name.to_string(), Var { val: Val::Unset, exported: true, readonly: false });
            }
        }
    }

    pub fn environ(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        for s in &self.scopes {
            for (k, v) in s {
                match &v.val {
                    Val::Str(val) if v.exported => {
                        map.insert(k.clone(), val.clone());
                    }
                    Val::Unset => {
                        map.remove(k);
                    }
                    _ => {}
                }
            }
        }
        map
    }

    pub fn ifs(&mut self) -> String {
        self.get("IFS").unwrap_or_else(|| " \t\n".to_string())
    }

    pub fn home(&mut self) -> String {
        self.get("HOME").unwrap_or_else(|| "/".to_string())
    }

    pub fn cwd(&mut self) -> String {
        self.get("PWD").unwrap_or_else(|| "/".to_string())
    }

    // ---------- command lookup ----------

    pub fn find_command(&mut self, name: &str) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        if name.contains('/') {
            return if std::fs::metadata(name).is_ok() { Some(name.to_string()) } else { None };
        }
        if let Some(p) = self.hash.get(name) {
            if sys::is_executable(p) {
                return Some(p.clone());
            }
        }
        let path = self.get("PATH").unwrap_or_default();
        for dir in path.split(':') {
            let full = if dir.is_empty() { name.to_string() } else { format!("{}/{}", dir.trim_end_matches('/'), name) };
            if sys::is_executable(&full) {
                self.hash.insert(name.to_string(), full.clone());
                return Some(full);
            }
        }
        None
    }

    /// Names of all executables on PATH (cached per PATH value).
    pub fn path_commands(&mut self) -> &Vec<String> {
        let path = self.get("PATH").unwrap_or_default();
        let fresh = matches!(&self.path_cmds, Some((p, _)) if *p == path);
        if !fresh {
            let mut v = Vec::new();
            for dir in path.split(':') {
                if let Ok(rd) = std::fs::read_dir(if dir.is_empty() { "." } else { dir }) {
                    for e in rd.flatten() {
                        v.push(e.file_name().to_string_lossy().into_owned());
                    }
                }
            }
            v.sort();
            v.dedup();
            self.path_cmds = Some((path, v));
        }
        &self.path_cmds.as_ref().unwrap().1
    }

    /// Every command name the shell knows about: builtins, functions, aliases, PATH.
    pub fn command_names(&mut self) -> Vec<String> {
        let mut v: Vec<String> = crate::builtins::BUILTINS.iter().map(|s| s.to_string()).collect();
        v.extend(self.funcs.keys().cloned());
        v.extend(self.aliases.keys().cloned());
        v.extend(self.path_commands().iter().cloned());
        v.sort();
        v.dedup();
        v
    }

    pub fn is_known_command(&mut self, name: &str) -> bool {
        self.funcs.contains_key(name) || self.aliases.contains_key(name) || crate::builtins::is_builtin(name) || self.find_command(name).is_some()
    }
}
