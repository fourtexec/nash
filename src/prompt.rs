//! Prompt rendering.

use std::fmt::Write as _;

use crate::builtins::fmt_duration;
use crate::shell::{Shell, Val};
use crate::sys;

pub fn char_width(c: char) -> usize {
    let u = c as u32;
    if u < 0x20 || (0x7f..0xa0).contains(&u) {
        return 0;
    }
    if (0x300..=0x36f).contains(&u) || (0x200b..=0x200f).contains(&u) || (0xfe00..=0xfe0f).contains(&u) {
        return 0;
    }
    if (0x1100..=0x115f).contains(&u)
        || (0x2e80..=0xa4cf).contains(&u)
        || (0xac00..=0xd7a3).contains(&u)
        || (0xf900..=0xfaff).contains(&u)
        || (0xfe30..=0xfe4f).contains(&u)
        || (0xff00..=0xff60).contains(&u)
        || (0xffe0..=0xffe6).contains(&u)
        || (0x1f300..=0x1f64f).contains(&u)
        || (0x1f900..=0x1f9ff).contains(&u)
        || (0x20000..=0x3fffd).contains(&u)
    {
        return 2;
    }
    1
}

/// Display width of a string, ignoring ANSI escape sequences.
pub fn visible_width(s: &str) -> usize {
    let cs: Vec<char> = s.chars().collect();
    let mut w = 0;
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '\x1b' {
            i += 1;
            match cs.get(i) {
                Some('[') => {
                    i += 1;
                    while i < cs.len() && !('\x40'..='\x7e').contains(&cs[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                Some(']') => {
                    while i < cs.len() && cs[i] != '\x07' && !(cs[i] == '\x1b' && cs.get(i + 1) == Some(&'\\')) {
                        i += 1;
                    }
                    i += if cs.get(i) == Some(&'\x1b') { 2 } else { 1 };
                }
                _ => i += 1,
            }
            continue;
        }
        w += char_width(cs[i]);
        i += 1;
    }
    w
}

/// Find the git directory (the one holding `HEAD`) by walking up from `cwd`.
fn find_git_dir(cwd: &str) -> Option<String> {
    let mut dir = std::path::PathBuf::from(cwd);
    loop {
        let dotgit = dir.join(".git");
        if let Ok(meta) = std::fs::metadata(&dotgit) {
            if meta.is_dir() {
                return Some(dotgit.to_string_lossy().into_owned());
            }
            // Worktrees and submodules use a `.git` file with "gitdir: <path>".
            let content = std::fs::read_to_string(&dotgit).ok()?;
            let gd = content.trim().strip_prefix("gitdir: ")?;
            let p = std::path::PathBuf::from(gd);
            let abs = if p.is_absolute() { p } else { dir.join(p) };
            return Some(abs.to_string_lossy().into_owned());
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Current git branch, or a short commit hash when detached. The repository
/// lookup is cached per cwd (and `GIT_DIR`), so repeating the prompt in the same
/// directory only reads `HEAD` instead of walking the tree.
pub fn git_branch(sh: &mut Shell, cwd: &str) -> Option<String> {
    let gd_env = sh.get("GIT_DIR").filter(|s| !s.is_empty());
    let hit = match &sh.git_cache {
        Some((c, g, Some(_))) if c == cwd && *g == gd_env => true,
        Some((c, g, None)) if c == cwd && *g == gd_env => {
            // Negative result: trust it only while `cwd` still has no `.git`, so
            // a fresh `git init` is picked up on the next prompt.
            gd_env.is_none() && !std::path::Path::new(cwd).join(".git").exists()
        }
        _ => false,
    };
    if !hit {
        let d = match &gd_env {
            Some(gd) => {
                let p = std::path::PathBuf::from(gd);
                let abs = if p.is_absolute() { p } else { std::path::PathBuf::from(cwd).join(p) };
                Some(abs.to_string_lossy().into_owned())
            }
            None => find_git_dir(cwd),
        };
        sh.git_cache = Some((cwd.to_string(), gd_env, d));
    }
    let dir = sh.git_cache.as_ref().and_then(|(_, _, d)| d.as_deref())?;
    let head = match std::fs::read_to_string(std::path::Path::new(dir).join("HEAD")) {
        Ok(h) => h,
        Err(_) => {
            // Repository moved or disappeared; force a fresh lookup next time.
            sh.git_cache = None;
            return None;
        }
    };
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(b) => b.to_string(),
        None => head.chars().take(7).collect(),
    })
}

fn tilde(sh: &mut Shell, p: &str) -> String {
    let home = sh.home();
    if home.len() > 1
        && (p == home || (p.starts_with(home.as_str()) && p.as_bytes().get(home.len()) == Some(&b'/')))
    {
        let mut s = String::with_capacity(p.len() - home.len() + 1);
        s.push('~');
        s.push_str(&p[home.len()..]);
        s
    } else {
        p.to_string()
    }
}

/// The built-in prompt, written with nash's segment escapes. Keeping the
/// default in `PS1` form means it can be customized the same way as in other
/// shells: `$PS1` is visible, editable and recompiled only when it changes.
pub const DEFAULT_PS1: &str = "\\U\\C\\G\\J\\M\\Q\\Z";

#[derive(Clone, Copy)]
enum TimeKind {
    Hms,
    Hms12,
    AmPm,
    Hm,
    Date,
    Custom,
}

#[derive(Clone)]
enum Seg {
    Lit(String),
    User,
    HostShort,
    HostFull,
    Cwd,
    CwdBase,
    Jobs,
    History,
    Git,
    Status,
    PipeStatus,
    Duration,
    Time(TimeKind, Option<String>),
    UserHost,
    CwdStyled,
    GitStyled,
    JobsStyled,
    DurationStyled,
    StatusStyled,
    SymbolStyled,
}

/// A parsed prompt. Compiling once and rendering it on every prompt avoids
/// re-scanning the escape string (and re-allocating protected values) each time.
pub struct PromptPlan {
    segs: Vec<Seg>,
    expand: bool,
}

fn flush(segs: &mut Vec<Seg>, lit: &mut String) {
    if !lit.is_empty() {
        segs.push(Seg::Lit(std::mem::take(lit)));
    }
}

impl PromptPlan {
    fn compile(ps: &str) -> PromptPlan {
        let cs: Vec<char> = ps.chars().collect();
        let mut segs = Vec::with_capacity(8);
        let mut lit = String::new();
        let mut expand = false;
        let is_root = unsafe { libc::geteuid() } == 0;
        let mut i = 0;
        while i < cs.len() {
            if cs[i] != '\\' || i + 1 >= cs.len() {
                lit.push(cs[i]);
                if matches!(cs[i], '$' | '`' | '\\') {
                    expand = true;
                }
                i += 1;
                continue;
            }
            i += 1;
            let c = cs[i];
            i += 1;
            match c {
                'u' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::User);
                }
                'h' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::HostShort);
                }
                'H' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::HostFull);
                }
                'w' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Cwd);
                }
                'W' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::CwdBase);
                }
                '$' => {
                    if is_root {
                        lit.push('#');
                    } else {
                        lit.push_str("\\$");
                        expand = true;
                    }
                }
                'n' => lit.push('\n'),
                'r' => lit.push('\r'),
                'e' => lit.push('\x1b'),
                'a' => lit.push('\x07'),
                '[' | ']' => {}
                't' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Time(TimeKind::Hms, None));
                }
                'T' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Time(TimeKind::Hms12, None));
                }
                '@' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Time(TimeKind::AmPm, None));
                }
                'A' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Time(TimeKind::Hm, None));
                }
                'd' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Time(TimeKind::Date, None));
                }
                'D' => {
                    // `\D` without `{...}` expands to nothing, like the old
                    // implementation.
                    if cs.get(i) == Some(&'{') {
                        let end = (i..cs.len()).find(|&k| cs[k] == '}').unwrap_or(cs.len());
                        let mut fmt: String = cs[i + 1..end].iter().collect();
                        if fmt.is_empty() {
                            fmt.push_str("%X");
                        }
                        i = (end + 1).min(cs.len());
                        flush(&mut segs, &mut lit);
                        segs.push(Seg::Time(TimeKind::Custom, Some(fmt)));
                    }
                }
                'j' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Jobs);
                }
                '!' | '#' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::History);
                }
                's' => lit.push_str("nash"),
                'v' | 'V' => lit.push_str(env!("CARGO_PKG_VERSION")),
                'g' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Git);
                }
                '?' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Status);
                }
                'P' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::PipeStatus);
                }
                'L' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::Duration);
                }
                'U' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::UserHost);
                }
                'C' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::CwdStyled);
                }
                'G' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::GitStyled);
                }
                'J' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::JobsStyled);
                }
                'M' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::DurationStyled);
                }
                'Q' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::StatusStyled);
                }
                'Z' => {
                    flush(&mut segs, &mut lit);
                    segs.push(Seg::SymbolStyled);
                }
                '0'..='7' => {
                    let mut v = c.to_digit(8).unwrap();
                    let mut n = 1;
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
                        lit.push(ch);
                        if matches!(ch, '$' | '`' | '\\') {
                            expand = true;
                        }
                    }
                }
                '\\' => {
                    lit.push_str("\\\\");
                    expand = true;
                }
                other => {
                    lit.push('\\');
                    lit.push(other);
                    expand = true;
                }
            }
        }
        flush(&mut segs, &mut lit);
        PromptPlan { segs, expand }
    }
}

