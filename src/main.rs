//! nash - a modern sh-compatible shell.

#[macro_use]
mod sys;
mod arith;
mod ast;
mod builtins;
mod colors;
mod cond;
mod editor;
mod exec;
mod expand;
mod glob;
mod highlight;
mod history;
mod logo;
mod parser;
mod prompt;
mod shell;

use editor::Input;
use shell::{Flow, Shell, R};
use std::time::Instant;

const USAGE: &str = "\
usage: nash [options] [script [args...]]
       nash [options] -c command [name [args...]]

options:
  -c command    run command and exit
  -i            force interactive mode
  -l, --login   act as a login shell (source ~/.profile)
  -s            read commands from standard input
  -e -u -x -f -C -a -v, -o name    set shell options (see `help')
  --norc        do not read ~/.nashrc
  --logo        print the nash terminal logo and exit
  --version     print version and exit
  --help        print this help
";

/// The version of this build: the release it was built for (`0.1.1`, `dev7`;
/// the release workflow sets NASH_RELEASE), else the version of the crate.
pub fn version() -> &'static str {
    match option_env!("NASH_RELEASE") {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// Run exit handlers and terminate.
fn finish(sh: &mut Shell, r: R) -> ! {
    let mut status = match r {
        Ok(s) => s,
        Err(Flow::Exit(s)) | Err(Flow::Abort(s)) | Err(Flow::Return(s)) | Err(Flow::Throw(s)) => s,
        Err(Flow::Interrupt) => 130,
        Err(_) => sh.last_status,
    };
    sh.last_status = status;
    // global defers, then the EXIT trap
    let ds: Vec<Vec<String>> = sh.defers.drain(..).flatten().collect();
    for d in ds.into_iter().rev() {
        if let Err(Flow::Exit(s)) = sh.run_argv(d, false) {
            status = s;
        }
    }
    if let Some(t) = sh.traps.remove(&0) {
        sh.last_status = status;
        if let Err(Flow::Exit(s)) = sh.run_trap(&t) {
            status = s;
        }
    }
    if sh.interactive && !sh.is_child {
        sh.save_last_dir();
    }
    if sh.interactive {
        for j in &sh.jobs {
            if j.pgid > 0 && !j.done() {
                sys::kill(-j.pgid, libc::SIGHUP);
                sys::kill(-j.pgid, libc::SIGCONT);
            }
        }
    }
    std::process::exit(status);
}

/// `!!` and `!$` history expansion for interactive lines.
fn history_expand(sh: &Shell, line: &str) -> Option<String> {
    if !line.contains('!') {
        return None;
    }
    let last = sh.history.entries.last()?.cmd.clone();
    let cs: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut changed = false;
    let mut in_sq = false;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\'' {
            in_sq = !in_sq;
        } else if c == '\\' && !in_sq && i + 1 < cs.len() {
            out.push(c);
            out.push(cs[i + 1]);
            i += 2;
            continue;
        } else if c == '!' && !in_sq && i + 1 < cs.len() {
            if cs[i + 1] == '!' {
                out.push_str(&last);
                changed = true;
                i += 2;
                continue;
            }
            if cs[i + 1] == '$' {
                out.push_str(last.split_whitespace().last().unwrap_or(""));
                changed = true;
                i += 2;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    if changed {
        Some(out)
    } else {
        None
    }
}

fn call_hook(sh: &mut Shell, name: &str, args: &[String]) {
    if let Some(f) = sh.funcs.get(name).cloned() {
        let st = sh.last_status;
        let _ = sh.call_function(&f, args);
        sh.last_status = st;
    }
}

fn interactive_loop(sh: &mut Shell) -> R {
    let mut warned_jobs = false;
    let mut pcache = prompt::PromptCache::new();
    let mut rcache = prompt::PromptCache::new();
    logo::print_startup(sh);
    loop {
        sh.notify_jobs();
        sys::take_pending(libc::SIGINT);
        if let Some(pc) = sh.get("PROMPT_COMMAND") {
            let st = sh.last_status;
            let _ = sh.run_source(&pc, "PROMPT_COMMAND");
            sh.last_status = st;
        }
        call_hook(sh, "precmd", &[]);
        let prompt = pcache.render(sh, &["PS1"], prompt::DEFAULT_PS1);
        let rprompt = rcache.render(sh, &["RPROMPT", "RPS1"], "");
        prompt::report_cwd(sh);
        if sh.job_control {
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(sh.tty, &mut t) == 0 {
                    sh.term_modes = Some(t);
                }
            }
        }
        let mut line = match editor::read_line(sh, prompt, rprompt) {
            Input::Eof => {
                if sh.jobs.iter().any(|j| j.stopped()) && !warned_jobs {
                    err!("\nnash: there are stopped jobs\n");
                    warned_jobs = true;
                    continue;
                }
                err!("exit\n");
                return Ok(sh.last_status);
            }
            Input::Interrupt => {
                // Ctrl+C at the prompt: fish cancels the command line without
                // changing the last exit status, so just re-prompt.
                continue;
            }
            Input::Line(l) => l,
        };
        warned_jobs = false;
        // without the line editor, gather continuation lines by hand
        if !sys::isatty(0) {
            loop {
                match parser::check_complete(&line, &sh.aliases) {
                    Err(e) if e.incomplete => {
                        let ps2 = sh.get("PS2").unwrap_or_default();
                        match editor::read_plain(&ps2) {
                            Input::Line(more) => {
                                line.push('\n');
                                line.push_str(&more);
                            }
                            _ => break,
                        }
                    }
                    _ => break,
                }
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(expanded) = history_expand(sh, &line) {
            err!("{}\n", expanded);
            line = expanded;
        }
        call_hook(sh, "preexec", &[line.clone()]);
        let cwd = sh.cwd();
        let t0 = Instant::now();
        let ts = sys::now_secs();
        let r = sh.run_source(&line, "nash");
        let dur = t0.elapsed().as_millis() as u64;
        let mut exit = None;
        match r {
            Ok(_) => {}
            Err(Flow::Exit(s)) => exit = Some(s),
            Err(Flow::Interrupt) => sh.last_status = 130,
            Err(Flow::Abort(s)) | Err(Flow::Throw(s)) | Err(Flow::Return(s)) => sh.last_status = s,
            Err(_) => {}
        }
        sh.cond_depth = 0;
        sh.loop_depth = 0;
        sh.last_duration_ms = dur;
        if !line.starts_with(' ') {
            sh.history.add(history::Entry::new(line.trim_end().to_string(), cwd, ts, exit.unwrap_or(sh.last_status), dur));
        }
        if let Some(s) = exit {
            return Err(Flow::Exit(s));
        }
    }
}

fn setup_interactive(sh: &mut Shell, norc: bool, login: bool, restore_dir: bool) {
    sh.interactive = true;
    sh.opts.autocd = true;
    // Expose the built-in prompt as a normal PS1 string, like bash/zsh do.
    // Do this before sourcing the rc files so they can inspect or replace it.
    prompt::install_default_ps1(sh);
    let tty = sys::open("/dev/tty", libc::O_RDWR | libc::O_CLOEXEC, 0);
    let tty = if tty >= 0 {
        let hi = sys::save_fd(tty);
        sys::close(tty);
        hi
    } else {
        sys::save_fd(0)
    };
    sh.tty = tty;
    for s in [libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU, libc::SIGQUIT, libc::SIGTERM] {
        sys::ignore_signal(s);
    }
    sys::catch_signal(libc::SIGINT);
    sys::catch_signal(libc::SIGWINCH);
    if tty >= 0 && sys::isatty(tty) {
        unsafe {
            let pid = libc::getpid();
            if libc::getpgrp() != pid {
                libc::setpgid(0, pid);
            }
            sh.shell_pgid = libc::getpgrp();
            libc::tcsetpgrp(tty, sh.shell_pgid);
        }
        sh.job_control = true;
    }
    let hist = match sh.get("NASH_HISTFILE") {
        Some(h) => h,
        None => format!("{}/history", sh.data_dir()),
    };
    sh.history.load(&hist);
    colors::setup(sh);
    let home = sh.home();
    if login {
        for f in ["/etc/profile".to_string(), format!("{}/.profile", home)] {
            if std::path::Path::new(&f).is_file() {
                let _ = sh.source_file(&f, &[]);
            }
        }
    }
    if !norc {
        let rc = format!("{}/.nashrc", home);
        if std::path::Path::new(&rc).is_file() {
            if let Err(Flow::Exit(s)) = sh.source_file(&rc, &[]) {
                std::process::exit(s);
            }
        }
    }
    // Start where the previous session left off, unless the terminal was
    // deliberately opened somewhere other than the home directory.
    if restore_dir && sh.get("NASH_RESTORE_DIR").as_deref() != Some("0") && sh.cwd() == home {
        let file = format!("{}/last_dir", sh.data_dir());
        if let Ok(d) = std::fs::read_to_string(file) {
            let d = d.trim_end_matches('\n').to_string();
            if d != home && sys::is_dir(&d) {
                let _ = sh.run_builtin("cd", &["cd".to_string(), d]);
                sh.last_status = 0;
            }
        }
    }
}

fn main() {
    // Rust ignores SIGPIPE by default; a shell must not.
    sys::default_signal(libc::SIGPIPE);
    let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let mut sh = Shell::new();
    let mut command: Option<String> = None;
    let mut force_interactive = false;
    let mut login = args.first().map(|a| a.starts_with('-')).unwrap_or(false);
    let mut norc = false;
    let mut from_stdin = false;
    let mut i = 1;
    let mut want_command = false;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--logo" => {
                logo::print_cli(version());
                return;
            }
            "--version" => {
                out!("nash {}\n", version());
                return;
            }
            "--help" => {
                out!("{}", USAGE);
                return;
            }
            "--norc" => norc = true,
            "--login" => login = true,
            "--" => {
                i += 1;
                break;
            }
            _ if (a.starts_with('-') || a.starts_with('+')) && a.len() > 1 && !a.starts_with("--") => {
                let on = a.starts_with('-');
                for c in a[1..].chars() {
                    match c {
                        'c' => want_command = true,
                        'i' => force_interactive = true,
                        'l' => login = true,
                        's' => from_stdin = true,
                        'e' => sh.opts.errexit = on,
                        'u' => sh.opts.nounset = on,
                        'x' => sh.opts.xtrace = on,
                        'f' => sh.opts.noglob = on,
                        'C' => sh.opts.noclobber = on,
                        'a' => sh.opts.allexport = on,
                        'v' => sh.opts.verbose = on,
                        'o' => {
                            i += 1;
                            match args.get(i) {
                                Some(n) if sh.opts.set(n, on) => {}
                                other => {
                                    sherr!("{}: invalid option name", other.cloned().unwrap_or_default());
                                    std::process::exit(2);
                                }
                            }
                        }
                        _ => {
                            sherr!("-{}: invalid option\n{}", c, USAGE);
                            std::process::exit(2);
                        }
                    }
                }
            }
            _ => break,
        }
        i += 1;
        if want_command {
            break;
        }
    }
    if want_command {
        match args.get(i) {
            Some(c) => {
                command = Some(c.clone());
                i += 1;
            }
            None => {
                sherr!("-c: option requires an argument");
                std::process::exit(2);
            }
        }
    }
    sh.login = login;
    let rest = &args[i.min(args.len())..];

    if let Some(cmd) = command {
        if let Some(n) = rest.first() {
            sh.arg0 = n.clone();
        }
        sh.pos_params = rest.iter().skip(1).cloned().collect();
        if force_interactive {
            setup_interactive(&mut sh, norc, login, false);
        }
        let r = sh.run_source(&cmd, "nash");
        finish(&mut sh, r);
    }
    if let (Some(file), false) = (rest.first(), from_stdin) {
        sh.arg0 = file.clone();
        sh.script_name = file.clone();
        sh.pos_params = rest[1..].to_vec();
        let src = match std::fs::read(file) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                sherr!("{}: {}", file, sys::strerror(e.raw_os_error().unwrap_or(0)));
                std::process::exit(127);
            }
        };
        let r = sh.run_source(&src, file);
        finish(&mut sh, r);
    }
    sh.pos_params = rest.to_vec();
    if force_interactive || (sys::isatty(0) && sys::isatty(2)) {
        setup_interactive(&mut sh, norc, login, true);
        let r = interactive_loop(&mut sh);
        finish(&mut sh, r);
    }
    let src = String::from_utf8_lossy(&sys::read_all(0)).into_owned();
    let r = sh.run_source(&src, "nash");
    finish(&mut sh, r);
}
