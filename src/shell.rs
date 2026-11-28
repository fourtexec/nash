//! Shell state: variables, options, functions, jobs.

use crate::ast::Func;
use crate::history::History;
use crate::sys;
use std::collections::{BTreeMap, HashMap};
use std::ffi::CString;
use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;
use std::time::Instant;

/// Fast non-cryptographic hasher for short shell identifiers (variables, functions, PATH hash).
#[derive(Default, Clone, Copy)]
pub struct FxBuildHasher;

pub struct FxHasher {
    hash: usize,
}

impl BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;
    #[inline]
    fn build_hasher(&self) -> FxHasher {
        FxHasher { hash: 0 }
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash as u64
    }
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        const K: usize = 0x517cc1b727220a95;
        let mut h = self.hash;
        while bytes.len() >= 8 {
            let w = usize::from_ne_bytes(bytes[..8].try_into().unwrap());
            h = (h.rotate_left(5) ^ w).wrapping_mul(K);
            bytes = &bytes[8..];
        }
        if bytes.len() >= 4 {
            let w = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize;
            h = (h.rotate_left(5) ^ w).wrapping_mul(K);
            bytes = &bytes[4..];
        }
        if bytes.len() >= 2 {
            let w = u16::from_ne_bytes(bytes[..2].try_into().unwrap()) as usize;
            h = (h.rotate_left(5) ^ w).wrapping_mul(K);
            bytes = &bytes[2..];
        }
        if let Some(&b) = bytes.first() {
            h = (h.rotate_left(5) ^ (b as usize)).wrapping_mul(K);
        }
        self.hash = h;
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        const K: usize = 0x517cc1b727220a95;
        self.hash = (self.hash.rotate_left(5) ^ (i as usize)).wrapping_mul(K);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        const K: usize = 0x517cc1b727220a95;
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(K);
    }
}

pub type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;

#[inline]
pub fn fx_map<K, V>() -> FxHashMap<K, V> {
    HashMap::with_hasher(FxBuildHasher)
}

#[inline]
pub fn fx_map_with_capacity<K, V>(cap: usize) -> FxHashMap<K, V> {
    HashMap::with_capacity_and_hasher(cap, FxBuildHasher)
}

fn make_env_cstring(k: &str, v: &str) -> CString {
    let mut buf = Vec::with_capacity(k.len() + 1 + v.len());
    for &b in k.as_bytes() {
        if b != 0 {
            buf.push(b);
        }
    }
    buf.push(b'=');
    for &b in v.as_bytes() {
        if b != 0 {
            buf.push(b);
        }
    }
    CString::new(buf).unwrap()
}

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
    Int(i64),
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
    pub strict_arith: bool,
    pub autocd: bool,
    pub dotglob: bool,
    pub nullglob: bool,
    pub allexport: bool,
    pub verbose: bool,
}

pub const OPTION_NAMES: &[&str] = &["allexport", "autocd", "dotglob", "errexit", "noclobber", "noglob", "nounset", "nullglob", "pipefail", "strict_arith", "verbose", "xtrace"];

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
            "strict_arith" => self.strict_arith,
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
            "strict_arith" => self.strict_arith = on,
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

/// Resolved repository lookup for one `(cwd, GIT_DIR)` pair. `head_path` is
/// precomputed so prompt rendering never has to build a path.
pub(crate) struct GitCache {
    pub(crate) cwd: String,
    pub(crate) git_dir_env: Option<String>,
    pub(crate) git_dir: Option<String>,
    pub(crate) head_path: Option<Rc<str>>,
    /// Precomputed `<cwd>/.git`, used to invalidate the negative cache without
    /// allocating a `PathBuf` on every prompt.
    pub(crate) dot_git: Rc<str>,
}

/// Cached branch name, keyed by the resolved `HEAD` path and its metadata.
/// Re-reading is only needed when `HEAD`'s mtime, size, or inode changes.
pub(crate) struct GitBranchCache {
    pub(crate) head_path: Rc<str>,
    pub(crate) branch: Option<String>,
    /// `(mtime, size, inode)`. The inode catches atomic `HEAD` replacement even
    /// when the timestamp resolution and file size happen to match.
    pub(crate) stamp: Option<(std::time::SystemTime, u64, u64)>,
}

