//! Command execution: pipelines, redirections, job control, functions.

use crate::ast::*;
use crate::expand::shell_quote;
use crate::parser::Parser;
use crate::shell::*;
use crate::sys::{self, WaitStatus};
use std::collections::HashMap;
use std::ffi::CString;
use std::rc::Rc;
use std::time::Instant;

pub const ERR_TRAP: i32 = -1;
const DECL: &[&str] = &["declare", "typeset", "local", "export", "readonly"];

fn rusage_times() -> (f64, f64) {
    let mut user = 0.0;
    let mut sys_ = 0.0;
    for who in [libc::RUSAGE_SELF, libc::RUSAGE_CHILDREN] {
        unsafe {
            let mut ru: libc::rusage = std::mem::zeroed();
            libc::getrusage(who, &mut ru);
            user += ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 / 1e6;
            sys_ += ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 / 1e6;
        }
    }
    (user, sys_)
}

fn fmt_time(t: f64) -> String {
    format!("{}m{:.3}s", (t / 60.0) as u64, t % 60.0)
}

pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev2: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                cur[j] = cur[j].min(prev2[j - 2] + 1);
            }
        }
        prev2 = prev;
        prev = cur;
    }
    prev[b.len()]
}

impl Shell {
    // ---------- entry points ----------

    /// Parse and run shell source text, one complete command at a time.
    pub fn run_source(&mut self, src: &str, name: &str) -> R {
        let mut p = Parser::new(src);
        let mut status = self.last_status;
        loop {
            p.aliases = self.aliases.clone();
            match p.parse_program_command() {
                Ok(None) => break,
                Ok(Some(cmd)) => {
                    status = match self.run(&cmd) {
                        // an expansion error aborts only the current command
                        Err(Flow::Abort(s)) if self.try_depth == 0 => {
                            self.last_status = s;
                            return Ok(s);
                        }
                        other => other?,
                    }
                }
                Err(e) => {
                    if self.interactive && name == "nash" {
                        sherr!("{}", e.msg);
                    } else {
                        sherr!("{}: line {}: {}", name, e.line, e.msg);
                    }
                    self.last_status = 2;
                    return Ok(2);
                }
            }
        }
        Ok(status)
    }

    pub fn run(&mut self, cmd: &Cmd) -> R {
        let r = self.run_inner(cmd);
        if let Ok(s) = &r {
            self.last_status = *s;
        }
        r
    }