/// Append `s`, escaping prompt-expansion metacharacters when the surrounding
/// prompt is going to be run through `expand_string`. This avoids the three
/// temporary strings the old `protect` closure allocated per dynamic segment.
fn push_protected(out: &mut String, s: &str, protect: bool) {
    if protect && s.bytes().any(|b| b == b'\\' || b == b'$' || b == b'`') {
        for c in s.chars() {
            if c == '\\' || c == '$' || c == '`' {
                out.push('\\');
            }
            out.push(c);
        }
    } else {
        out.push_str(s);
    }
}

fn has_utf8(v: &str) -> bool {
    let b = v.as_bytes();
    b.windows(4).any(|w| w.eq_ignore_ascii_case(b"utf8"))
        || b.windows(5).any(|w| w.eq_ignore_ascii_case(b"utf-8"))
}

fn utf8_locale(sh: &Shell) -> bool {
    for k in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(v) = sh.var(k) {
            if let Val::Str(s) = &v.val {
                if !s.is_empty() {
                    return has_utf8(s);
                }
            }
        }
    }
    false
}

fn push_userhost(sh: &mut Shell, out: &mut String, protect: bool) {
    if sh.var("SSH_CONNECTION").is_none() {
        return;
    }
    let user = sh.get("USER").unwrap_or_default();
    let host = sh.hostname();
    out.push_str("\x1b[33m");
    push_protected(out, &user, protect);
    out.push('@');
    push_protected(out, host.split('.').next().unwrap_or(""), protect);
    out.push_str("\x1b[0m ");
}