fn is_locale_var(name: &str) -> bool {
    name == "LC_ALL" || name == "LC_CTYPE" || name == "LANG"
}

pub struct Shell {
    pub scopes: Vec<FxHashMap<String, Var>>,
    pub pos_params: Vec<String>,
    pub arg0: String,
    pub funcs: FxHashMap<String, Rc<Func>>,
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
    pub hash: FxHashMap<String, String>,
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
    /// Lazily resolved home directory (resolved on first access to avoid getpwuid syscall at startup).
    home_dir: Option<String>,
    /// Lazily resolved username (resolved on first access to avoid getpwuid syscall at startup).
    user_name: Option<String>,
    /// Host name, cached once at startup so prompt escapes never call
    /// `gethostname` or allocate on every prompt.
    hostname: String,
    /// Cached UTF-8 locale detection for the prompt symbol. Invalidated when
    /// one of the locale variables changes.
    pub(crate) prompt_utf8: Option<bool>,
    /// Cache of the resolved git directory for the last prompt's cwd and
    /// `GIT_DIR`, so the common case only stats `HEAD` instead of walking the
    /// tree. `git_dir: None` means "not inside a repository".
    pub(crate) git_cache: Option<GitCache>,
    /// Last branch name and `HEAD` metadata, so a prompt in the same
    /// repository does not re-read the file when it has not changed.
    pub(crate) git_branch_cache: Option<GitBranchCache>,
    /// Cached null-terminated `envp` built from global exported variables.
    /// Avoids rebuilding 40+ `CString` allocations on every external spawn.
    env_cache_strings: Vec<CString>,
    env_cache_ptrs: Vec<*const libc::c_char>,
    pub env_dirty: bool,
}