    pub fn check_signals(&mut self) -> Result<(), Flow> {
        use std::sync::atomic::Ordering;
        if !sys::ANY_PENDING.load(Ordering::SeqCst) {
            return Ok(());
        }
        sys::ANY_PENDING.store(false, Ordering::SeqCst);
        for sig in 1..65 {
            if !sys::take_pending(sig) {
                continue;
            }
            match self.traps.get(&sig).cloned() {
                Some(cmd) => {
                    if !cmd.is_empty() {
                        self.run_trap(&cmd)?;
                    }
                }
                None => {
                    if sig == libc::SIGINT && self.interactive && !self.is_child {
                        sys::ANY_PENDING.store(true, Ordering::SeqCst);
                        return Err(Flow::Interrupt);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn run_trap(&mut self, cmd: &str) -> Result<(), Flow> {
        if self.in_trap {
            return Ok(());
        }
        self.in_trap = true;
        let saved = self.last_status;
        let r = self.run_source(cmd, "trap");
        self.in_trap = false;
        self.last_status = saved;
        match r {
            Err(Flow::Exit(s)) => Err(Flow::Exit(s)),
            _ => Ok(()),
        }
    }

    /// Apply errexit / `try` semantics to a finished command.
    fn after(&mut self, status: i32) -> R {
        if status != 0 && self.cond_depth == 0 {
            if let Some(t) = self.traps.get(&ERR_TRAP).cloned() {
                self.run_trap(&t)?;
            }
            if self.try_depth > 0 {
                return Err(Flow::Throw(status));
            }
            if self.opts.errexit {
                return Err(Flow::Exit(status));
            }
        }
        Ok(status)
    }

    fn loop_iter(&mut self, body: &Cmd, status: &mut i32) -> Result<bool, Flow> {
        self.loop_depth += 1;
        let r = self.run(body);
        self.loop_depth -= 1;
        match r {
            Ok(s) => {
                *status = s;
                Ok(false)
            }
            Err(Flow::Break(n)) => {
                if n > 1 {
                    Err(Flow::Break(n - 1))
                } else {
                    Ok(true)
                }
            }
            Err(Flow::Continue(n)) => {
                if n > 1 {
                    Err(Flow::Continue(n - 1))
                } else {
                    Ok(false)
                }
            }
            Err(e) => Err(e),
        }
    }

    fn run_cond(&mut self, c: &Cmd) -> R {
        self.cond_depth += 1;
        let r = self.run(c);
        self.cond_depth -= 1;
        r
    }

    fn run_inner(&mut self, cmd: &Cmd) -> R {
        match cmd {
            Cmd::Simple(sc) => {
                let s = self.exec_simple(sc)?;
                self.after(s)
            }
            Cmd::List(items) => {
                let mut st = 0;
                for it in items {
                    self.check_signals()?;
                    if let Some(t) = &it.text {
                        if !self.is_child && !it.bg {
                            self.cur_text = t.clone();
                        }
                    }
                    if it.bg {
                        let text = it.text.clone().unwrap_or_default();
                        st = self.run_bg(&it.cmd, &text)?;
                        self.last_status = st;
                    } else {
                        match self.run(&it.cmd) {
                            Ok(s) => st = s,
                            Err(Flow::Abort(s)) => {
                                self.last_status = s;
                                return Err(Flow::Abort(s));
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }
                Ok(st)
            }
            Cmd::AndOr(l, and, r) => {
                let ls = self.run_cond(l)?;
                self.last_status = ls;
                if (ls == 0) == *and {
                    self.run(r)
                } else {
                    Ok(ls)
                }
            }
            Cmd::Pipeline { cmds, negate, timed } => {
                let t0 = Instant::now();
                let (u0, s0) = if *timed { rusage_times() } else { (0.0, 0.0) };
                if *negate {
                    self.cond_depth += 1;
                }
                let r = if cmds.is_empty() {
                    Ok(0)
                } else if cmds.len() == 1 {
                    self.run(&cmds[0])
                } else {
                    self.run_pipeline(cmds)
                };
                if *negate {
                    self.cond_depth -= 1;
                }
                if *timed {
                    let (u1, s1) = rusage_times();
                    err!("\nreal\t{}\nuser\t{}\nsys\t{}\n", fmt_time(t0.elapsed().as_secs_f64()), fmt_time(u1 - u0), fmt_time(s1 - s0));
                }
                let s = r?;
                if *negate {
                    Ok((s == 0) as i32)
                } else if cmds.len() > 1 {
                    self.after(s)
                } else {
                    Ok(s)
                }
            }
            Cmd::Group(body) => self.run(body),
            Cmd::Subshell(body) => {
                let s = self.run_subshell(body)?;
                self.after(s)
            }
            Cmd::If { conds, else_ } => {
                for (c, body) in conds {
                    if self.run_cond(c)? == 0 {
                        return self.run(body);
                    }
                }
                match else_ {
                    Some(e) => self.run(e),
                    None => Ok(0),
                }
            }
            Cmd::While { cond, body, until } => {
                let mut status = 0;
                loop {
                    self.check_signals()?;
                    self.loop_depth += 1;
                    let c = self.run_cond(cond);
                    self.loop_depth -= 1;
                    let c = match c {
                        Ok(c) => c,
                        Err(Flow::Break(n)) => {
                            if n > 1 {
                                return Err(Flow::Break(n - 1));
                            }
                            break;
                        }
                        Err(e) => return Err(e),
                    };
                    if (c == 0) == *until {
                        break;
                    }
                    if self.loop_iter(body, &mut status)? {
                        break;
                    }
                }
                Ok(status)
            }
            Cmd::For { var, words, body } => {
                let items = match words {
                    Some(ws) => self.expand_words(ws)?,
                    None => self.pos_params.clone(),
                };
                let mut status = 0;
                for it in items {
                    self.check_signals()?;
                    if let Err(e) = self.set_str(var, it) {
                        return self.fail(e);
                    }
                    if self.loop_iter(body, &mut status)? {
                        break;
                    }
                }
                Ok(status)
            }
            Cmd::CFor { init, cond, step, body } => {
                if !init.trim().is_empty() {
                    self.arith(init)?;
                }
                let mut status = 0;
                loop {
                    self.check_signals()?;
                    if !cond.trim().is_empty() && !self.arith(cond)?.truthy() {
                        break;
                    }
                    if self.loop_iter(body, &mut status)? {
                        break;
                    }
                    if !step.trim().is_empty() {
                        self.arith(step)?;
                    }
                }
                Ok(status)
            }
            Cmd::Case { word, arms } => {
                let s = self.expand_word_str(word)?;
                let mut status = 0;
                let mut fall = false;
                for (pats, body, term) in arms {
                    let mut hit = fall;
                    if !hit {
                        for p in pats {
                            let pat = self.expand_pattern(p)?;
                            if crate::glob::matches(&pat, &s) {
                                hit = true;
                                break;
                            }
                        }
                    }
                    if hit {
                        status = self.run(body)?;
                        match term {
                            CaseTerm::Break => break,
                            CaseTerm::Fall => fall = true,
                            CaseTerm::Cont => fall = false,
                        }
                    }
                }
                Ok(status)
            }
            Cmd::FuncDef(f) => {
                self.funcs.insert(f.name.clone(), f.clone());
                Ok(0)
            }
            Cmd::Cond(e) => {
                let b = self.eval_cond(e)?;
                self.after(!b as i32)
            }
            Cmd::Arith(raw) => {
                let n = self.arith(raw)?;
                self.after(!n.truthy() as i32)
            }
            Cmd::Try { body, catch } => {
                let saved_cond = std::mem::replace(&mut self.cond_depth, 0);
                self.try_depth += 1;
                let r = self.run(body);
                self.try_depth -= 1;
                self.cond_depth = saved_cond;
                match r {
                    Err(Flow::Throw(s)) | Err(Flow::Abort(s)) => {
                        self.last_status = s;
                        let _ = self.set_str("ERROR", s.to_string());
                        match catch {
                            Some(c) => self.run(c),
                            None => Ok(s),
                        }
                    }
                    other => other,
                }
            }
            Cmd::Redirected(inner, redirs) => {
                let mut saved = Vec::new();
                let ok = self.apply_redirs(redirs, Some(&mut saved));
                let r = match ok {
                    Ok(true) => self.run(inner),
                    Ok(false) => Ok(1),
                    Err(e) => Err(e),
                };
                self.restore_redirs(saved);
                r
            }
        }
    }

    // ---------- processes ----------

    pub fn fork(&mut self) -> Result<i32, Flow> {
        let pid = sys::fork();
        if pid < 0 {
            return self.fail(format!("fork: {}", sys::last_error()));
        }
        Ok(pid)
    }

    /// Prepare a freshly forked child process.
    pub fn child_setup(&mut self, jc: bool, pgid: i32, fg: bool) {
        let was_interactive = self.interactive;
        self.is_child = true;
        if jc {
            let pid = sys::getpid();
            let pg = if pgid == 0 { pid } else { pgid };
            let _ = sys::setpgid(0, pg);
            if fg {
                let _ = sys::tcsetpgrp(self.tty, pg);
            }
        }
        if was_interactive {
            for s in [libc::SIGINT, libc::SIGQUIT, libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU, libc::SIGTERM, libc::SIGWINCH] {
                sys::default_signal(s);
            }
        }
        for (sig, cmd) in &self.traps {
            if *sig > 0 && !cmd.is_empty() {
                sys::default_signal(*sig);
            }
        }
        self.traps.retain(|_, c| c.is_empty());
        self.interactive = false;
        self.job_control = false;
        self.jobs.clear();
        for d in &mut self.defers {
            d.clear();
        }
    }

    pub fn run_child(&mut self, cmd: &Cmd) -> i32 {
        if let Cmd::Simple(_) = cmd {
            self.exec_direct = true;
        }
        match self.run(cmd) {
            Ok(s) => s,
            Err(Flow::Exit(s)) | Err(Flow::Return(s)) | Err(Flow::Abort(s)) | Err(Flow::Throw(s)) => s,
            Err(Flow::Interrupt) => 130,
            Err(_) => self.last_status,
        }
    }

    pub fn exit_child(&mut self, mut status: i32) -> ! {
        if let Some(t) = self.traps.remove(&0) {
            self.last_status = status;
            if let Err(Flow::Exit(s)) = self.run_trap(&t) {
                status = s;
            }
        }
        sys::_exit(status)
    }

    pub fn add_job(&mut self, pgid: i32, procs: Vec<Proc>, text: &str) -> usize {
        let id = self.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        self.jobs.push(Job { id, pgid, procs, text: text.to_string(), notified: false });
        id
    }

    /// Wait for a foreground job. Returns the exit status of each process.
    pub fn wait_job(&mut self, pgid: i32, pids: Vec<i32>, text: &str) -> Result<Vec<i32>, Flow> {
        let jc = self.job_control && pgid != 0;
        let mut sts = vec![0; pids.len()];
        let mut stopped_at = None;
        let mut sigint = false;
        'outer: for (i, &pid) in pids.iter().enumerate() {
            loop {
                let st = match sys::waitpid_status(pid, if jc { sys::WUNTRACED } else { 0 }) {
                    Some(s) => s,
                    None => {
                        if sys::errno() == sys::EINTR {
                            if let Err(f) = self.check_signals() {
                                return Err(f);
                            }
                            continue;
                        }
                        sts[i] = 127;
                        break;
                    }
                };
                match sys::decode_status(st) {
                    WaitStatus::Exited(c) => sts[i] = c,
                    WaitStatus::Signaled(s) => {
                        sts[i] = 128 + s;
                        if s == libc::SIGINT {
                            sigint = true;
                        } else if i == pids.len() - 1 {
                            let d = sys::sig_description(s);
                            if !d.is_empty() && !self.is_child {
                                err!("{}\n", d);
                            }
                        }
                    }
                    WaitStatus::Stopped => {
                        stopped_at = Some(i);
                        break 'outer;
                    }
                    WaitStatus::Continued => continue,
                }
                break;
            }
        }
        if jc {
            let abnormal = stopped_at.is_some() || sts.iter().any(|s| *s > 128);
            self.take_terminal(abnormal);
        }
        if let Some(idx) = stopped_at {
            let procs = pids.iter().enumerate().map(|(i, &pid)| Proc { pid, status: if i < idx { Some(sts[i]) } else { None }, stopped: i >= idx }).collect();
            let id = self.add_job(pgid, procs, text);
            if let Some(j) = self.jobs.last_mut() {
                j.notified = true;
            }
            err!("\n[{}]+  Stopped                 {}\n", id, text);
            let n = sts.len();
            sts[n - 1] = 128 + libc::SIGTSTP;
            return Ok(sts);
        }
        if sigint && self.interactive {
            err!("\n");
            self.last_status = 130;
            return Err(Flow::Interrupt);
        }
        Ok(sts)
    }

    /// Give the terminal back to the shell and restore its modes.
    pub fn take_terminal(&mut self, restore_modes: bool) {
        unsafe {
            libc::tcsetpgrp(self.tty, self.shell_pgid);
            if restore_modes {
                if let Some(t) = &self.term_modes {
                    libc::tcsetattr(self.tty, libc::TCSADRAIN, t);
                }
            }
        }
    }

    pub fn reap(&mut self) {
        loop {
            let (pid, st) = match sys::waitpid_any(sys::WNOHANG | sys::WUNTRACED | sys::WCONTINUED) {
                Some(x) => x,
                None => break,
            };
            for j in &mut self.jobs {
                for p in &mut j.procs {
                    if p.pid == pid {
                        match sys::decode_status(st) {
                            WaitStatus::Exited(c) => {
                                p.status = Some(c);
                                p.stopped = false;
                            }
                            WaitStatus::Signaled(s) => {
                                p.status = Some(128 + s);
                                p.stopped = false;
                            }
                            WaitStatus::Stopped => {
                                p.stopped = true;
                                j.notified = false;
                            }
                            WaitStatus::Continued => p.stopped = false,
                        }
                    }
                }
            }
        }
    }

    pub fn notify_jobs(&mut self) {
        self.reap();
        if !self.interactive {
            if self.jobs.len() > 256 {
                self.jobs.retain(|j| !j.done());
            }
            return;
        }
        let last = self.jobs.last().map(|j| j.id).unwrap_or(0);
        for j in &mut self.jobs {
            let mark = if j.id == last { '+' } else { ' ' };
            if j.done() {
                let st = j.status();
                let state = if st == 0 {
                    "Done".to_string()
                } else if st > 128 {
                    let d = sys::sig_description(st - 128);
                    if d.is_empty() {
                        format!("Exit {}", st)
                    } else {
                        d.to_string()
                    }
                } else {
                    format!("Exit {}", st)
                };
                err!("[{}]{}  {:<22}  {}\n", j.id, mark, state, j.text);
            } else if j.stopped() && !j.notified {
                err!("[{}]{}  {:<22}  {}\n", j.id, mark, "Stopped", j.text);
                j.notified = true;
            }
        }
        self.jobs.retain(|j| !j.done());
    }

    fn run_subshell(&mut self, body: &Cmd) -> R {
        let jc = self.job_control;
        let text = self.cur_text.clone();
        let pid = self.fork()?;
        if pid == 0 {
            self.child_setup(jc, 0, true);
            let s = self.run_child(body);
            self.exit_child(s);
        }
        if jc {
            let _ = sys::setpgid(pid, pid);
        }
        let sts = self.wait_job(if jc { pid } else { 0 }, vec![pid], &text)?;
        Ok(sts[0])
    }

    fn run_bg(&mut self, cmd: &Cmd, text: &str) -> R {
        let jc = self.job_control;
        let pid = self.fork()?;
        if pid == 0 {
            self.child_setup(jc, 0, false);
            if !jc {
                sys::ignore_signal(libc::SIGINT);
                sys::ignore_signal(libc::SIGQUIT);
                let fd = sys::open("/dev/null", libc::O_RDONLY, 0);
                if fd >= 0 {
                    sys::dup2(fd, 0);
                    sys::close(fd);
                }
            }
            let s = self.run_child(cmd);
            self.exit_child(s);
        }
        if jc {
            let _ = sys::setpgid(pid, pid);
        }
        self.last_bg = pid;
        let id = self.add_job(if jc { pid } else { 0 }, vec![Proc { pid, status: None, stopped: false }], text);
        if self.interactive {
            err!("[{}] {}\n", id, pid);
        }
        Ok(0)
    }

    /// Can the last element of a pipeline run in the current shell (zsh style)?
    fn can_lastpipe(&self, cmd: &Cmd) -> bool {
        match cmd {
            Cmd::Simple(sc) => match sc.words.first().and_then(word_literal) {
                Some(name) => self.funcs.contains_key(&name) || crate::builtins::is_builtin(&name),
                None => false,
            },
            Cmd::Subshell(_) | Cmd::Pipeline { .. } => false,
            Cmd::Redirected(inner, _) => self.can_lastpipe(inner),
            _ => true,
        }
    }

    fn run_pipeline(&mut self, cmds: &[Cmd]) -> R {
        let n = cmds.len();
        let lastpipe = !self.is_child && self.can_lastpipe(&cmds[n - 1]);
        let jc = self.job_control && !lastpipe;
        let text = self.cur_text.clone();
        let mut pgid = 0;
        let mut pids = Vec::new();
        let mut prev_r = -1;
        for (i, c) in cmds.iter().enumerate() {
            let last = i == n - 1;
            if last && lastpipe {
                break;
            }
            let (r, w) = if !last {
                match sys::pipe() {
                    Some(p) => p,
                    None => return self.fail(format!("pipe: {}", sys::last_error())),
                }
            } else {
                (-1, -1)
            };
            let pid = self.fork()?;
            if pid == 0 {
                let was_interactive = self.interactive;
                self.child_setup(jc, pgid, true);
                if lastpipe && was_interactive {
                    sys::ignore_signal(libc::SIGTSTP);
                }
                if prev_r >= 0 {
                    sys::dup2(prev_r, 0);
                    sys::close(prev_r);
                }
                if w >= 0 {
                    sys::dup2(w, 1);
                    sys::close(w);
                    sys::close(r);
                }
                let s = self.run_child(c);
                self.exit_child(s);
            }
            if jc {
                if pgid == 0 {
                    pgid = pid;
                }
                unsafe {
                    libc::setpgid(pid, pgid);
                }
            }
            if prev_r >= 0 {
                sys::close(prev_r);
            }
            if w >= 0 {
                sys::close(w);
            }
            prev_r = r;
            pids.push(pid);
        }
        let mut sts;
        let mut flow = None;
        if lastpipe {
            let saved = sys::save_fd(0);
            sys::dup2(prev_r, 0);
            sys::close(prev_r);
            let r = self.run(&cmds[n - 1]);
            sys::dup2(saved, 0);
            sys::close(saved);
            sts = self.wait_job(0, pids, &text)?;
            match r {
                Ok(s) => sts.push(s),
                Err(e) => {
                    sts.push(self.last_status);
                    flow = Some(e);
                }
            }
        } else {
            sts = self.wait_job(pgid, pids, &text)?;
        }
        let _ = self.set("PIPESTATUS", Val::Arr(sts.iter().map(|s| s.to_string()).collect()));
        if let Some(e) = flow {
            return Err(e);
        }
        let mut status = *sts.last().unwrap();
        if self.opts.pipefail {
            if let Some(s) = sts.iter().rev().find(|s| **s != 0) {
                status = *s;
            }
        }
        Ok(status)
    }

    pub fn cmd_subst(&mut self, cmd: &Cmd) -> Result<String, Flow> {
        // $(< file)
        if let Cmd::Simple(sc) = cmd {
            if sc.words.is_empty() && sc.assigns.is_empty() && sc.redirs.len() == 1 && matches!(sc.redirs[0].kind, RedirKind::In) {
                let path = self.expand_word_str(&sc.redirs[0].target)?;
                return match std::fs::read(&path) {
                    Ok(b) => {
                        self.last_subst = Some(0);
                        Ok(String::from_utf8_lossy(&b).trim_end_matches('\n').replace('\0', ""))
                    }
                    Err(e) => {
                        sherr!("{}: {}", path, sys::strerror(e.raw_os_error().unwrap_or(0)));
                        self.last_subst = Some(1);
                        Ok(String::new())
                    }
                };
            }
        }
        let (r, w) = match sys::pipe() {
            Some(p) => p,
            None => return self.fail(format!("pipe: {}", sys::last_error())),
        };
        let pid = self.fork()?;
        if pid == 0 {
            self.child_setup(false, 0, false);
            sys::close(r);
            sys::dup2(w, 1);
            sys::close(w);
            let s = self.run_child(cmd);
            self.exit_child(s);
        }
        sys::close(w);
        let bytes = sys::read_all(r);
        sys::close(r);
        let st = self.wait_pid(pid);
        self.last_subst = Some(st);
        self.last_status = st;
        let mut s = String::from_utf8_lossy(&bytes).into_owned();
        if s.contains('\0') {
            s = s.replace('\0', "");
        }
        let n = s.trim_end_matches('\n').len();
        s.truncate(n);
        Ok(s)
    }

    pub fn wait_pid(&mut self, pid: i32) -> i32 {
        loop {
            let st = match sys::waitpid_status(pid, 0) {
                Some(s) => s,
                None => {
                    if sys::errno() == sys::EINTR {
                        if let Err(_) = self.check_signals() {
                            // propagate interrupt by returning 130
                            return 130;
                        }
                        continue;
                    }
                    return 127;
                }
            };
            return match sys::decode_status(st) {
                WaitStatus::Exited(c) => c,
                WaitStatus::Signaled(s) => 128 + s,
                _ => continue,
            };
        }
    }

    pub fn proc_subst(&mut self, input: bool, cmd: &Cmd) -> Result<String, Flow> {
        let (r, w) = match sys::pipe() {
            Some(p) => p,
            None => return self.fail(format!("pipe: {}", sys::last_error())),
        };
        let pid = self.fork()?;
        if pid == 0 {
            self.child_setup(false, 0, false);
            if input {
                sys::close(r);
                sys::dup2(w, 1);
                sys::close(w);
            } else {
                sys::close(w);
                sys::dup2(r, 0);
                sys::close(r);
            }
            let s = self.run_child(cmd);
            self.exit_child(s);
        }
        let keep = if input {
            sys::close(w);
            r
        } else {
            sys::close(r);
            w
        };
        self.procsubst_fds.push(keep);
        Ok(format!("/dev/fd/{}", keep))
    }

    // ---------- redirections ----------

    fn heredoc_fd(&mut self, s: &str) -> i32 {
        if s.len() <= 4096 {
            if let Some((r, w)) = sys::pipe() {
                sys::write_fd(w, s.as_bytes());
                sys::close(w);
                return r;
            }
            return -1;
        }
        let dir = self.get("TMPDIR").unwrap_or_else(|| "/tmp".into());
        for n in 0..100 {
            let path = format!("{}/nash-heredoc-{}-{}-{}", dir, sys::getpid(), self.rng % 100000, n);
            let fd = sys::open(&path, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600);
            if fd >= 0 {
                sys::write_fd(fd, s.as_bytes());
                let _ = std::fs::remove_file(&path);
                unsafe {
                    libc::lseek(fd, 0, libc::SEEK_SET);
                }
                return fd;
            }
        }
        -1
    }

    /// Apply redirections. Ok(false) means a redirection failed (already reported).
    pub fn apply_redirs(&mut self, redirs: &[Redir], mut saved: Option<&mut Vec<(i32, i32)>>) -> Result<bool, Flow> {
        let mut save = |fd: i32| {
            if let Some(v) = saved.as_mut() {
                v.push((fd, sys::save_fd(fd)));
            }
        };
        for r in redirs {
            let default_fd = match r.kind {
                RedirKind::In | RedirKind::DupIn | RedirKind::HereStr | RedirKind::HereDoc(_) | RedirKind::RW => 0,
                _ => 1,
            };
            let fd = r.fd.unwrap_or(default_fd);
            let mut both = matches!(r.kind, RedirKind::OutErr | RedirKind::AppendErr);
            let newfd = match &r.kind {
                RedirKind::HereDoc(body) => {
                    let w = body.borrow().clone();
                    let s = self.expand_word_str(&w)?;
                    self.heredoc_fd(&s)
                }
                RedirKind::HereStr => {
                    let mut s = self.expand_word_str(&r.target)?;
                    s.push('\n');
                    self.heredoc_fd(&s)
                }
                RedirKind::DupIn | RedirKind::DupOut => {
                    let t = self.expand_word_str(&r.target)?;
                    if t == "-" {
                        save(fd);
                        sys::close(fd);
                        continue;
                    }
                    if let Ok(n) = t.trim_end_matches('-').parse::<i32>() {
                        if sys::fcntl_getfd(n) < 0 {
                            sherr!("{}: Bad file descriptor", n);
                            return Ok(false);
                        }
                        save(fd);
                        if n != fd {
                            sys::dup2(n, fd);
                        }
                        if t.ends_with('-') {
                            sys::close(n);
                        }
                        continue;
                    }
                    if matches!(r.kind, RedirKind::DupOut) && r.fd.is_none() {
                        both = true;
                        sys::open(&t, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o666)
                    } else {
                        sherr!("{}: ambiguous redirect", t);
                        return Ok(false);
                    }
                }
                kind => {
                    let t = self.expand_word_str(&r.target)?;
                    let flags = match kind {
                        RedirKind::In => libc::O_RDONLY,
                        RedirKind::RW => libc::O_RDWR | libc::O_CREAT,
                        RedirKind::Append | RedirKind::AppendErr => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                        RedirKind::Out if self.opts.noclobber && std::fs::metadata(&t).map(|m| m.is_file()).unwrap_or(false) => {
                            sherr!("{}: cannot overwrite existing file", t);
                            return Ok(false);
                        }
                        _ => libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    };
                    let f = sys::open(&t, flags, 0o666);
                    if f < 0 {
                        sherr!("{}: {}", t, sys::last_error());
                        return Ok(false);
                    }
                    f
                }
            };
            if newfd < 0 {
                sherr!("redirection failed: {}", sys::last_error());
                return Ok(false);
            }
            save(fd);
            if newfd != fd {
                sys::dup2(newfd, fd);
                sys::close(newfd);
            }
            if both {
                save(2);
                sys::dup2(fd, 2);
            }
        }
        Ok(true)
    }

    pub fn restore_redirs(&mut self, saved: Vec<(i32, i32)>) {
        for (fd, s) in saved.into_iter().rev() {
            if s >= 0 {
                sys::dup2(s, fd);
                sys::close(s);
            } else {
                sys::close(fd);
            }
        }
    }

    // ---------- simple commands ----------

    fn exec_simple(&mut self, sc: &SimpleCmd) -> R {
        let direct = std::mem::take(&mut self.exec_direct);
        self.lineno = sc.line;
        self.check_signals()?;
        let mark = self.procsubst_fds.len();
        let r = self.exec_simple2(sc, direct);
        if self.procsubst_fds.len() > mark {
            for fd in self.procsubst_fds.split_off(mark) {
                sys::close(fd);
            }
            self.reap();
        }
        r
    }

    fn xtrace(&mut self, line: &str) {
        let ps4 = self.get("PS4").unwrap_or_else(|| "+ ".into());
        err!("{}{}\n", ps4, line);
    }

    fn exec_simple2(&mut self, sc: &SimpleCmd, direct: bool) -> R {
        self.last_subst = None;
        if let Some(l) = sc.words.first().and_then(word_literal) {
            if DECL.contains(&l.as_str()) && !self.funcs.contains_key(&l) {
                let mut saved = Vec::new();
                let ok = self.apply_redirs(&sc.redirs, Some(&mut saved));
                let r = match ok {
                    Ok(true) => self.exec_decl(&l, sc),
                    Ok(false) => Ok(1),
                    Err(e) => Err(e),
                };
                self.restore_redirs(saved);
                return r;
            }
        }
        let argv = self.expand_words(&sc.words)?;
        if argv.is_empty() {
            let mut saved = Vec::new();
            let ok = self.apply_redirs(&sc.redirs, Some(&mut saved));
            self.restore_redirs(saved);
            if !ok? {
                return Ok(1);
            }
            for a in &sc.assigns {
                self.do_assign(a, false, false)?;
                if self.opts.xtrace {
                    let v = self.get(&a.name).unwrap_or_default();
                    self.xtrace(&format!("{}={}", a.name, shell_quote(&v)));
                }
            }
            return Ok(self.last_subst.unwrap_or(0));
        }
        // Fast path for simple builtin commands without assignments/redirections.
        if sc.assigns.is_empty() && sc.redirs.is_empty() {
            if crate::builtins::is_builtin(&argv[0]) && argv[0] != "exec" {
                if self.opts.xtrace {
                    let line = argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
                    self.xtrace(&line);
                }
                return self.run_builtin(&argv[0], &argv);
            }
        }
        if self.opts.xtrace {
            let line = argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
            self.xtrace(&line);
        }
        if !sc.assigns.is_empty() && sc.assigns.iter().any(|a| a.append) && !argv.is_empty() {
            sherr!("nash: append assignment not allowed as environment prefix");
            return Ok(2);
        }
        self.dispatch(argv, &sc.assigns, &sc.redirs, direct, false)
    }

    /// Run a command given as an argument vector (used by `command`, `defer`, ...).
    pub fn run_argv(&mut self, argv: Vec<String>, skip_funcs: bool) -> R {
        if argv.is_empty() {
            return Ok(0);
        }
        self.dispatch(argv, &[], &[], false, skip_funcs)
    }

    fn with_context(&mut self, assigns: &[Assign], redirs: &[Redir], f: impl FnOnce(&mut Shell) -> R) -> R {
        let mut saved = Vec::new();
        match self.apply_redirs(redirs, Some(&mut saved)) {
            Ok(true) => {}
            Ok(false) => {
                self.restore_redirs(saved);
                return Ok(1);
            }
            Err(e) => {
                self.restore_redirs(saved);
                return Err(e);
            }
        }
        let pushed = !assigns.is_empty();
        if pushed {
            self.scopes.push(HashMap::new());
            for a in assigns {
                if let Err(e) = self.do_assign(a, true, false) {
                    self.scopes.pop();
                    self.restore_redirs(saved);
                    return Err(e);
                }
                if let Some(v) = self.scopes.last_mut().unwrap().get_mut(&a.name) {
                    v.exported = true;
                }
            }
        }
        let r = f(self);
        if pushed {
            self.scopes.pop();
        }
        self.restore_redirs(saved);
        r
    }

    fn dispatch(&mut self, argv: Vec<String>, assigns: &[Assign], redirs: &[Redir], direct: bool, skip_funcs: bool) -> R {
        let name = argv[0].clone();
        if !skip_funcs {
            if let Some(f) = self.funcs.get(&name).cloned() {
                return self.with_context(assigns, redirs, |sh| sh.call_function(&f, &argv[1..]));
            }
        }
        if crate::builtins::is_builtin(&name) {
            if name == "exec" {
                if !self.apply_redirs(redirs, None)? {
                    return Ok(1);
                }
                let args: Vec<String> = argv[1..].iter().filter(|a| *a != "--").cloned().collect();
                if args.is_empty() {
                    return Ok(0);
                }
                let env = self.assign_env(assigns)?;
                return match self.find_command(&args[0]) {
                    Some(path) => {
                        if self.interactive {
                            self.history.flush();
                        }
                        self.do_exec(&path, &args, &env)
                    }
                    None => {
                        sherr!("exec: {}: not found", args[0]);
                        if self.interactive {
                            Ok(127)
                        } else {
                            Err(Flow::Exit(127))
                        }
                    }
                };
            }
            return self.with_context(assigns, redirs, |sh| sh.run_builtin(&name, &argv));
        }
        self.run_external(argv, assigns, redirs, direct)
    }

    fn assign_env(&mut self, assigns: &[Assign]) -> Result<Vec<(String, String)>, Flow> {
        let mut env = Vec::new();
        for a in assigns {
            if let AssignVal::Scalar(w) = &a.value {
                let mut v = self.expand_word_str(w)?;
                if a.append {
                    v = self.get(&a.name).unwrap_or_default() + &v;
                }
                env.push((a.name.clone(), v));
            }
        }
        Ok(env)
    }

    fn run_external(&mut self, argv: Vec<String>, assigns: &[Assign], redirs: &[Redir], direct: bool) -> R {
        let env = self.assign_env(assigns)?;
        let name = &argv[0];
        // nash: `..`, `...` and directory names act as cd
        if self.opts.autocd && argv.len() == 1 && redirs.is_empty() && !direct {
            if name.len() >= 2 && name.chars().all(|c| c == '.') {
                let target = vec![".."; name.len() - 1].join("/");
                return self.run_builtin("cd", &["cd".to_string(), target]);
            }
            if (name.contains('/') || self.find_command(name).is_none()) && sys::is_dir(name) {
                return self.run_builtin("cd", &["cd".to_string(), name.clone()]);
            }
        }
        let path = match env.iter().find(|(k, _)| k == "PATH") {
            Some((_, p)) if !name.contains('/') => p.split(':').map(|d| format!("{}/{}", if d.is_empty() { "." } else { d }, name)).find(|f| sys::is_executable(f)),
            _ => self.find_command(name),
        };
        let path = match path {
            Some(p) => p,
            None => {
                let mut saved = Vec::new();
                let ok = self.apply_redirs(redirs, Some(&mut saved));
                let r = match ok {
                    Ok(true) => self.command_not_found(&argv),
                    Ok(false) => Ok(1),
                    Err(e) => Err(e),
                };
                self.restore_redirs(saved);
                return r;
            }
        };
        if direct {
            match self.apply_redirs(redirs, None) {
                Ok(true) => {}
                _ => self.exit_child(1),
            }
            self.do_exec(&path, &argv, &env);
        }
        let jc = self.job_control;
        let text = if self.cur_text.is_empty() { argv.join(" ") } else { self.cur_text.clone() };
        let pid = self.fork()?;
        if pid == 0 {
            self.child_setup(jc, 0, true);
            match self.apply_redirs(redirs, None) {
                Ok(true) => {}
                _ => sys::_exit(1),
            }
            self.do_exec(&path, &argv, &env);
        }
        if jc {
            let _ = sys::setpgid(pid, pid);
        }
        let sts = self.wait_job(if jc { pid } else { 0 }, vec![pid], &text)?;
        Ok(sts[0])
    }

    fn command_not_found(&mut self, argv: &[String]) -> R {
        if let Some(f) = self.funcs.get("command_not_found_handle").or_else(|| self.funcs.get("command_not_found_handler")).cloned() {
            return self.call_function(&f, argv);
        }
        let name = &argv[0];
        if name.contains('/') {
            sherr!("{}: No such file or directory", name);
            return Ok(127);
        }
        let mut best: Option<(usize, String)> = None;
        if name.chars().count() >= 2 {
            let limit = if name.chars().count() <= 4 { 1 } else { 2 };
            for c in self.command_names() {
                if (c.len() as i64 - name.len() as i64).abs() > limit as i64 {
                    continue;
                }
                let d = edit_distance(name, &c);
                if d <= limit && best.as_ref().map(|(bd, _)| d < *bd).unwrap_or(true) {
                    best = Some((d, c));
                }
            }
        }
        match best {
            Some((_, s)) => sherr!("{}: command not found (did you mean `{}'?)", name, s),
            None => sherr!("{}: command not found", name),
        }
        Ok(127)
    }

    /// Replace the current process with an external program. Never returns.
    pub fn do_exec(&mut self, path: &str, argv: &[String], extra_env: &[(String, String)]) -> ! {
        let mut env = self.environ();
        for (k, v) in extra_env {
            env.insert(k.clone(), v.clone());
        }
        let c = |s: &str| CString::new(s.replace('\0', "")).unwrap();
        let cpath = c(path);
        let cargs: Vec<CString> = argv.iter().map(|a| c(a)).collect();
        let cenv: Vec<CString> = env.iter().map(|(k, v)| c(&format!("{}={}", k, v))).collect();
        let mut pargs: Vec<*const libc::c_char> = cargs.iter().map(|a| a.as_ptr()).collect();
        pargs.push(std::ptr::null());
        let mut penv: Vec<*const libc::c_char> = cenv.iter().map(|a| a.as_ptr()).collect();
        penv.push(std::ptr::null());
        unsafe {
            libc::execve(cpath.as_ptr(), pargs.as_ptr(), penv.as_ptr());
        }
        let e = sys::errno();
        if e == sys::ENOEXEC {
            // a script without a #! line: run it with nash itself
            let me = c(&self.self_exe);
            let mut sargs: Vec<CString> = vec![c("nash"), c(path)];
            sargs.extend(argv[1..].iter().map(|a| c(a)));
            let mut p: Vec<*const libc::c_char> = sargs.iter().map(|a| a.as_ptr()).collect();
            p.push(std::ptr::null());
            unsafe {
                libc::execve(me.as_ptr(), p.as_ptr(), penv.as_ptr());
            }
        }
        if sys::is_dir(path) {
            sherr!("{}: Is a directory", argv[0]);
            sys::_exit(126)
        }
        sherr!("{}: {}", argv[0], sys::strerror(e));
        sys::_exit(if e == sys::ENOENT { 127 } else { 126 })
    }

    // ---------- functions ----------

    pub fn call_function(&mut self, f: &Rc<Func>, args: &[String]) -> R {
        if self.func_depth >= 2000 {
            sherr!("{}: maximum function nesting level exceeded", f.name);
            return Err(Flow::Abort(1));
        }
        self.scopes.push(HashMap::new());
        if let Some(params) = &f.params {
            let usage = || {
                let mut u = f.name.clone();
                for p in params {
                    if p.rest {
                        u.push_str(&format!(" [{}...]", p.name));
                    } else if p.default.is_some() {
                        u.push_str(&format!(" [{}]", p.name));
                    } else {
                        u.push_str(&format!(" <{}>", p.name));
                    }
                }
                u
            };
            let has_rest = params.last().map(|p| p.rest).unwrap_or(false);
            let mut problem = None;
            if !has_rest && args.len() > params.len() {
                problem = Some(format!("too many arguments (expected at most {}, got {})", params.len(), args.len()));
            }
            for (i, p) in params.iter().enumerate() {
                if problem.is_some() {
                    break;
                }
                let val = if p.rest {
                    Val::Arr(args.get(i..).unwrap_or(&[]).to_vec())
                } else if let Some(a) = args.get(i) {
                    Val::Str(a.clone())
                } else if let Some(d) = &p.default {
                    match self.expand_word_str(d) {
                        Ok(s) => Val::Str(s),
                        Err(e) => {
                            self.scopes.pop();
                            return Err(e);
                        }
                    }
                } else {
                    problem = Some(format!("missing argument `{}'", p.name));
                    break;
                };
                let _ = self.set_local(&p.name, val);
            }
            if let Some(msg) = problem {
                self.scopes.pop();
                sherr!("{}: {}\nusage: {}", f.name, msg, usage());
                return Ok(2);
            }
        }
        let saved_pos = std::mem::replace(&mut self.pos_params, args.to_vec());
        let saved_loop = std::mem::take(&mut self.loop_depth);
        self.func_depth += 1;
        self.defers.push(Vec::new());
        self.func_names.push(f.name.clone());
        let r = self.run(&f.body);
        let ds = self.defers.pop().unwrap_or_default();
        let mut r = match r {
            Err(Flow::Return(s)) => Ok(s),
            other => other,
        };
        if !ds.is_empty() {
            let st = self.last_status;
            let cond = std::mem::replace(&mut self.cond_depth, 1);
            for d in ds.into_iter().rev() {
                if let Err(Flow::Exit(s)) = self.run_argv(d, false) {
                    r = Err(Flow::Exit(s));
                    break;
                }
            }
            self.cond_depth = cond;
            self.last_status = st;
        }
        self.func_names.pop();
        self.func_depth -= 1;
        self.loop_depth = saved_loop;
        self.pos_params = saved_pos;
        self.scopes.pop();
        if let Ok(s) = &r {
            self.last_status = *s;
        }
        r
    }

    // ---------- declarations ----------

    pub fn decl_line(&self, name: &str, v: &Var) -> String {
        let mut flags = String::new();
        match v.val {
            Val::Arr(_) => flags.push('a'),
            Val::Map(_) => flags.push('A'),
            _ => {}
        }
        if v.readonly {
            flags.push('r');
        }
        if v.exported {
            flags.push('x');
        }
        if flags.is_empty() {
            flags.push('-');
        }
        let dq = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$").replace('`', "\\`"));
        match &v.val {
            Val::Unset => format!("declare -{} {}", flags, name),
            Val::Str(s) => format!("declare -{} {}={}", flags, name, dq(s)),
            Val::Arr(a) => format!("declare -{} {}=({})", flags, name, a.iter().enumerate().map(|(i, s)| format!("[{}]={}", i, dq(s))).collect::<Vec<_>>().join(" ")),
            Val::Map(m) => format!("declare -{} {}=({})", flags, name, m.iter().map(|(k, s)| format!("[{}]={}", shell_quote(k), dq(s))).collect::<Vec<_>>().join(" ")),
        }
    }

    pub fn all_vars(&self) -> Vec<(String, Var)> {
        let mut map: std::collections::BTreeMap<String, Var> = std::collections::BTreeMap::new();
        for s in &self.scopes {
            for (k, v) in s {
                map.insert(k.clone(), v.clone());
            }
        }
        map.into_iter().collect()
    }

    /// Names of all visible variables, sorted and without cloning their values.
    /// Used by completion, where only the names matter.
    pub fn var_names(&self) -> Vec<String> {
        let mut set = std::collections::BTreeSet::new();
        for s in &self.scopes {
            for (k, v) in s {
                if !matches!(v.val, Val::Unset) {
                    set.insert(k.clone());
                }
            }
        }
        set.into_iter().collect()
    }

    fn exec_decl(&mut self, cmd: &str, sc: &SimpleCmd) -> R {
        enum Item<'a> {
            A(&'a Assign),
            NV(String, String, bool),
            Name(String),
        }
        let mut flags = String::new();
        let mut items: Vec<Item> = Vec::new();
        for w in &sc.words[1..] {
            if let [WordPart::Assign(a)] = &w[..] {
                items.push(Item::A(a));
                continue;
            }
            for s in self.expand_words(std::slice::from_ref(w))? {
                if items.is_empty() && s.len() > 1 && (s.starts_with('-') || s.starts_with('+')) {
                    if s.starts_with('-') && s != "--" {
                        flags.push_str(&s[1..]);
                    }
                } else if let Some(eq) = s.find('=') {
                    let (n, v) = (&s[..eq], &s[eq + 1..]);
                    match n.strip_suffix('+') {
                        Some(n) => items.push(Item::NV(n.to_string(), v.to_string(), true)),
                        None => items.push(Item::NV(n.to_string(), v.to_string(), false)),
                    }
                } else {
                    items.push(Item::Name(s));
                }
            }
        }
        for a in &sc.assigns {
            self.do_assign(a, false, false)?;
        }
        if cmd == "local" && self.func_depth == 0 {
            sherr!("local: can only be used in a function");
            return Ok(1);
        }
        let is_local = cmd == "local" || ((cmd == "declare" || cmd == "typeset") && self.func_depth > 0 && !flags.contains('g'));
        let export = cmd == "export" || flags.contains('x');
        let ro = cmd == "readonly" || flags.contains('r');
        let map = flags.contains('A');
        let arr = flags.contains('a');
        if flags.contains('f') || flags.contains('F') {
            let mut names: Vec<String> = items
                .iter()
                .filter_map(|i| match i {
                    Item::Name(n) => Some(n.clone()),
                    _ => None,
                })
                .collect();
            let all = names.is_empty();
            if all {
                names = self.funcs.keys().cloned().collect();
                names.sort();
            }
            let mut status = 0;
            for n in names {
                match self.funcs.get(&n) {
                    Some(f) => {
                        if flags.contains('F') {
                            out!("{}\n", if all { format!("declare -f {}", n) } else { n.clone() });
                        } else {
                            out!("{}\n", f.text);
                        }
                    }
                    None => status = 1,
                }
            }
            return Ok(status);
        }
        if flags.contains('p') || items.is_empty() {
            let names: Vec<String> = items
                .iter()
                .filter_map(|i| match i {
                    Item::Name(n) => Some(n.clone()),
                    _ => None,
                })
                .collect();
            let mut status = 0;
            if names.is_empty() {
                if cmd == "local" {
                    return Ok(0);
                }
                for (n, v) in self.all_vars() {
                    if matches!(v.val, Val::Unset) && !v.exported {
                        continue;
                    }
                    if (cmd == "export" && !v.exported) || (cmd == "readonly" && !v.readonly) {
                        continue;
                    }
                    if flags.contains('x') && !v.exported || flags.contains('r') && !v.readonly || map && !matches!(v.val, Val::Map(_)) || arr && !matches!(v.val, Val::Arr(_)) {
                        continue;
                    }
                    if cmd == "export" {
                        match &v.val {
                            Val::Str(s) => out!("export {}={}\n", n, shell_quote(s)),
                            _ => out!("export {}\n", n),
                        };
                    } else {
                        out!("{}\n", self.decl_line(&n, &v));
                    }
                }
            } else {
                for n in names {
                    match self.var(&n) {
                        Some(v) => {
                            out!("{}\n", self.decl_line(&n, v));
                        }
                        None => {
                            sherr!("{}: {}: not found", cmd, n);
                            status = 1;
                        }
                    }
                }
            }
            return Ok(status);
        }
        let unexport = cmd == "export" && flags.contains('n');
        let mut status = 0;
        for item in items {
            let name = match item {
                Item::A(a) => {
                    if cmd == "export" && a.append && matches!(a.value, crate::ast::AssignVal::Array(_)) {
                        sherr!("{}: append to array not allowed with export", cmd);
                        status = 1;
                        continue;
                    }
                    self.do_assign(a, is_local, map)?;
                    a.name.clone()
                }
                Item::NV(n, v, append) => {
                    if !crate::parser::is_name(&n) {
                        sherr!("{}: `{}': not a valid identifier", cmd, n);
                        status = 1;
                        continue;
                    }
                    let v = if append { self.get(&n).unwrap_or_default() + &v } else { v };
                    let r = if is_local { self.set_local(&n, Val::Str(v)) } else { self.set_str(&n, v) };
                    if let Err(e) = r {
                        sherr!("{}: {}", cmd, e);
                        status = 1;
                        continue;
                    }
                    n
                }
                Item::Name(n) => {
                    if !crate::parser::is_name(&n) {
                        sherr!("{}: `{}': not a valid identifier", cmd, n);
                        status = 1;
                        continue;
                    }
                    let empty = if map {
                        Some(Val::Map(Vec::new()))
                    } else if arr {
                        Some(Val::Arr(Vec::new()))
                    } else {
                        None
                    };
                    if is_local {
                        if !self.scopes.last().unwrap().contains_key(&n) {
                            let _ = self.set_local(&n, empty.unwrap_or(Val::Unset));
                        }
                    } else if let Some(e) = empty {
                        let ok = matches!((self.var(&n).map(|v| &v.val), &e), (Some(Val::Map(_)), Val::Map(_)) | (Some(Val::Arr(_)), Val::Arr(_)));
                        if !ok {
                            let _ = self.set(&n, e);
                        }
                    }
                    n
                }
            };
            if unexport {
                if let Some(v) = self.var_mut(&name) {
                    v.exported = false;
                }
            } else if export {
                self.export_var(&name);
            }
            if ro {
                if self.var_mut(&name).is_none() {
                    self.scopes[0].insert(name.clone(), Var { val: Val::Unset, exported: false, readonly: true });
                }
                if let Some(v) = self.var_mut(&name) {
                    v.readonly = true;
                }
            }
        }
        Ok(status)
    }
}
