//! Prompt rendering.

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
    if home.len() > 1 && (p == home || p.starts_with(&format!("{}/", home))) {
        format!("~{}", &p[home.len()..])
    } else {
        p.to_string()
    }
}

/// Expand bash-style prompt escapes plus nash's `\g`, `\?` and `\L`.
pub fn expand_prompt(sh: &mut Shell, ps: &str) -> String {
    let cs: Vec<char> = ps.chars().collect();
    let mut out = String::new();
    let now = sys::now_secs();
    // text inserted from dynamic sources must not be expanded again
    let protect = |s: &str| s.replace('\\', "\\\\").replace('$', "\\$").replace('`', "\\`");
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
            'u' => out.push_str(&protect(&sh.get("USER").unwrap_or_default())),
            'h' => out.push_str(&protect(sys::hostname().split('.').next().unwrap_or(""))),
            'H' => out.push_str(&protect(&sys::hostname())),
            'w' => {
                let cwd = sh.cwd();
                out.push_str(&protect(&tilde(sh, &cwd)));
            }
            'W' => {
                let cwd = sh.cwd();
                let t = tilde(sh, &cwd);
                let base = if t == "~" || t == "/" { t.clone() } else { t.rsplit('/').next().unwrap_or("").to_string() };
                out.push_str(&protect(&base));
            }
            '$' => out.push_str(if unsafe { libc::geteuid() } == 0 { "#" } else { "\\$" }),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            'e' => out.push('\x1b'),
            'a' => out.push('\x07'),
            '[' | ']' => {}
            't' => out.push_str(&sys::strftime("%H:%M:%S", now)),
            'T' => out.push_str(&sys::strftime("%I:%M:%S", now)),
            '@' => out.push_str(&sys::strftime("%I:%M %p", now)),
            'A' => out.push_str(&sys::strftime("%H:%M", now)),
            'd' => out.push_str(&sys::strftime("%a %b %d", now)),
            'D' => {
                if cs.get(i) == Some(&'{') {
                    let end = (i..cs.len()).find(|&k| cs[k] == '}').unwrap_or(cs.len());
                    let fmt: String = cs[i + 1..end].iter().collect();
                    out.push_str(&protect(&sys::strftime(if fmt.is_empty() { "%X" } else { &fmt }, now)));
                    i = (end + 1).min(cs.len());
                }
            }
            'j' => out.push_str(&sh.jobs.len().to_string()),
            '!' | '#' => out.push_str(&(sh.history.entries.len() + 1).to_string()),
            's' => out.push_str("nash"),
            'v' | 'V' => out.push_str(env!("CARGO_PKG_VERSION")),
            'g' => {
                let cwd = sh.cwd();
                if let Some(b) = git_branch(sh, &cwd) {
                    out.push_str(&protect(&b));
                }
            }
            '?' => out.push_str(&sh.last_status.to_string()),
            'P' => {
                if let Some(v) = sh.var("PIPESTATUS") {
                    if let Val::Arr(a) = &v.val {
                        out.push_str(&protect(&a.join("|")));
                    }
                }
            }
            'L' => {
                if sh.last_duration_ms >= 1000 {
                    out.push_str(&fmt_duration(sh.last_duration_ms));
                }
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
                    out.push(ch);
                }
            }
            '\\' => out.push_str("\\\\"),
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    if out.contains(['$', '`', '\\']) {
        let saved = sh.last_status;
        let r = sh.expand_string(&out);
        sh.last_status = saved;
        if let Ok(s) = r {
            return s;
        }
    }
    out
}

/// The built-in prompt used when PS1 is not set.
pub fn default_prompt(sh: &mut Shell) -> String {
    let cwd = sh.cwd();
    let mut s = String::new();
    if sh.get("SSH_CONNECTION").is_some() {
        s.push_str(&format!("\x1b[33m{}@{}\x1b[0m ", sh.get("USER").unwrap_or_default(), sys::hostname().split('.').next().unwrap_or("")));
    }
    s.push_str(&format!("\x1b[1;34m{}\x1b[0m", tilde(sh, &cwd)));
    if let Some(b) = git_branch(sh, &cwd) {
        s.push_str(&format!(" \x1b[35m({})\x1b[0m", b));
    }
    if !sh.jobs.is_empty() {
        s.push_str(&format!(" \x1b[36m&{}\x1b[0m", sh.jobs.len()));
    }
    if sh.last_duration_ms >= 2000 {
        s.push_str(&format!(" \x1b[33m{}\x1b[0m", fmt_duration(sh.last_duration_ms)));
    }
    if sh.last_status != 0 {
        s.push_str(&format!(" \x1b[31m[{}]\x1b[0m", sh.last_status));
    }
    let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"].iter().filter_map(|k| sh.get(k)).find(|v| !v.is_empty()).map(|v| v.to_uppercase().replace('-', "").contains("UTF8")).unwrap_or(false);
    let sym = if unsafe { libc::geteuid() } == 0 {
        "#"
    } else if utf8 {
        "❯"
    } else {
        ">"
    };
    s.push_str(&format!(" {}{}\x1b[0m ", if sh.last_status == 0 { "\x1b[32m" } else { "\x1b[31m" }, sym));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
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