impl Shell {
    pub fn new() -> Shell {
        let pid = unsafe { libc::getpid() };

        // Import the environment in a single pass. Doing it once is cheap, and
        // it makes `$VAR`, `$<TAB>`, prompt escapes and child processes all see
        // the variables the shell was started with.
        let mut global = fx_map_with_capacity(64);
        for (k, v) in std::env::vars_os() {
            let (Some(k), Some(v)) = (k.to_str(), v.to_str()) else { continue };
            if crate::parser::is_name(k) {
                global.insert(k.to_string(), Var { val: Val::Str(v.to_string()), exported: true, readonly: false });
            }
        }
        let mut sh = Shell {
            scopes: vec![global],
            pos_params: Vec::new(),
            arg0: "nash".into(),
            funcs: fx_map(),
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
            hash: fx_map(),
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
            rng: 0,
            in_trap: false,
            term_modes: None,
            getopts_off: 0,
            script_name: "nash".into(),
            func_names: Vec::new(),
            tty: 0,
            procsubst_fds: Vec::new(),
            home_dir: None,
            user_name: None,
            hostname: String::new(),
            prompt_utf8: None,
            git_cache: None,
            git_branch_cache: None,
            env_cache_strings: Vec::new(),
            env_cache_ptrs: Vec::new(),
            env_dirty: true,
        };
        let host = sys::hostname();
        sh.hostname = host.clone();
        let _ = sh.set_str("HOSTNAME", host);
        // PWD — needed for autocd and relative path resolution.
        let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
        let _ = sh.set_str("PWD", cwd);
        sh.export_var("PWD");

        // Like bash/zsh, never inherit a custom IFS. Reset it to the default,
        // keeping the exported flag if it happened to be exported.
        let _ = sh.set_str("IFS", " \t\n");

        // Prime the HOME/USER caches from the imported environment, but keep
        // the lazy getpwuid fallback for shells started without them.
        let home = match sh.var("HOME") {
            Some(Var { val: Val::Str(h), .. }) => Some(h.clone()),
            _ => None,
        };
        if let Some(h) = home {
            sh.home_dir = Some(h);
        }
        let user = match sh.var("USER") {
            Some(Var { val: Val::Str(u), .. }) => Some(u.clone()),
            _ => None,
        };
        if let Some(u) = user {
            sh.user_name = Some(u);
        }

        let lvl = sh.get("SHLVL").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0) + 1;
        let _ = sh.set_str("SHLVL", lvl.to_string());
        sh.export_var("SHLVL");
        let _ = sh.set_str("PPID", sys::getppid().to_string());
        let _ = sh.set_str("NASH_VERSION", crate::version());
        let _ = sh.set_str("OPTIND", "1");
        if sh.get("PS2").is_none() {
            let _ = sh.set_str("PS2", "… ");
        }
        if sh.get("PS4").is_none() {
            let _ = sh.set_str("PS4", "+ ");
        }
        let exe = sh.self_exe.clone();
        let _ = sh.set_str("SHELL", exe);
        sh
    }

    // ---------- variables ----------

    #[inline]
    pub fn var(&self, name: &str) -> Option<&Var> {
        if self.scopes.len() == 1 {
            return match self.scopes[0].get(name) {
                Some(v) if !matches!(v.val, Val::Unset) => Some(v),
                _ => None,
            };
        }
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return if matches!(v.val, Val::Unset) { None } else { Some(v) };
            }
        }
        None
    }

    #[inline]
    fn scope_of(&self, name: &str) -> Option<usize> {
        if self.scopes.len() == 1 {
            return if self.scopes[0].contains_key(name) { Some(0) } else { None };
        }
        (0..self.scopes.len()).rev().find(|&i| self.scopes[i].contains_key(name))
    }

    pub fn next_random(&mut self) -> u64 {
        if self.rng == 0 {
            self.rng = sys::random_u64();
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// Borrow a scalar variable without allocating when it is not a dynamic special variable.
    #[inline]
    pub fn get_cow(&mut self, name: &str) -> Option<std::borrow::Cow<'_, str>> {
        use std::borrow::Cow;
        if matches!(name.as_bytes().first(), Some(b'R' | b'S' | b'E' | b'L' | b'F' | b'U')) {
            match name {
                "RANDOM" | "SECONDS" | "EPOCHSECONDS" | "EPOCHREALTIME" | "LINENO" | "FUNCNAME" | "USER" => {
                    return self.get(name).map(Cow::Owned);
                }
                _ => {}
            }
        }
        match &self.var(name)?.val {
            Val::Int(n) => Some(Cow::Owned(n.to_string())),
            Val::Str(s) => Some(Cow::Borrowed(s.as_str())),
            Val::Arr(a) => a.first().map(|s| Cow::Borrowed(s.as_str())),
            Val::Map(m) => m.iter().find(|(k, _)| k == "0").map(|(_, v)| Cow::Borrowed(v.as_str())),
            Val::Unset => None,
        }
    }

    /// Scalar value of a variable (first element for arrays).
    pub fn get(&mut self, name: &str) -> Option<String> {
        if matches!(name.as_bytes().first(), Some(b'R' | b'S' | b'E' | b'L' | b'F' | b'U')) {
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
                "USER" => {
                    if let Some(u) = self.user_name.as_ref() {
                        return Some(u.clone());
                    }
                    let u = sys::current_user().0;
                    self.user_name = Some(u.clone());
                    let _ = self.set_str("USER", u.clone());
                    self.export_var("USER");
                    return Some(u);
                }
                _ => {}
            }
        }
        match &self.var(name)?.val {
            Val::Int(n) => Some(n.to_string()),
            Val::Str(s) => Some(s.clone()),
            Val::Arr(a) => a.first().cloned(),
            Val::Map(m) => m.iter().find(|(k, _)| k == "0").map(|(_, v)| v.clone()),
            Val::Unset => None,
        }
    }

    /// Fast arithmetic lookup for a scalar variable: returns `Some(Ok(n))` immediately
    /// when stored natively as `Val::Int(n)`, avoiding string formatting/parsing.
    #[inline]
    pub fn get_int_fast(&self, name: &str) -> Option<i64> {
        if matches!(name.as_bytes().first(), Some(b'R' | b'S' | b'E' | b'L' | b'F' | b'U'))
            && matches!(name, "RANDOM" | "SECONDS" | "EPOCHSECONDS" | "EPOCHREALTIME" | "LINENO" | "FUNCNAME" | "USER")
        {
            return None;
        }
        match self.var(name)?.val {
            Val::Int(n) => Some(n),
            _ => None,
        }
    }

    /// Single-lookup in-place integer addition (`i++`, `i--`, `sum += x`).
    /// Returns `Some(Ok((old, new)))` on hit, `Some(Err(..))` on readonly,
    /// or `None` if the variable does not exist or is not currently a `Val::Int`.
    #[inline]
    pub fn add_i64_in_place(&mut self, name: &str, delta: i64) -> Option<Result<(i64, i64), String>> {
        if matches!(name.as_bytes().first(), Some(b'R' | b'S' | b'E' | b'L' | b'F' | b'U' | b'I' | b'P' | b'H'))
            && matches!(
                name,
                "RANDOM" | "SECONDS" | "EPOCHSECONDS" | "EPOCHREALTIME" | "LINENO" | "FUNCNAME" | "USER"
                | "IFS" | "PATH" | "PS1" | "RPROMPT" | "RPS1" | "PWD" | "HOME" | "LC_ALL" | "LC_CTYPE" | "LANG"
            )
        {
            return None;
        }
        let idx = if self.scopes.len() == 1 {
            0
        } else {
            self.scope_of(name)?
        };
        let allexport = self.opts.allexport;
        let v = self.scopes[idx].get_mut(name)?;
        if let Val::Int(ref mut n) = v.val {
            if v.readonly {
                return Some(Err(format!("{}: readonly variable", name)));
            }
            let old = *n;
            let new = old.wrapping_add(delta);
            *n = new;
            if allexport || v.exported {
                v.exported = true;
                self.env_dirty = true;
            }
            Some(Ok((old, new)))
        } else {
            None
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
                if v.exported {
                    self.env_dirty = true;
                }
            }
            None => {
                if allexport {
                    self.env_dirty = true;
                }
                self.scopes[idx].insert(name.to_string(), Var { val, exported: allexport, readonly: false });
            }
        }
        if name == "PATH" {
            self.hash.clear();
        }
        if is_locale_var(name) {
            self.prompt_utf8 = None;
        }
        Ok(())
    }

    pub fn set_str(&mut self, name: &str, val: impl Into<String>) -> Result<(), String> {
        self.set(name, Val::Str(val.into()))
    }

    /// Fast integer assignment storing `Val::Int(val)` natively without decimal formatting.
    #[inline]
    pub fn set_i64(&mut self, name: &str, val: i64) -> Result<(), String> {
        // Keep special shell/prompt/IFS variables as strings so `&str` accessors always see Val::Str.
        if matches!(name.as_bytes().first(), Some(b'I' | b'P' | b'R' | b'H' | b'L'))
            && matches!(name, "IFS" | "PATH" | "PS1" | "RPROMPT" | "RPS1" | "PWD" | "HOME" | "LC_ALL" | "LC_CTYPE" | "LANG")
        {
            return self.set_str(name, val.to_string());
        }
        let idx = if self.scopes.len() == 1 {
            0
        } else {
            self.scope_of(name).unwrap_or(0)
        };
        let allexport = self.opts.allexport;
        if let Some(v) = self.scopes[idx].get_mut(name) {
            if v.readonly {
                return Err(format!("{}: readonly variable", name));
            }
            v.val = Val::Int(val);
            if allexport {
                v.exported = true;
            }
            if v.exported {
                self.env_dirty = true;
            }
        } else {
            if allexport {
                self.env_dirty = true;
            }
            self.scopes[idx].insert(name.to_string(), Var { val: Val::Int(val), exported: allexport, readonly: false });
        }
        Ok(())
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
                if v.exported {
                    self.env_dirty = true;
                }
            }
            None => {
                self.scopes[top].insert(name.to_string(), Var { val, exported: false, readonly: false });
            }
        }
        if is_locale_var(name) {
            self.prompt_utf8 = None;
        }
        Ok(())
    }

    pub fn var_mut(&mut self, name: &str) -> Option<&mut Var> {
        if is_locale_var(name) {
            self.prompt_utf8 = None;
        }
        let idx = self.scope_of(name)?;
        let v = self.scopes[idx].get_mut(name)?;
        if v.exported {
            self.env_dirty = true;
        }
        Some(v)
    }

    pub fn unset_var(&mut self, name: &str) -> Result<(), String> {
        if let Some(idx) = self.scope_of(name) {
            if self.scopes[idx][name].readonly {
                return Err(format!("{}: cannot unset: readonly variable", name));
            }
            if self.scopes[idx][name].exported {
                self.env_dirty = true;
            }
            if idx == 0 {
                self.scopes[0].remove(name);
            } else {
                self.scopes[idx].get_mut(name).unwrap().val = Val::Unset;
            }
            if name == "PATH" {
                self.hash.clear();
            }
            if is_locale_var(name) {
                self.prompt_utf8 = None;
            }
        }
        Ok(())
    }

    pub fn export_var(&mut self, name: &str) {
        self.env_dirty = true;
        match self.var_mut(name) {
            Some(v) => v.exported = true,
            None => {
                self.scopes[0].insert(name.to_string(), Var { val: Val::Unset, exported: true, readonly: false });
            }
        }
    }

    /// Return a cached, null-terminated `envp` pointer array for `scopes[0]` exported vars.
    /// Must only be used when `self.scopes.len() == 1` and there are no per-command extra env vars.
    pub fn cached_envp(&mut self) -> &[*const libc::c_char] {
        if self.env_dirty || self.env_cache_ptrs.is_empty() {
            self.env_cache_strings.clear();
            self.env_cache_ptrs.clear();
            for (k, v) in &self.scopes[0] {
                if !v.exported {
                    continue;
                }
                match &v.val {
                    Val::Str(val) => {
                        self.env_cache_strings.push(make_env_cstring(k, val));
                    }
                    Val::Int(n) => {
                        self.env_cache_strings.push(make_env_cstring(k, &n.to_string()));
                    }
                    _ => {}
                }
            }
            self.env_cache_ptrs.reserve(self.env_cache_strings.len() + 1);
            for s in &self.env_cache_strings {
                self.env_cache_ptrs.push(s.as_ptr());
            }
            self.env_cache_ptrs.push(std::ptr::null());
            self.env_dirty = false;
        }
        &self.env_cache_ptrs
    }

    pub fn environ(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        for s in &self.scopes {
            for (k, v) in s {
                match &v.val {
                    Val::Str(val) if v.exported => {
                        map.insert(k.clone(), val.clone());
                    }
                    Val::Int(n) if v.exported => {
                        map.insert(k.clone(), n.to_string());
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

    pub fn ifs_str(&self) -> &str {
        match self.var("IFS").map(|v| &v.val) {
            Some(Val::Str(s)) => s.as_str(),
            Some(Val::Arr(a)) => a.first().map(|s| s.as_str()).unwrap_or(" \t\n"),
            Some(Val::Map(m)) => m.iter().find(|(k, _)| k == "0").map(|(_, v)| v.as_str()).unwrap_or(" \t\n"),
            _ => " \t\n",
        }
    }

    pub fn home(&mut self) -> String {
        if let Some(h) = self.get("HOME") {
            return h;
        }
        if let Some(h) = self.home_dir.as_ref() {
            return h.clone();
        }
        // Lazily resolve HOME from the system (avoid getpwuid syscall at startup).
        let h = sys::current_user().1.unwrap_or_else(|| "/".to_string());
        self.home_dir = Some(h.clone());
        let _ = self.set_str("HOME", h.clone());
        self.export_var("HOME");
        h
    }

    pub fn cwd(&mut self) -> String {
        self.get("PWD").unwrap_or_else(|| "/".to_string())
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
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
            return Some(p.clone());
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
