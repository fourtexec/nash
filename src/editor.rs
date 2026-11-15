//! The interactive line editor: syntax highlighting, history autosuggestions,
//! menu completion, multi-line editing and incremental history search.

use crate::highlight::{highlight, sgr, St};
use crate::parser::RESERVED;
use crate::prompt::{char_width, visible_width};
use crate::shell::Shell;
use crate::sys;

pub enum Input {
    Line(String),
    Eof,
    Interrupt,
}

#[derive(Debug, PartialEq, Clone)]
enum Key {
    Char(char),
    Ctrl(char),
    Alt(char),
    Enter,
    Tab,
    ShiftTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    WordLeft,
    WordRight,
    AltBackspace,
    Paste(String),
    Esc,
    Eof,
    Resize,
    Unknown,
}

struct Cand {
    text: String,
    display: String,
    dir: bool,
    /// SGR parameters (from LS_COLORS) used in the completion menu.
    color: String,
}

struct Menu {
    items: Vec<Cand>,
    sel: Option<usize>,
    start: usize,
    end: usize,
    quote: Option<char>,
}

struct Painter {
    out: String,
    row: usize,
    col: usize,
    cols: usize,
}

impl Painter {
    fn put(&mut self, c: char) {
        if c == '\n' {
            self.out.push_str("\r\n");
            self.row += 1;
            self.col = 0;
            return;
        }
        let (s, w) = if c == '\t' {
            (' ', 1)
        } else if c.is_control() {
            ('?', 1)
        } else {
            (c, char_width(c))
        };
        if self.col + w > self.cols {
            self.out.push_str("\r\n");
            self.row += 1;
            self.col = 0;
        }
        self.out.push(s);
        self.col += w;
        if self.col >= self.cols {
            self.out.push_str("\r\n");
            self.row += 1;
            self.col = 0;
        }
    }
}

struct Editor<'a> {
    sh: &'a mut Shell,
    prompt: String,
    head: String,
    rprompt: String,
    buf: Vec<char>,
    cur: usize,
    row: usize,
    suggestion: String,
    hist_idx: Option<usize>,
    hist_prefix: String,
    saved: Vec<char>,
    menu: Option<Menu>,
    kill: Vec<char>,
    search: Option<(String, Option<usize>)>,
    cwd: String,
    ls: crate::colors::LsColors,
}

const GIT_CMDS: &[&str] = &[
    "add", "bisect", "blame", "branch", "checkout", "cherry-pick", "clean", "clone", "commit", "config", "describe", "diff", "fetch", "grep", "init", "log", "merge", "mv", "pull", "push", "rebase", "reflog", "remote", "reset", "restore", "revert", "rm", "show", "stash", "status", "submodule", "switch", "tag", "worktree",
];
const GIT_REF_CMDS: &[&str] = &["checkout", "switch", "merge", "rebase", "branch", "diff", "log", "show", "push", "pull", "reset", "cherry-pick", "revert"];
const CARGO_CMDS: &[&str] = &["add", "bench", "build", "check", "clean", "clippy", "doc", "fmt", "init", "install", "new", "publish", "remove", "run", "search", "test", "tree", "uninstall", "update"];
const PREFIX_CMDS: &[&str] = &["sudo", "exec", "command", "builtin", "time", "env", "nohup", "xargs", "doas", "nice", "then", "do", "else", "elif", "if", "while", "until", "!", "{", "defer"];

