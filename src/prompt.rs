//! Prompt rendering.

use std::fmt::Write as _;
use std::os::unix::fs::MetadataExt;
use std::rc::Rc;

use crate::builtins::fmt_duration;
use crate::shell::{GitBranchCache, GitCache, Shell, Val};
use crate::sys;

pub fn char_width(c: char) -> usize {
    let u = c as u32;
    if u < 0x20 || (0x7f..0xa0).contains(&u) {
        return 0;
    }
    // Zero-width format characters, combining marks, variation selectors, and Hangul Jamo vowels/finals.
    if u == 0x00ad
        || (0x0300..=0x036f).contains(&u)
        || (0x0483..=0x0489).contains(&u)
        || (0x0591..=0x05bd).contains(&u)
        || u == 0x05bf
        || (0x05c1..=0x05c2).contains(&u)
        || (0x05c4..=0x05c5).contains(&u)
        || u == 0x05c7
        || (0x0610..=0x061a).contains(&u)
        || (0x064b..=0x065f).contains(&u)
        || u == 0x0670
        || (0x06d6..=0x06dc).contains(&u)
        || (0x06df..=0x06e4).contains(&u)
        || (0x06e7..=0x06e8).contains(&u)
        || (0x06ea..=0x06ed).contains(&u)
        || (0x0730..=0x074a).contains(&u)
        || (0x0901..=0x0902).contains(&u)
        || (0x093c..=0x093c).contains(&u)
        || (0x0941..=0x0948).contains(&u)
        || u == 0x094d
        || (0x0951..=0x0957).contains(&u)
        || (0x0e31..=0x0e31).contains(&u)
        || (0x0e34..=0x0e3a).contains(&u)
        || (0x0e47..=0x0e4e).contains(&u)
        || (0x1160..=0x11ff).contains(&u)
        || (0x1ab0..=0x1aff).contains(&u)
        || (0x1dc0..=0x1dff).contains(&u)
        || (0x200b..=0x200f).contains(&u)
        || (0x202a..=0x202e).contains(&u)
        || (0x2060..=0x206f).contains(&u)
        || (0x20d0..=0x20ff).contains(&u)
        || (0xfe00..=0xfe0f).contains(&u)
        || (0xfe20..=0xfe2f).contains(&u)
        || u == 0xfeff
        || (0xe0100..=0xe01ef).contains(&u)
    {
        return 0;
    }
    // East Asian Wide/Fullwidth and wide graphic symbols.
    if (0x1100..=0x115f).contains(&u)
        || (0x231a..=0x231b).contains(&u)
        || (0x2329..=0x232a).contains(&u)
        || (0x23e9..=0x23ec).contains(&u)
        || u == 0x23f0
        || u == 0x23f3
        || (0x25fd..=0x25fe).contains(&u)
        || (0x2614..=0x2615).contains(&u)
        || (0x2648..=0x2653).contains(&u)
        || u == 0x267f
        || u == 0x2693
        || u == 0x26a1
        || (0x26aa..=0x26ab).contains(&u)
        || (0x26bd..=0x26be).contains(&u)
        || (0x26c4..=0x26c5).contains(&u)
        || u == 0x26ce
        || u == 0x26d4
        || u == 0x26ea
        || (0x26f2..=0x26f3).contains(&u)
        || u == 0x26f5
        || u == 0x26fa
        || u == 0x26fd
        || u == 0x2705
        || (0x270a..=0x270b).contains(&u)
        || u == 0x2728
        || u == 0x274c
        || u == 0x274e
        || (0x2753..=0x2755).contains(&u)
        || u == 0x2757
        || (0x2795..=0x2797).contains(&u)
        || u == 0x27b0
        || u == 0x27bf
        || (0x2b1b..=0x2b1c).contains(&u)
        || u == 0x2b50
        || u == 0x2b55
        || (0x2e80..=0x303e).contains(&u)
        || (0x3041..=0xa4cf).contains(&u)
        || (0xa960..=0xa97c).contains(&u)
        || (0xac00..=0xd7a3).contains(&u)
        || (0xf900..=0xfaff).contains(&u)
        || (0xfe10..=0xfe19).contains(&u)
        || (0xfe30..=0xfe6b).contains(&u)
        || (0xff01..=0xff60).contains(&u)
        || (0xffe0..=0xffe6).contains(&u)
        || u == 0x1f004
        || u == 0x1f0cf
        || u == 0x1f18e
        || (0x1f191..=0x1f19a).contains(&u)
        || (0x1f200..=0x1f251).contains(&u)
        || (0x1f300..=0x1f64f).contains(&u)
        || (0x1f680..=0x1f6ff).contains(&u)
        || (0x1f900..=0x1f9ff).contains(&u)
        || (0x1fa70..=0x1faff).contains(&u)
        || (0x20000..=0x3fffd).contains(&u)
    {
        return 2;
    }
    1
}