fn push_cwd_seg(sh: &mut Shell, cwd: &str, out: &mut String, protect: bool) {
    let t = tilde(sh, cwd);
    out.push_str("\x1b[1;34m");
    push_protected(out, &t, protect);
    out.push_str("\x1b[0m");
}

fn push_git_seg(sh: &mut Shell, cwd: &str, out: &mut String, protect: bool) {
    if let Some(b) = git_branch(sh, cwd) {
        out.push_str(" \x1b[35m(");
        push_protected(out, &b, protect);
        out.push_str(")\x1b[0m");
    }
}

fn push_jobs_seg(sh: &Shell, out: &mut String) {
    if !sh.jobs.is_empty() {
        out.push_str(" \x1b[36m&");
        let _ = write!(out, "{}", sh.jobs.len());
        out.push_str("\x1b[0m");
    }
}

fn push_duration_seg(sh: &Shell, out: &mut String, protect: bool) {
    if sh.last_duration_ms >= 2000 {
        let d = fmt_duration(sh.last_duration_ms);
        out.push_str(" \x1b[33m");
        push_protected(out, &d, protect);
        out.push_str("\x1b[0m");
    }
}

fn push_status_seg(sh: &Shell, out: &mut String) {
    if sh.last_status != 0 {
        out.push_str(" \x1b[31m[");
        let _ = write!(out, "{}", sh.last_status);
        out.push_str("]\x1b[0m");
    }
}

fn push_symbol_seg(sh: &mut Shell, out: &mut String) {
    let sym = if unsafe { libc::geteuid() } == 0 {
        "#"
    } else if utf8_locale(sh) {
        "❯"
    } else {
        ">"
    };
    out.push(' ');
    out.push_str(if sh.last_status == 0 { "\x1b[32m" } else { "\x1b[31m" });
    out.push_str(sym);
    out.push_str("\x1b[0m ");
}