fn escape_word(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if " \t\n'\"\\$&;|<>()*?[]#!{}`".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn unescape_word(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(n) = it.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn poll_byte(ms: i32) -> Option<u8> {
    let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    let r = sys::poll(&mut [pfd], ms);
    if r > 0 {
        sys::read_byte(0).ok().flatten()
    } else {
        None
    }
}

fn read_key() -> Key {
    let b = match sys::read_byte(0) {
        Ok(Some(b)) => b,
        Ok(None) => return Key::Eof,
        Err(e) if e == libc::EINTR => return Key::Resize,
        Err(_) => return Key::Eof,
    };
    match b {
        0x1b => {
            let b = match poll_byte(40) {
                Some(b) => b,
                None => return Key::Esc,
            };
            match b {
                b'[' => {
                    let mut seq = String::new();
                    loop {
                        match poll_byte(40) {
                            Some(c) => {
                                seq.push(c as char);
                                if (0x40..=0x7e).contains(&c) {
                                    break;
                                }
                            }
                            None => return Key::Unknown,
                        }
                    }
                    match seq.as_str() {
                        "A" => Key::Up,
                        "B" => Key::Down,
                        "C" => Key::Right,
                        "D" => Key::Left,
                        "H" | "1~" | "7~" => Key::Home,
                        "F" | "4~" | "8~" => Key::End,
                        "3~" => Key::Delete,
                        "Z" => Key::ShiftTab,
                        "1;5C" | "1;3C" | "5C" => Key::WordRight,
                        "1;5D" | "1;3D" | "5D" => Key::WordLeft,
                        "3;5~" | "3;3~" => Key::Alt('d'),
                        "200~" => {
                            let mut bytes: Vec<u8> = Vec::new();
                            let end = b"\x1b[201~";
                            loop {
                                match sys::read_byte(0) {
                                    Ok(Some(c)) => {
                                        bytes.push(c);
                                        if bytes.ends_with(end) {
                                            bytes.truncate(bytes.len() - end.len());
                                            break;
                                        }
                                    }
                                    Err(e) if e == libc::EINTR => continue,
                                    _ => break,
                                }
                            }
                            Key::Paste(String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n"))
                        }
                        _ => Key::Unknown,
                    }
                }
                b'O' => match poll_byte(40) {
                    Some(b'A') => Key::Up,
                    Some(b'B') => Key::Down,
                    Some(b'C') => Key::Right,
                    Some(b'D') => Key::Left,
                    Some(b'H') => Key::Home,
                    Some(b'F') => Key::End,
                    _ => Key::Unknown,
                },
                0x7f | 0x08 => Key::AltBackspace,
                b'\r' | b'\n' => Key::Alt('\n'),
                c if (0x20..0x7f).contains(&c) => Key::Alt(c as char),
                _ => Key::Unknown,
            }
        }
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x7f | 0x08 => Key::Backspace,
        0..=0x1f => Key::Ctrl((b + b'a' - 1) as char),
        _ => {
            let n = if b < 0x80 {
                1
            } else if b >> 5 == 0b110 {
                2
            } else if b >> 4 == 0b1110 {
                3
            } else if b >> 3 == 0b11110 {
                4
            } else {
                return Key::Unknown;
            };
            let mut bytes = vec![b];
            while bytes.len() < n {
                match sys::read_byte(0) {
                    Ok(Some(c)) => bytes.push(c),
                    Err(e) if e == libc::EINTR => continue,
                    _ => return Key::Unknown,
                }
            }
            match std::str::from_utf8(&bytes).ok().and_then(|s| s.chars().next()) {
                Some(c) => Key::Char(c),
                None => Key::Unknown,
            }
        }
    }
}

impl<'a> Editor<'a> {
    fn text(&self) -> String {
        self.buf.iter().collect()
    }

    fn menu_lines(&self, cols: usize, rows: usize) -> Vec<String> {
        let m = match &self.menu {
            Some(m) => m,
            None => return Vec::new(),
        };
        let n = m.items.len();
        let maxw = m.items.iter().map(|c| visible_width(&c.display)).max().unwrap_or(1).min(cols.saturating_sub(2)).max(1);
        let colw = maxw + 2;
        let ncols = (cols / colw).max(1);
        let nrows = n.div_ceil(ncols);
        let max_rows = nrows.min(rows.saturating_sub(4).clamp(2, 10));
        let sel_row = m.sel.map(|s| s / ncols).unwrap_or(0);
        let first = if sel_row >= max_rows { sel_row + 1 - max_rows } else { 0 };
        let mut lines = Vec::new();
        for r in first..first + max_rows {
            let mut line = String::new();
            for c in 0..ncols {
                let idx = r * ncols + c;
                if idx >= n {
                    break;
                }
                let it = &m.items[idx];
                let mut d = String::new();
                let mut w = 0;
                for ch in it.display.chars() {
                    let cw = char_width(ch);
                    if w + cw > maxw {
                        break;
                    }
                    d.push(ch);
                    w += cw;
                }
                let padding = " ".repeat(colw - w);
                if Some(idx) == m.sel {
                    line.push_str(&format!("\x1b[7m{}\x1b[0m{}", d, padding));
                } else if !it.color.is_empty() {
                    line.push_str(&format!("\x1b[{}m{}\x1b[0m{}", it.color, d, padding));
                } else {
                    line.push_str(&format!("{}{}", d, padding));
                }
            }
            lines.push(line);
        }
        if nrows > max_rows {
            lines.push(format!("\x1b[90m… {} items, Tab to cycle\x1b[0m", n));
        }
        lines
    }

    fn refresh(&mut self) {
        let (cols, rows) = sys::term_size();
        let mut p = Painter { out: String::new(), row: 0, col: 0, cols };
        if self.row > 0 {
            p.out.push_str(&format!("\x1b[{}A", self.row));
        }
        p.out.push_str("\r\x1b[J");
        let prompt = match &self.search {
            Some((q, idx)) => format!("({}reverse-i-search)`{}': ", if idx.is_none() && !q.is_empty() { "failed " } else { "" }, q),
            None => self.prompt.clone(),
        };
        p.out.push_str(&prompt);
        p.col = visible_width(&prompt) % cols;
        let styles = if self.buf.len() < 5000 { highlight(self.sh, &self.buf) } else { vec![St::Plain; self.buf.len()] };
        let mut cur_pos = (0, p.col);
        let mut last = St::Plain;
        for (i, &c) in self.buf.iter().enumerate() {
            if i == self.cur {
                cur_pos = (p.row, p.col);
            }
            if styles[i] != last {
                p.out.push_str("\x1b[0m");
                p.out.push_str(sgr(styles[i]));
                last = styles[i];
            }
            p.put(c);
        }
        if last != St::Plain {
            p.out.push_str("\x1b[0m");
        }
        if self.cur >= self.buf.len() {
            cur_pos = (p.row, p.col);
        }
        if !self.suggestion.is_empty() && self.cur == self.buf.len() && self.menu.is_none() && self.search.is_none() {
            p.out.push_str("\x1b[90m");
            let budget = (cols - p.col).saturating_sub(1) + cols;
            for c in self.suggestion.chars().take(budget) {
                p.put(c);
            }
            p.out.push_str("\x1b[0m");
        }
        if !self.rprompt.is_empty() && p.row == 0 && self.menu.is_none() && self.search.is_none() {
            let w = visible_width(&self.rprompt);
            if p.col + w + 2 < cols {
                p.out.push_str(&format!("\x1b[{}G{}\x1b[0m", cols - w, self.rprompt));
            }
        }
        for line in self.menu_lines(cols, rows) {
            p.out.push_str("\r\n");
            p.out.push_str(&line);
            p.row += 1;
        }
        if p.row > cur_pos.0 {
            p.out.push_str(&format!("\x1b[{}A", p.row - cur_pos.0));
        }
        p.out.push('\r');
        if cur_pos.1 > 0 {
            p.out.push_str(&format!("\x1b[{}C", cur_pos.1));
        }
        self.row = cur_pos.0;
        sys::write_fd(2, p.out.as_bytes());
    }

    fn edited(&mut self) {
        self.hist_idx = None;
        self.suggestion.clear();
        if !self.buf.is_empty() && self.cur == self.buf.len() {
            let t = self.text();
            if let Some(s) = self.sh.history.suggest(&t, &self.cwd) {
                self.suggestion = s[t.len()..].to_string();
            } else if !t.contains(char::is_whitespace) && !t.is_empty() {
                // fish-style: suggest a command name when nothing in history matches
                let mut names = self.sh.command_names();
                names.sort();
                if let Some(name) = names.into_iter().find(|n| n.starts_with(&t) && n.len() > t.len()) {
                    self.suggestion = name[t.len()..].to_string();
                }
            }
        }
    }

    fn insert_str(&mut self, s: &str) {
        let cs: Vec<char> = s.chars().collect();
        let n = cs.len();
        self.buf.splice(self.cur..self.cur, cs);
        self.cur += n;
        self.edited();
    }

    fn replace(&mut self, start: usize, end: usize, s: &str) {
        let cs: Vec<char> = s.chars().collect();
        let n = cs.len();
        self.buf.splice(start..end, cs);
        self.cur = start + n;
        self.edited();
    }

    fn set_buf(&mut self, s: &str) {
        self.buf = s.chars().collect();
        self.cur = self.buf.len();
        self.suggestion.clear();
    }

    fn word_left(&self) -> usize {
        let mut i = self.cur;
        while i > 0 && !self.buf[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && self.buf[i - 1].is_alphanumeric() {
            i -= 1;
        }
        i
    }

    fn word_right(&self) -> usize {
        let n = self.buf.len();
        let mut i = self.cur;
        while i < n && !self.buf[i].is_alphanumeric() {
            i += 1;
        }
        while i < n && self.buf[i].is_alphanumeric() {
            i += 1;
        }
        i
    }

    fn accept_suggestion(&mut self, whole: bool) {
        if self.suggestion.is_empty() {
            return;
        }
        let take: String = if whole {
            std::mem::take(&mut self.suggestion)
        } else {
            let cs: Vec<char> = self.suggestion.chars().collect();
            let mut i = 0;
            while i < cs.len() && !cs[i].is_alphanumeric() {
                i += 1;
            }
            while i < cs.len() && cs[i].is_alphanumeric() {
                i += 1;
            }
            cs[..i].iter().collect()
        };
        let cs: Vec<char> = take.chars().collect();
        self.buf.extend(cs);
        self.cur = self.buf.len();
        self.edited();
    }

    fn hist_move(&mut self, up: bool) {
        let n = self.sh.history.entries.len();
        if self.hist_idx.is_none() {
            if !up {
                return;
            }
            self.saved = self.buf.clone();
            self.hist_prefix = self.text();
        }
        let cur_text = self.text();
        let found = if up {
            let from = self.hist_idx.unwrap_or(n);
            (0..from).rev().find(|&i| {
                let c = &self.sh.history.entries[i].cmd;
                c.starts_with(&self.hist_prefix) && *c != cur_text
            })
        } else {
            let from = self.hist_idx.unwrap_or(n) + 1;
            (from..n).find(|&i| {
                let c = &self.sh.history.entries[i].cmd;
                c.starts_with(&self.hist_prefix) && *c != cur_text
            })
        };
        match found {
            Some(i) => {
                let c = self.sh.history.entries[i].cmd.clone();
                self.set_buf(&c);
                self.hist_idx = Some(i);
            }
            None => {
                if !up {
                    self.buf = self.saved.clone();
                    self.cur = self.buf.len();
                    self.hist_idx = None;
                    self.suggestion.clear();
                }
            }
        }
    }

    /// Move the cursor one line up/down inside a multi-line buffer.
    fn line_move(&mut self, up: bool) -> bool {
        let line_start = |pos: usize, buf: &[char]| (0..pos).rev().find(|&i| buf[i] == '\n').map(|i| i + 1).unwrap_or(0);
        let start = line_start(self.cur, &self.buf);
        let col = self.cur - start;
        if up {
            if start == 0 {
                return false;
            }
            let prev_start = line_start(start - 1, &self.buf);
            self.cur = (prev_start + col).min(start - 1);
        } else {
            let end = (self.cur..self.buf.len()).find(|&i| self.buf[i] == '\n');
            match end {
                None => return false,
                Some(e) => {
                    let next_end = (e + 1..self.buf.len()).find(|&i| self.buf[i] == '\n').unwrap_or(self.buf.len());
                    self.cur = (e + 1 + col).min(next_end);
                }
            }
        }
        true
    }

    // ---------- completion ----------

    fn word_start(&self) -> (usize, Option<char>) {
        let mut ws = 0;
        let mut quote: Option<(char, usize)> = None;
        let mut i = 0;
        while i < self.cur {
            let c = self.buf[i];
            match quote {
                Some((q, _)) => {
                    if c == '\\' && q == '"' {
                        i += 1;
                    } else if c == q {
                        quote = None;
                    }
                }
                None => {
                    if c == '\\' {
                        i += 1;
                    } else if c == '\'' || c == '"' {
                        quote = Some((c, i));
                    } else if c.is_whitespace() || "|&;()<>=".contains(c) {
                        ws = i + 1;
                    }
                }
            }
            i += 1;
        }
        match quote {
            Some((q, pos)) => (pos, Some(q)),
            None => (ws, None),
        }
    }

    fn file_cands(&mut self, raw: &str, dirs_only: bool, quote: Option<char>) -> Vec<Cand> {
        let (dir_typed, prefix) = match raw.rfind('/') {
            Some(i) => (&raw[..=i], &raw[i + 1..]),
            None => ("", raw),
        };
        let home = self.sh.home();
        let dir_real = if dir_typed.is_empty() {
            ".".to_string()
        } else if let Some(r) = dir_typed.strip_prefix("~/") {
            format!("{}/{}", home, r)
        } else {
            dir_typed.to_string()
        };
        let mut entries: Vec<(String, bool)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir_real) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && !prefix.starts_with('.') {
                    continue;
                }
                let is_dir = std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false);
                if dirs_only && !is_dir {
                    continue;
                }
                entries.push((name, is_dir));
            }
        }
        entries.sort();
        let lp = prefix.to_lowercase();
        let mut hits: Vec<&(String, bool)> = entries.iter().filter(|(n, _)| n.starts_with(prefix)).collect();
        if hits.is_empty() && !prefix.is_empty() {
            hits = entries.iter().filter(|(n, _)| n.to_lowercase().starts_with(&lp)).collect();
        }
        if hits.is_empty() && !prefix.is_empty() {
            hits = entries.iter().filter(|(n, _)| n.to_lowercase().contains(&lp)).collect();
        }
        hits.into_iter()
            .map(|(name, is_dir)| {
                let mut text = match quote {
                    Some(q) => format!("{}{}{}", q, dir_typed, name),
                    None => {
                        let d = if let Some(r) = dir_typed.strip_prefix('~') { format!("~{}", escape_word(r)) } else { escape_word(dir_typed) };
                        format!("{}{}", d, escape_word(name))
                    }
                };
                let mut display = name.clone();
                if *is_dir {
                    text.push('/');
                    display.push('/');
                }
                let color = self.ls.color_for(&format!("{}/{}", dir_real.trim_end_matches('/'), name), name);
                Cand { text, display, dir: *is_dir, color }
            })
            .collect()
    }

    fn word_cands<I: IntoIterator<Item = String>>(names: I, prefix: &str) -> Vec<Cand> {
        let mut v: Vec<String> = names.into_iter().filter(|n| n.starts_with(prefix)).collect();
        v.sort();
        v.dedup();
        v.into_iter().map(|n| Cand { text: escape_word(&n), display: n, dir: false, color: String::new() }).collect()
    }

    fn git_refs(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let mut dir = std::path::PathBuf::from(&self.cwd);
        loop {
            let heads = dir.join(".git/refs/heads");
            if heads.is_dir() {
                fn walk(d: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
                    if let Ok(rd) = std::fs::read_dir(d) {
                        for e in rd.flatten() {
                            let name = format!("{}{}", prefix, e.file_name().to_string_lossy());
                            if e.path().is_dir() {
                                walk(&e.path(), &format!("{}/", name), out);
                            } else {
                                out.push(name);
                            }
                        }
                    }
                }
                walk(&heads, "", &mut out);
                if let Ok(packed) = std::fs::read_to_string(dir.join(".git/packed-refs")) {
                    for l in packed.lines() {
                        if let Some(i) = l.find("refs/heads/") {
                            out.push(l[i + 11..].to_string());
                        }
                    }
                }
                break;
            }
            if !dir.pop() {
                break;
            }
        }
        out
    }

    fn candidates(&mut self, before: &str, word: &str, quote: Option<char>) -> Vec<Cand> {
        let raw = match quote {
            Some(_) => word.chars().skip(1).collect::<String>(),
            None => unescape_word(word),
        };
        if quote != Some('\'') && raw.starts_with('$') && !raw.contains('/') {
            let (pre, name, post) = if let Some(n) = raw.strip_prefix("${") { ("${", n, "}") } else { ("$", &raw[1..], "") };
            let mut names: Vec<String> = self.sh.all_vars().into_iter().map(|(n, _)| n).filter(|n| n.starts_with(name)).collect();
            names.sort();
            let q = quote.map(|q| q.to_string()).unwrap_or_default();
            return names.into_iter().map(|n| Cand { text: format!("{}{}{}{}", q, pre, n, post), display: n, dir: false, color: "36".to_string() }).collect();
        }
        let seg_start = before.rfind(['|', ';', '&', '(', '\n', '`']).map(|i| i + 1).unwrap_or(0);
        let mut tokens: Vec<&str> = before[seg_start..].split_whitespace().collect();
        while !tokens.is_empty() && (PREFIX_CMDS.contains(&tokens[0]) || (tokens[0].contains('=') && !tokens[0].starts_with('-'))) {
            tokens.remove(0);
        }
        let after_eq = before.ends_with('=');
        let cmd_pos = tokens.is_empty() && !after_eq;
        if cmd_pos && !raw.contains('/') && !raw.starts_with('~') && !raw.starts_with('.') && quote.is_none() {
            let mut names = self.sh.command_names();
            names.extend(RESERVED.iter().filter(|r| r.len() > 1).map(|s| s.to_string()));
            let c = Self::word_cands(names, &raw);
            if !c.is_empty() {
                return c;
            }
        }
        let cmd = tokens.first().copied().unwrap_or("");
        let mut dirs_only = false;
        if quote.is_none() && !raw.contains('/') && !after_eq {
            let special: Option<Vec<String>> = match cmd {
                "cd" | "pushd" | "rmdir" => {
                    dirs_only = true;
                    None
                }
                "unset" | "export" | "readonly" | "declare" | "local" | "typeset" => Some(self.sh.all_vars().into_iter().map(|(n, _)| n).collect()),
                "unalias" => Some(self.sh.aliases.keys().cloned().collect()),
                "type" | "which" | "man" | "hash" | "whereis" => Some(self.sh.command_names()),
                "kill" if raw.starts_with('-') => Some(sys::SIGNALS.iter().map(|(n, _)| format!("-{}", n)).collect()),
                "set" | "setopt" | "unsetopt" if tokens.last() == Some(&"-o") || cmd != "set" => Some(crate::shell::OPTION_NAMES.iter().map(|s| s.to_string()).chain(std::iter::once("strict".to_string())).collect()),
                "git" if tokens.len() == 1 => Some(GIT_CMDS.iter().map(|s| s.to_string()).collect()),
                "git" if tokens.len() >= 2 && GIT_REF_CMDS.contains(&tokens[1]) && !raw.starts_with('-') => {
                    let refs = self.git_refs();
                    let mut c = Self::word_cands(refs, &raw);
                    if !c.is_empty() {
                        c.extend(self.file_cands(&raw, false, quote));
                        return c;
                    }
                    None
                }
                "cargo" if tokens.len() == 1 => Some(CARGO_CMDS.iter().map(|s| s.to_string()).collect()),
                _ => {
                    if let Some(func) = self.sh.completion_funcs.get(cmd).cloned() {
                        let line: String = self.buf.iter().collect();
                        let before: String = self.buf[..self.cur].iter().collect();
                        let trailing_space = before.ends_with(char::is_whitespace);
                        let mut words: Vec<String> = line.split_whitespace().map(|s| s.to_string()).collect();
                        if words.is_empty() {
                            words.push(String::new());
                        }
                        let cword = if trailing_space {
                            before.split_whitespace().count()
                        } else {
                            before.split_whitespace().count().saturating_sub(1)
                        };
                        let cword = cword.min(words.len().saturating_sub(1));
                        let _ = self.sh.set_str("COMP_LINE", line.clone());
                        let _ = self.sh.set("COMP_WORDS", crate::shell::Val::Arr(words));
                        let _ = self.sh.set_str("COMP_POINT", self.cur.to_string());
                        let _ = self.sh.set_str("COMP_CWORD", cword.to_string());
                        let _ = self.sh.unset_var("COMPREPLY");
                        if let Some(f) = self.sh.funcs.get(&func).cloned() {
                            let _ = self.sh.call_function(&f, &[]);
                        }
                        let mut reply = Vec::new();
                        if let Some(v) = self.sh.var("COMPREPLY") {
                            match &v.val {
                                crate::shell::Val::Arr(a) => reply = a.clone(),
                                crate::shell::Val::Str(s) => reply = s.split_whitespace().map(|x| x.to_string()).collect(),
                                _ => {}
                            }
                        }
                        if reply.is_empty() {
                            self.sh.completions.get(cmd).cloned()
                        } else {
                            Some(reply)
                        }
                    } else {
                        self.sh.completions.get(cmd).cloned()
                    }
                }
            };
            if let Some(names) = special {
                let c = Self::word_cands(names, &raw);
                if !c.is_empty() {
                    return c;
                }
            }
        }
        self.file_cands(&raw, dirs_only, quote)
    }

    fn apply_menu(&mut self) {
        let (start, end, text) = match &self.menu {
            Some(m) => match m.sel {
                Some(s) => (m.start, m.end, m.items[s].text.clone()),
                None => return,
            },
            None => return,
        };
        self.replace(start, end, &text);
        if let Some(m) = &mut self.menu {
            m.end = start + text.chars().count();
        }
    }

    fn complete(&mut self, back: bool) {
        if let Some(m) = &mut self.menu {
            let n = m.items.len();
            m.sel = Some(match m.sel {
                None => {
                    if back {
                        n - 1
                    } else {
                        0
                    }
                }
                Some(s) => {
                    if back {
                        (s + n - 1) % n
                    } else {
                        (s + 1) % n
                    }
                }
            });
            self.apply_menu();
            return;
        }
        let (start, quote) = self.word_start();
        let word: String = self.buf[start..self.cur].iter().collect();
        let before: String = self.buf[..start].iter().collect();
        let cands = self.candidates(&before, &word, quote);
        if cands.is_empty() {
            sys::write_fd(2, b"\x07");
            return;
        }
        if cands.len() == 1 {
            let c = &cands[0];
            let mut t = c.text.clone();
            if !c.dir {
                if let Some(q) = quote {
                    t.push(q);
                }
                t.push(' ');
            }
            let end = self.cur;
            self.replace(start, end, &t);
            return;
        }
        let first: Vec<char> = cands[0].text.chars().collect();
        let mut n = first.len();
        for c in &cands[1..] {
            let cs: Vec<char> = c.text.chars().collect();
            n = n.min(cs.len());
            for k in 0..n {
                if cs[k] != first[k] {
                    n = k;
                    break;
                }
            }
        }
        let lcp: String = first[..n].iter().collect();
        if n > word.chars().count() && lcp.starts_with(&word) {
            let end = self.cur;
            self.replace(start, end, &lcp);
        }
        let end = self.cur;
        self.menu = Some(Menu { items: cands, sel: None, start, end, quote });
    }

    fn close_menu(&mut self, accept: bool) {
        if let Some(m) = self.menu.take() {
            if accept {
                if let Some(s) = m.sel {
                    if !m.items[s].dir {
                        let mut t = String::new();
                        if let Some(q) = m.quote {
                            t.push(q);
                        }
                        t.push(' ');
                        self.insert_str(&t);
                    }
                }
            }
        }
    }

    // ---------- history search ----------

    fn search_find(&mut self, from: usize) {
        let q = match &self.search {
            Some((q, _)) => q.clone(),
            None => return,
        };
        let found = (0..from.min(self.sh.history.entries.len())).rev().find(|&i| self.sh.history.entries[i].cmd.contains(&q));
        if let Some(i) = found {
            let c = self.sh.history.entries[i].cmd.clone();
            self.set_buf(&c);
        }
        if let Some(s) = &mut self.search {
            s.1 = found;
        }
    }

    /// Returns true if the key was consumed by search mode.
    fn search_key(&mut self, key: &Key) -> bool {
        let n = self.sh.history.entries.len();
        match key {
            Key::Char(c) => {
                if let Some(s) = &mut self.search {
                    s.0.push(*c);
                }
                self.search_find(n);
                true
            }
            Key::Backspace => {
                if let Some(s) = &mut self.search {
                    s.0.pop();
                }
                self.search_find(n);
                true
            }
            Key::Ctrl('r') => {
                let from = self.search.as_ref().and_then(|s| s.1).unwrap_or(n);
                self.search_find(from);
                true
            }
            Key::Ctrl('c') | Key::Ctrl('g') => {
                self.search = None;
                self.buf = self.saved.clone();
                self.cur = self.buf.len();
                true
            }
            Key::Esc | Key::Resize => {
                if *key == Key::Esc {
                    self.search = None;
                }
                true
            }
            _ => {
                self.search = None;
                false
            }
        }
    }

    fn finish(&mut self) {
        self.menu = None;
        self.search = None;
        self.suggestion.clear();
        self.cur = self.buf.len();
        let rp = std::mem::take(&mut self.rprompt);
        self.refresh();
        self.rprompt = rp;
    }

    fn run(&mut self) -> Input {
        loop {
            let key = read_key();
            if self.search.is_some() && self.search_key(&key) {
                self.refresh();
                continue;
            }
            if self.menu.is_some() {
                let has_sel = self.menu.as_ref().map(|m| m.sel.is_some()).unwrap_or(false);
                match key {
                    Key::Tab | Key::ShiftTab => {}
                    Key::Down | Key::Right if has_sel => {
                        self.complete(false);
                        self.refresh();
                        continue;
                    }
                    Key::Up | Key::Left if has_sel => {
                        self.complete(true);
                        self.refresh();
                        continue;
                    }
                    Key::Enter if has_sel => {
                        self.close_menu(true);
                        self.refresh();
                        continue;
                    }
                    Key::Esc => {
                        self.close_menu(false);
                        self.refresh();
                        continue;
                    }
                    Key::Resize => {}
                    _ => self.close_menu(false),
                }
            }
            match key {
                Key::Resize => {
                    sys::take_pending(libc::SIGWINCH);
                }
                Key::Eof => {
                    self.finish();
                    return Input::Eof;
                }
                Key::Enter => {
                    let s = self.text();
                    let trailing = s.chars().rev().take_while(|c| *c == '\\').count();
                    let incomplete = trailing % 2 == 1 || matches!(crate::parser::check_complete(&s, &self.sh.aliases), Err(e) if e.incomplete);
                    if incomplete && !s.trim().is_empty() {
                        self.cur = self.buf.len();
                        self.insert_str("\n");
                    } else {
                        self.finish();
                        sys::write_fd(2, b"\r\n");
                        return Input::Line(s);
                    }
                }
                Key::Alt('\n') => self.insert_str("\n"),
                Key::Ctrl('c') => {
                    // fish's `cancel-commandline` (src/reader/reader.rs): on an
                    // empty command line there is nothing to cancel, so do
                    // nothing and keep reading; otherwise discard what was typed
                    // and redraw the prompt in place.  No `^C`, no newline, and
                    // no change to the last exit status.
                    if self.buf.is_empty() && self.menu.is_none() && self.search.is_none() {
                        continue;
                    }
                    self.menu = None;
                    self.search = None;
                    self.suggestion.clear();
                    self.buf.clear();
                    self.cur = 0;
                }
                Key::Ctrl('d') => {
                    if self.buf.is_empty() {
                        self.finish();
                        return Input::Eof;
                    }
                    if self.cur < self.buf.len() {
                        self.buf.remove(self.cur);
                        self.edited();
                    }
                }
                Key::Char(c) => {
                    let mut tmp = [0u8; 4];
                    self.insert_str(c.encode_utf8(&mut tmp));
                }
                Key::Paste(s) => self.insert_str(&s),
                Key::Backspace => {
                    if self.cur > 0 {
                        self.cur -= 1;
                        self.buf.remove(self.cur);
                        self.edited();
                    }
                }
                Key::Delete => {
                    if self.cur < self.buf.len() {
                        self.buf.remove(self.cur);
                        self.edited();
                    }
                }
                Key::Left | Key::Ctrl('b') => {
                    if self.cur > 0 {
                        self.cur -= 1;
                    }
                }
                Key::Right | Key::Ctrl('f') => {
                    if self.cur < self.buf.len() {
                        self.cur += 1;
                    } else {
                        self.accept_suggestion(true);
                    }
                }
                Key::Home | Key::Ctrl('a') => {
                    self.cur = (0..self.cur).rev().find(|&i| self.buf[i] == '\n').map(|i| i + 1).unwrap_or(0);
                }
                Key::End | Key::Ctrl('e') => {
                    if self.cur == self.buf.len() {
                        self.accept_suggestion(true);
                    } else {
                        self.cur = (self.cur..self.buf.len()).find(|&i| self.buf[i] == '\n').unwrap_or(self.buf.len());
                    }
                }
                Key::WordLeft | Key::Alt('b') => self.cur = self.word_left(),
                Key::WordRight | Key::Alt('f') => {
                    if self.cur == self.buf.len() {
                        self.accept_suggestion(false);
                    } else {
                        self.cur = self.word_right();
                    }
                }
                Key::Up | Key::Ctrl('p') => {
                    if !(self.hist_idx.is_none() && self.line_move(true)) {
                        self.hist_move(true);
                    }
                }
                Key::Down | Key::Ctrl('n') => {
                    if !(self.hist_idx.is_none() && self.line_move(false)) {
                        self.hist_move(false);
                    }
                }
                Key::Ctrl('k') => {
                    let end = (self.cur..self.buf.len()).find(|&i| self.buf[i] == '\n').filter(|e| *e > self.cur).unwrap_or(self.buf.len());
                    self.kill = self.buf.drain(self.cur..end).collect();
                    self.edited();
                }
                Key::Ctrl('u') => {
                    let start = (0..self.cur).rev().find(|&i| self.buf[i] == '\n').map(|i| i + 1).unwrap_or(0);
                    self.kill = self.buf.drain(start..self.cur).collect();
                    self.cur = start;
                    self.edited();
                }
                Key::Ctrl('w') => {
                    let mut i = self.cur;
                    while i > 0 && self.buf[i - 1].is_whitespace() {
                        i -= 1;
                    }
                    while i > 0 && !self.buf[i - 1].is_whitespace() {
                        i -= 1;
                    }
                    self.kill = self.buf.drain(i..self.cur).collect();
                    self.cur = i;
                    self.edited();
                }
                Key::AltBackspace => {
                    let i = self.word_left();
                    self.kill = self.buf.drain(i..self.cur).collect();
                    self.cur = i;
                    self.edited();
                }
                Key::Alt('d') => {
                    let j = self.word_right();
                    self.kill = self.buf.drain(self.cur..j).collect();
                    self.edited();
                }
                Key::Ctrl('y') => {
                    let k: String = self.kill.iter().collect();
                    self.insert_str(&k);
                }
                Key::Ctrl('t') => {
                    if self.cur > 0 && self.buf.len() >= 2 {
                        let i = if self.cur == self.buf.len() { self.cur - 1 } else { self.cur };
                        self.buf.swap(i - 1, i);
                        self.cur = i + 1;
                        self.edited();
                    }
                }
                Key::Ctrl('l') => {
                    sys::write_fd(2, format!("\x1b[H\x1b[2J{}", self.head).as_bytes());
                    self.row = 0;
                }
                Key::Ctrl('r') => {
                    self.saved = self.buf.clone();
                    self.search = Some((String::new(), None));
                }
                Key::Alt('.') => {
                    let last = self.sh.history.entries.last().and_then(|e| e.cmd.split_whitespace().last().map(|s| s.to_string()));
                    if let Some(w) = last {
                        self.insert_str(&w);
                    }
                }
                Key::Tab => self.complete(false),
                Key::ShiftTab => {
                    if self.menu.is_some() {
                        self.complete(true);
                    }
                }
                _ => {}
            }
            self.refresh();
        }
    }
}