/// Skip an ANSI escape sequence starting at `cs[i] == '\x1b'`, returning the
/// index immediately following the sequence.
fn skip_ansi_escape(cs: &[char], mut i: usize) -> usize {
    i += 1;
    match cs.get(i) {
        Some('[') => {
            // CSI: ESC [ params intermediate final (0x40..=0x7e)
            i += 1;
            while i < cs.len() && !('\x40'..='\x7e').contains(&cs[i]) {
                i += 1;
            }
            (i + 1).min(cs.len())
        }
        Some(']') => {
            // OSC: terminated by BEL (\x07) or ST (ESC \)
            i += 1;
            while i < cs.len()
                && cs[i] != '\x07'
                && !(cs[i] == '\x1b' && cs.get(i + 1) == Some(&'\\'))
            {
                i += 1;
            }
            if cs.get(i) == Some(&'\x1b') {
                (i + 2).min(cs.len())
            } else {
                (i + 1).min(cs.len())
            }
        }
        Some('P') | Some('X') | Some('^') | Some('_') => {
            // DCS / SOS / PM / APC: terminated by ST (ESC \)
            i += 1;
            while i < cs.len() && !(cs[i] == '\x1b' && cs.get(i + 1) == Some(&'\\')) {
                i += 1;
            }
            (i + 2).min(cs.len())
        }
        Some(&c) if ('\x20'..='\x2f').contains(&c) => {
            // 3+ byte sequences such as charset designation (ESC ( B)
            i += 1;
            while i < cs.len() && ('\x20'..='\x2f').contains(&cs[i]) {
                i += 1;
            }
            (i + 1).min(cs.len())
        }
        Some(_) => i + 1,
        None => i,
    }
}

/// Display width of a string, ignoring ANSI escape sequences.
pub fn visible_width(s: &str) -> usize {
    let cs: Vec<char> = s.chars().collect();
    let mut w = 0;
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '\x1b' {
            i = skip_ansi_escape(&cs, i);
            continue;
        }
        w += char_width(cs[i]);
        i += 1;
    }
    w
}

/// Cursor position after writing a prompt string to a terminal of width `cols`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptPos {
    pub row: usize,
    pub col: usize,
    /// True when the last printable character reached the right margin and a
    /// VT100/xterm terminal is holding a deferred wrap until the next character.
    pub pending_wrap: bool,
}

/// Simulate terminal cell wrapping for `s` on a screen of `cols` columns.
/// Accounts for ANSI escapes, wide characters crossing the right margin, tabs,
/// carriage returns, newlines, and deferred wrap (`xenl`).
pub fn measure_prompt(s: &str, cols: usize) -> PromptPos {
    let cols = cols.max(1);
    let cs: Vec<char> = s.chars().collect();
    let mut row = 0usize;
    let mut col = 0usize;
    let mut pending_wrap = false;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\x1b' {
            i = skip_ansi_escape(&cs, i);
            continue;
        }
        i += 1;
        match c {
            '\r' => {
                col = 0;
                pending_wrap = false;
            }
            '\n' => {
                row += 1;
                pending_wrap = false;
            }
            '\t' => {
                pending_wrap = false;
                col = ((col / 8) + 1) * 8;
                if col >= cols {
                    col = cols - 1;
                }
            }
            _ => {
                let w = char_width(c);
                if w == 0 {
                    continue;
                }
                if pending_wrap {
                    row += 1;
                    col = 0;
                    pending_wrap = false;
                }
                if col > 0 && col + w > cols {
                    row += 1;
                    col = 0;
                }
                if col + w >= cols {
                    col = cols - 1;
                    pending_wrap = true;
                } else {
                    col += w;
                }
            }
        }
    }
    PromptPos { row, col, pending_wrap }
}