fn render_plan(sh: &mut Shell, plan: &PromptPlan, out: &mut String) {
    let mut now: Option<i64> = None;
    // `\C` and `\G` (and `\w` plus `\g`) all need the cwd; fetch PWD once.
    let mut cwd_cache: Option<String> = None;
    for seg in &plan.segs {
        match seg {
            Seg::Lit(s) => out.push_str(s),
            Seg::User => {
                let u = sh.get("USER").unwrap_or_default();
                push_protected(out, &u, plan.expand);
            }
            Seg::HostShort => {
                let host = sh.hostname();
                push_protected(out, host.split('.').next().unwrap_or(""), plan.expand);
            }
            Seg::HostFull => {
                let host = sh.hostname();
                push_protected(out, host, plan.expand);
            }
            Seg::Cwd => {
                let cwd = cwd_cache.get_or_insert_with(|| sh.cwd());
                let t = tilde(sh, cwd);
                push_protected(out, &t, plan.expand);
            }
            Seg::CwdBase => {
                let cwd = cwd_cache.get_or_insert_with(|| sh.cwd());
                let t = tilde(sh, cwd);
                let base = if t == "~" || t == "/" {
                    t
                } else {
                    t.rsplit('/').next().unwrap_or("").to_string()
                };
                push_protected(out, &base, plan.expand);
            }
            Seg::Jobs => {
                let _ = write!(out, "{}", sh.jobs.len());
            }
            Seg::History => {
                let _ = write!(out, "{}", sh.history.entries.len() + 1);
            }
            Seg::Git => {
                let cwd = cwd_cache.get_or_insert_with(|| sh.cwd());
                if let Some(b) = git_branch(sh, cwd) {
                    push_protected(out, &b, plan.expand);
                }
            }
            Seg::Status => {
                let _ = write!(out, "{}", sh.last_status);
            }
            Seg::PipeStatus => {
                if let Some(v) = sh.var("PIPESTATUS") {
                    if let Val::Arr(a) = &v.val {
                        for (i, s) in a.iter().enumerate() {
                            if i > 0 {
                                out.push('|');
                            }
                            push_protected(out, s, plan.expand);
                        }
                    }
                }
            }
            Seg::Duration => {
                if sh.last_duration_ms >= 1000 {
                    let d = fmt_duration(sh.last_duration_ms);
                    push_protected(out, &d, plan.expand);
                }
            }
            Seg::Time(kind, fmt) => {
                let t = *now.get_or_insert_with(sys::now_secs);
                let s = match kind {
                    TimeKind::Hms => sys::strftime("%H:%M:%S", t),
                    TimeKind::Hms12 => sys::strftime("%I:%M:%S", t),
                    TimeKind::AmPm => sys::strftime("%I:%M %p", t),
                    TimeKind::Hm => sys::strftime("%H:%M", t),
                    TimeKind::Date => sys::strftime("%a %b %d", t),
                    TimeKind::Custom => sys::strftime(fmt.as_deref().unwrap_or("%X"), t),
                };
                push_protected(out, &s, plan.expand);
            }
            Seg::UserHost => push_userhost(sh, out, plan.expand),
            Seg::CwdStyled => {
                let cwd = cwd_cache.get_or_insert_with(|| sh.cwd());
                push_cwd_seg(sh, cwd, out, plan.expand);
            }
            Seg::GitStyled => {
                let cwd = cwd_cache.get_or_insert_with(|| sh.cwd());
                push_git_seg(sh, cwd, out, plan.expand);
            }
            Seg::JobsStyled => push_jobs_seg(sh, out),
            Seg::DurationStyled => push_duration_seg(sh, out, plan.expand),
            Seg::StatusStyled => push_status_seg(sh, out),
            Seg::SymbolStyled => push_symbol_seg(sh, out),
        }
    }
    if plan.expand {
        let saved = sh.last_status;
        if let Ok(s) = sh.expand_string(out) {
            out.clear();
            out.push_str(&s);
        }
        sh.last_status = saved;
    }
}

/// Expand bash-style prompt escapes plus nash's segment escapes.
pub fn expand_prompt(sh: &mut Shell, ps: &str) -> String {
    let plan = PromptPlan::compile(ps);
    let mut out = String::with_capacity(ps.len() + 16);
    render_plan(sh, &plan, &mut out);
    out
}

/// Install the built-in prompt as a normal, non-exported `PS1` variable.
/// Called once for interactive shells, before `~/.nashrc` is sourced.
pub fn install_default_ps1(sh: &mut Shell) {
    if sh.get("PS1").is_none() {
        let _ = sh.set_str("PS1", DEFAULT_PS1);
    }
}