/// Read a line without the editor (stdin is not a terminal).
pub fn read_plain(prompt: &str) -> Input {
    sys::write_fd(2, prompt.as_bytes());
    let mut bytes = Vec::new();
    loop {
        match sys::read_byte(0) {
            Ok(Some(b'\n')) => break,
            Ok(Some(b)) => bytes.push(b),
            Ok(None) => {
                if bytes.is_empty() {
                    return Input::Eof;
                }
                break;
            }
            Err(e) if e == libc::EINTR => {
                if sys::take_pending(libc::SIGINT) {
                    return Input::Interrupt;
                }
            }
            Err(_) => return Input::Eof,
        }
    }
    Input::Line(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn read_line(sh: &mut Shell, prompt: &str, rprompt: &str) -> Input {
    let orig = match sys::tcgetattr(0) {
        Some(t) => t,
        None => return read_plain(prompt),
    };
    let mut raw = orig;
    raw.c_iflag &= !(sys::BRKINT | sys::ICRNL | sys::INPCK | sys::ISTRIP | sys::IXON);
    raw.c_cflag |= sys::CS8;
    raw.c_lflag &= !(sys::ECHO | sys::ICANON | sys::IEXTEN | sys::ISIG);
    raw.c_cc[sys::VMIN as usize] = 1;
    raw.c_cc[sys::VTIME as usize] = 0;
    let _ = sys::tcsetattr(0, sys::TCSADRAIN, &raw);
    let (head, last) = match prompt.rfind('\n') {
        Some(i) => (&prompt[..=i], &prompt[i + 1..]),
        None => ("", prompt),
    };
    let (cols, _) = sys::term_size();
    // zsh's PROMPT_SP heuristic (Src/utils.c:preprompt): print an end-of-line
    // mark (bold standout `%`), then enough spaces to run the cursor to the
    // start of the next line, then CR, then one space (the mark width) and CR
    // again.  If the previous command's output ended with a newline the cursor
    // is already in column 0, so the mark is overwritten and the prompt starts
    // cleanly; if the output was left dangling on the line the run of spaces
    // wraps first, leaving the mark visible at the end of that line and the
    // prompt on a fresh line.
    let mut intro = String::with_capacity(cols + head.len() + 16);
    intro.push_str("\x1b[?2004h");
    intro.push_str("\x1b[1m\x1b[7m%\x1b[0m");
    intro.extend(std::iter::repeat(' ').take(cols.saturating_sub(1)));
    intro.push('\r');
    intro.push(' ');
    intro.push('\r');
    intro.push_str(head);
    sys::write_fd(2, intro.as_bytes());
    let cwd = sh.cwd();
    let ls = crate::colors::LsColors::parse(&sh.get("LS_COLORS").unwrap_or_default());
    let mut ed = Editor {
        sh,
        prompt: last.to_string(),
        head: head.to_string(),
        rprompt: rprompt.to_string(),
        buf: Vec::new(),
        cur: 0,
        row: 0,
        suggestion: String::new(),
        hist_idx: None,
        hist_prefix: String::new(),
        saved: Vec::new(),
        menu: None,
        kill: Vec::new(),
        search: None,
        cwd,
        ls,
    };
    ed.refresh();
    let r = ed.run();
    sys::write_fd(2, b"\x1b[?2004l");
    let _ = sys::tcsetattr(0, sys::TCSADRAIN, &orig);
    r
}