/// Percent-encode a string for a `file://` URI in an OSC 7 sequence.
fn osc7_encode(s: &str, keep_slash: bool, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in s.as_bytes() {
        let safe = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && b == b'/');
        if safe {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
}

/// Build an OSC 7 sequence (`\x1b]7;file://<host><encoded-path>\x07`) so
/// terminal emulators track the shell's current working directory.
pub fn osc7_cwd(host: &str, cwd: &str) -> String {
    let mut out = String::with_capacity(16 + host.len() + cwd.len());
    out.push_str("\x1b]7;file://");
    osc7_encode(host, false, &mut out);
    if !cwd.starts_with('/') {
        out.push('/');
    }
    osc7_encode(cwd, true, &mut out);
    out.push('\x07');
    out
}

/// Report the shell's current working directory to the terminal emulator via
/// OSC 7 when running interactively on a terminal.
pub fn report_cwd(sh: &mut Shell) {
    if !sh.interactive || !sys::isatty(2) {
        return;
    }
    if var_str(sh, "TERM") == Some("dumb") {
        return;
    }
    let cwd = sh.cwd();
    let seq = osc7_cwd(sh.hostname(), &cwd);
    sys::write_fd(2, seq.as_bytes());
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
/// lookup is cached per cwd (and `GIT_DIR`), and the branch is cached by `HEAD`
/// metadata. Repeating the prompt in the same repository therefore costs one
/// `stat`, not a directory walk plus an open/read/close cycle.
pub fn git_branch<'a>(sh: &'a mut Shell, cwd: &str) -> Option<&'a str> {
    let gd_env = var_str(sh, "GIT_DIR").filter(|s| !s.is_empty());
    let hit = match &sh.git_cache {
        Some(c) if c.cwd == cwd && c.git_dir_env.as_deref() == gd_env => {
            c.git_dir.is_some()
                || (c.git_dir_env.is_none() && !std::path::Path::new(&*c.dot_git).exists())
        }
        _ => false,
    };
    if !hit {
        let gd_owned = gd_env.map(str::to_string);
        let d = match &gd_owned {
            Some(gd) => {
                let p = std::path::PathBuf::from(gd);
                let abs = if p.is_absolute() { p } else { std::path::PathBuf::from(cwd).join(p) };
                Some(abs.to_string_lossy().into_owned())
            }
            None => find_git_dir(cwd),
        };
        let head_path = d.as_ref().map(|d| Rc::<str>::from(format!("{}/HEAD", d)));
        if head_path.is_none() {
            sh.git_branch_cache = None;
        }
        let dot_git = Rc::<str>::from(format!("{}/.git", cwd));
        sh.git_cache = Some(GitCache {
            cwd: cwd.to_string(),
            git_dir_env: gd_owned,
            git_dir: d,
            head_path,
            dot_git,
        });
    }

    let (head_path, stamp) = {
        let c = sh.git_cache.as_ref()?;
        let head_path = Rc::clone(c.head_path.as_ref()?);
        let stamp = std::fs::metadata(&*head_path)
            .ok()
            .and_then(|m| m.modified().ok().map(|t| (t, m.len(), m.ino())));
        (head_path, stamp)
    };
    let cached = match &sh.git_branch_cache {
        Some(bc) => {
            (Rc::ptr_eq(&bc.head_path, &head_path) || &*bc.head_path == &*head_path)
                && bc.stamp.is_some()
                && bc.stamp == stamp
        }
        None => false,
    };
    if cached {
        return sh.git_branch_cache.as_ref().and_then(|bc| bc.branch.as_deref());
    }
    let head = match std::fs::read_to_string(&*head_path) {
        Ok(h) => h,
        Err(_) => {
            // Repository moved or disappeared; force a fresh lookup next time.
            sh.git_cache = None;
            sh.git_branch_cache = None;
            return None;
        }
    };
    let head = head.trim();
    let branch = match head.strip_prefix("ref: refs/heads/") {
        Some(b) => b.to_string(),
        None => head.chars().take(7).collect(),
    };
    sh.git_branch_cache = Some(GitBranchCache { head_path, branch: Some(branch), stamp });
    sh.git_branch_cache.as_ref().and_then(|bc| bc.branch.as_deref())
}

fn tilde_str(home: &str, p: &str) -> String {
    if home.len() > 1
        && (p == home || (p.starts_with(home) && p.as_bytes().get(home.len()) == Some(&b'/')))
    {
        let mut s = String::with_capacity(p.len() - home.len() + 1);
        s.push('~');
        s.push_str(&p[home.len()..]);
        s
    } else {
        p.to_string()
    }
}

fn var_str<'a>(sh: &'a Shell, name: &str) -> Option<&'a str> {
    sh.var(name).and_then(|v| match &v.val {
        Val::Str(s) => Some(s.as_str()),
        Val::Arr(a) => a.first().map(String::as_str),
        Val::Map(m) => m.iter().find(|(k, _)| k == "0").map(|(_, v)| v.as_str()),
        Val::Int(_) | Val::Unset => None,
    })
}