/// The built-in prompt used when `PS1` is unset.
#[allow(dead_code)]
pub fn default_prompt(sh: &mut Shell) -> String {
    expand_prompt(sh, DEFAULT_PS1)
}

/// Caches a compiled prompt, keyed by the source string. `PS1`, `RPROMPT` and
/// `RPS1` can change at any time (rc files, `precmd`, scripts), so the cache
/// compares the current value and recompiles only when it actually differs.
pub struct PromptCache {
    src: String,
    plan: PromptPlan,
    out: String,
}

impl PromptCache {
    pub fn new() -> PromptCache {
        PromptCache {
            src: String::new(),
            plan: PromptPlan::compile(""),
            out: String::new(),
        }
    }

    /// Render the first set variable from `vars`, or `default` when none is set.
    /// The returned slice is valid until the next call on this cache.
    pub fn render(&mut self, sh: &mut Shell, vars: &[&str], default: &str) -> &str {
        let current = vars.iter().find_map(|v| match sh.var(v) {
            Some(var) => match &var.val {
                Val::Str(s) => Some(s.as_str()),
                _ => None,
            },
            None => None,
        });
        let src = current.unwrap_or(default);
        if src != self.src {
            self.src.clear();
            self.src.push_str(src);
            self.plan = PromptPlan::compile(&self.src);
        }
        self.out.clear();
        render_plan(sh, &self.plan, &mut self.out);
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_recompiles_only_on_change() {
        let mut sh = Shell::new();
        let _ = sh.set_str("PS1", "A\\?");
        let mut c = PromptCache::new();
        sh.last_status = 1;
        assert_eq!(c.render(&mut sh, &["PS1"], DEFAULT_PS1), "A1");
        sh.last_status = 2;
        assert_eq!(c.render(&mut sh, &["PS1"], DEFAULT_PS1), "A2");
        let _ = sh.set_str("PS1", "B\\?");
        assert_eq!(c.render(&mut sh, &["PS1"], DEFAULT_PS1), "B2");
        let _ = sh.unset_var("PS1");
        assert!(!c.render(&mut sh, &["PS1"], DEFAULT_PS1).is_empty());
    }

    #[test]
    fn segment_escapes_use_shell_state() {
        let mut sh = Shell::new();
        let _ = sh.set_str("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22");
        sh.last_status = 3;
        sh.last_duration_ms = 2500;
        let s = expand_prompt(&mut sh, "\\U\\Q\\M\\Z");
        let user = sh.get("USER").unwrap_or_default();
        assert!(s.contains(&format!("{}@", user)));
        assert!(s.contains("[3]"));
        assert!(s.contains(&fmt_duration(2500)));
        assert!(s.ends_with(' '));
        sh.last_status = 0;
        assert!(expand_prompt(&mut sh, "\\Q").is_empty());
    }

    #[test]
    fn dynamic_values_are_not_expanded() {
        let mut sh = Shell::new();
        let _ = sh.set_str("HOME", "/home/test");
        let dir = std::env::temp_dir().join(format!("nash-$-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = sh.set_str("PWD", dir.to_string_lossy().into_owned());
        let s = expand_prompt(&mut sh, "\\w $HOME");
        assert!(s.contains("$-"));
        assert!(s.ends_with("/home/test"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn octal_dollar_still_expands() {
        let mut sh = Shell::new();
        let _ = sh.set_str("HOME", "/home/test");
        assert_eq!(expand_prompt(&mut sh, "\\044HOME"), "/home/test");
    }

    #[test]
    fn default_prompt_is_ps1() {
        let mut sh = Shell::new();
        sh.last_status = 0;
        sh.last_duration_ms = 0;
        assert_eq!(expand_prompt(&mut sh, DEFAULT_PS1), default_prompt(&mut sh));
        let _ = sh.unset_var("PS1");
        install_default_ps1(&mut sh);
        assert_eq!(sh.get("PS1").as_deref(), Some(DEFAULT_PS1));
    }

    #[test]
    fn git_init_after_negative_cache() {
        let base = std::env::temp_dir().join(format!("nash-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cwd = base.to_str().unwrap().to_string();
        let mut sh = Shell::new();
        assert_eq!(git_branch(&mut sh, &cwd), None, "empty dir");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::write(base.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(git_branch(&mut sh, &cwd).as_deref(), Some("main"), "after init");
        let _ = std::fs::remove_dir_all(&base);
    }
}