/// Cached `PWD`, `HOME` and the tilde-shortened cwd. Prompt caches keep this
/// across renders, so an unchanged directory costs no string allocation at all.
struct PathCache {
    valid: bool,
    pwd_var: Option<String>,
    home_var: Option<String>,
    pwd: String,
    home: String,
    tilde: String,
}

impl PathCache {
    fn new() -> PathCache {
        PathCache {
            valid: false,
            pwd_var: None,
            home_var: None,
            pwd: String::new(),
            home: String::new(),
            tilde: String::new(),
        }
    }

    fn get<'a>(&'a mut self, sh: &mut Shell) -> (&'a str, &'a str) {
        let pwd_now = var_str(sh, "PWD");
        let home_now = var_str(sh, "HOME");
        let pwd_changed = !self.valid || self.pwd_var.as_deref() != pwd_now;
        let home_changed = !self.valid || self.home_var.as_deref() != home_now;
        if pwd_changed {
            self.pwd = sh.cwd();
            self.pwd_var = var_str(sh, "PWD").map(str::to_string);
        }
        if home_changed {
            self.home = sh.home();
            self.home_var = var_str(sh, "HOME").map(str::to_string);
        }
        if pwd_changed || home_changed {
            self.tilde = tilde_str(&self.home, &self.pwd);
        }
        self.valid = true;
        (&self.pwd, &self.tilde)
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
    is_root: bool,
    uses_path: bool,
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
        let uses_path = segs.iter().any(|s| {
            matches!(
                s,
                Seg::Cwd | Seg::CwdBase | Seg::CwdStyled | Seg::Git | Seg::GitStyled
            )
        });
        PromptPlan { segs, expand, is_root, uses_path }
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

fn utf8_locale(sh: &mut Shell) -> bool {
    if let Some(v) = sh.prompt_utf8 {
        return v;
    }
    let v = {
        let mut v = false;
        for k in ["LC_ALL", "LC_CTYPE", "LANG"] {
            if let Some(var) = sh.var(k) {
                if let Val::Str(s) = &var.val {
                    if !s.is_empty() {
                        v = has_utf8(s);
                        break;
                    }
                }
            }
        }
        v
    };
    sh.prompt_utf8 = Some(v);
    v
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

fn push_cwd_seg(t: &str, out: &mut String, protect: bool) {
    out.push_str("\x1b[1;34m");
    push_protected(out, t, protect);
    out.push_str("\x1b[0m");
}

fn push_git_seg(sh: &mut Shell, cwd: &str, out: &mut String, protect: bool) {
    if let Some(b) = git_branch(sh, cwd) {
        out.push_str(" \x1b[35m(");
        push_protected(out, b, protect);
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

fn push_symbol_seg(sh: &mut Shell, is_root: bool, out: &mut String) {
    let sym = if is_root {
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

fn render_plan(sh: &mut Shell, plan: &PromptPlan, out: &mut String, paths: &mut PathCache) {
    let mut now: Option<i64> = None;
    // Resolve PWD/HOME/tilde once per render, and only when the plan uses them.
    let (cwd, t) = if plan.uses_path {
        paths.get(sh)
    } else {
        ("", "")
    };
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
            Seg::Cwd => push_protected(out, t, plan.expand),
            Seg::CwdBase => {
                let base = if t == "~" || t == "/" {
                    t
                } else {
                    t.rsplit('/').next().unwrap_or("")
                };
                push_protected(out, base, plan.expand);
            }
            Seg::Jobs => {
                let _ = write!(out, "{}", sh.jobs.len());
            }
            Seg::History => {
                let _ = write!(out, "{}", sh.history.entries.len() + 1);
            }
            Seg::Git => {
                if let Some(b) = git_branch(sh, cwd) {
                    push_protected(out, b, plan.expand);
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
            Seg::CwdStyled => push_cwd_seg(t, out, plan.expand),
            Seg::GitStyled => push_git_seg(sh, cwd, out, plan.expand),
            Seg::JobsStyled => push_jobs_seg(sh, out),
            Seg::DurationStyled => push_duration_seg(sh, out, plan.expand),
            Seg::StatusStyled => push_status_seg(sh, out),
            Seg::SymbolStyled => push_symbol_seg(sh, plan.is_root, out),
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
    render_plan(sh, &plan, &mut out, &mut PathCache::new());
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
    paths: PathCache,
}

impl PromptCache {
    pub fn new() -> PromptCache {
        PromptCache {
            src: String::new(),
            plan: PromptPlan::compile(""),
            out: String::new(),
            paths: PathCache::new(),
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
        render_plan(sh, &self.plan, &mut self.out, &mut self.paths);
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
    fn path_cache_tracks_cwd_and_home() {
        let mut sh = Shell::new();
        let _ = sh.set_str("PS1", "\\w");
        let _ = sh.set_str("HOME", "/home/test");
        let _ = sh.set_str("PWD", "/home/test/project");
        let mut c = PromptCache::new();
        assert_eq!(c.render(&mut sh, &["PS1"], DEFAULT_PS1), "~/project");
        let _ = sh.set_str("PWD", "/tmp");
        assert_eq!(c.render(&mut sh, &["PS1"], DEFAULT_PS1), "/tmp");
        let _ = sh.set_str("HOME", "/tmp");
        assert_eq!(c.render(&mut sh, &["PS1"], DEFAULT_PS1), "~");
    }

    #[test]
    fn utf8_locale_cache_invalidates() {
        let mut sh = Shell::new();
        let _ = sh.set_str("LC_ALL", "C");
        let _ = sh.set_str("LC_CTYPE", "");
        let _ = sh.set_str("LANG", "");
        assert!(!utf8_locale(&mut sh));
        let _ = sh.set_str("LC_ALL", "en_US.UTF-8");
        assert!(utf8_locale(&mut sh));
        let _ = sh.unset_var("LC_ALL");
        let _ = sh.set_str("LANG", "C");
        assert!(!utf8_locale(&mut sh));
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
        assert_eq!(git_branch(&mut sh, &cwd), Some("main"), "after init");
        // The cached branch must be invalidated when HEAD changes, including
        // when the checkout changes the file size.
        std::fs::write(base.join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();
        assert_eq!(git_branch(&mut sh, &cwd), Some("feature"), "after checkout");
        // Atomic replacement changes the inode even if the size is identical.
        let tmp = base.join(".git/HEAD.tmp");
        std::fs::write(&tmp, "ref: refs/heads/branch1\n").unwrap();
        std::fs::rename(&tmp, base.join(".git/HEAD")).unwrap();
        assert_eq!(git_branch(&mut sh, &cwd), Some("branch1"), "atomic replace");
        // Detached HEAD displays a short hash.
        std::fs::write(base.join(".git/HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        assert_eq!(git_branch(&mut sh, &cwd), Some("0123456"), "detached");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn measure_prompt_handles_wraps_wide_chars_and_escapes() {
        // Exact right margin triggers deferred wrap.
        let p = measure_prompt("1234567890", 10);
        assert_eq!(p, PromptPos { row: 0, col: 9, pending_wrap: true });

        // One char past margin wraps to row 1 col 1.
        let p = measure_prompt("12345678901", 10);
        assert_eq!(p, PromptPos { row: 1, col: 1, pending_wrap: false });

        // ANSI CSI, OSC, and 3-byte charset escapes consume zero columns.
        let s = "\x1b]7;file://host/tmp\x07\x1b(B\x1b[1;34m~/path\x1b[0m > ";
        assert_eq!(visible_width(s), 9);
        assert_eq!(measure_prompt(s, 40), PromptPos { row: 0, col: 9, pending_wrap: false });

        // Wide char at col == cols - 1 wraps before the character.
        let s = format!("{}a", "123456789\u{65e5}");
        assert_eq!(measure_prompt(&s, 10), PromptPos { row: 1, col: 3, pending_wrap: false });

        // Wide graphic symbol (U+1F680) has display width 2.
        assert_eq!(char_width('\u{1f680}'), 2);
        // Combining acute accent (U+0301) has display width 0.
        assert_eq!(char_width('\u{0301}'), 0);
    }

    #[test]
    fn osc7_cwd_encodes_uri_safely() {
        assert_eq!(
            osc7_cwd("workstation", "/home/user/project"),
            "\x1b]7;file://workstation/home/user/project\x07"
        );
        assert_eq!(
            osc7_cwd("host", "/tmp/my dir/100%#?name"),
            "\x1b]7;file://host/tmp/my%20dir/100%25%23%3Fname\x07"
        );
        assert_eq!(
            osc7_cwd("host", "/"),
            "\x1b]7;file://host/\x07"
        );
        // UTF-8 multibyte path segments are percent-encoded byte-by-byte.
        assert_eq!(
            osc7_cwd("host", "/tmp/\u{043f}\u{0430}\u{043f}\u{043a}\u{0430}"),
            "\x1b]7;file://host/tmp/%D0%BF%D0%B0%D0%BF%D0%BA%D0%B0\x07"
        );
    }
}
