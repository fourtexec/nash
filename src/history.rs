//! Command history with context: every entry remembers where it ran, when,
//! how long it took and how it exited.

use std::io::Write;

#[derive(Clone)]
pub struct Entry {
    pub cmd: String,
    /// `cmd` lowercased, cached so case-insensitive history search never has to
    /// allocate or re-fold the same entry twice.
    pub lc: String,
    pub cwd: String,
    pub ts: i64,
    pub status: i32,
    pub dur_ms: u64,
}

impl Entry {
    pub fn new(cmd: String, cwd: String, ts: i64, status: i32, dur_ms: u64) -> Entry {
        let lc = cmd.to_lowercase();
        Entry { cmd, lc, cwd, ts, status, dur_ms }
    }

    /// Whether the command contains `needle`. When `ignore_case` is set the
    /// needle must already be lowercased (smartcase), matching `self.lc`.
    #[inline]
    pub fn matches(&self, needle: &str, ignore_case: bool) -> bool {
        if ignore_case {
            self.lc.contains(needle)
        } else {
            self.cmd.contains(needle)
        }
    }
}

pub struct History {
    pub entries: Vec<Entry>,
    pub path: Option<String>,
    pub max: usize,
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn format_entry(e: &Entry) -> String {
    format!("{}\t{}\t{}\t{}\t{}\n", e.ts, e.dur_ms, e.status, e.cwd.replace('\t', " "), escape(&e.cmd))
}

impl History {
    pub fn new() -> History {
        History { entries: Vec::new(), path: None, max: 10000 }
    }

    pub fn load(&mut self, path: &str) {
        self.path = Some(path.to_string());
        let data = match std::fs::read(path) {
            Ok(d) => String::from_utf8_lossy(&d).into_owned(),
            Err(_) => return,
        };
        let mut lines = 0;
        for line in data.lines() {
            lines += 1;
            let mut it = line.splitn(5, '\t');
            if let (Some(ts), Some(dur), Some(st), Some(cwd), Some(cmd)) = (it.next(), it.next(), it.next(), it.next(), it.next()) {
                self.entries.push(Entry::new(unescape(cmd), cwd.to_string(), ts.parse().unwrap_or(0), st.parse().unwrap_or(0), dur.parse().unwrap_or(0)));
            }
        }
        if self.entries.len() > self.max {
            let cut = self.entries.len() - self.max;
            self.entries.drain(..cut);
        }
        if lines > self.max * 2 {
            // compact the file atomically with restrictive permissions
            let body: String = self.entries.iter().map(format_entry).collect();
            let _ = crate::sys::write_atomic_private(path, body.as_bytes());
        }
    }

    pub fn add(&mut self, e: Entry) {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(last) = self.entries.last() {
            if last.cmd == e.cmd && last.cwd == e.cwd {
                let n = self.entries.len() - 1;
                self.entries[n] = e;
                return;
            }
        }
        if let Some(p) = &self.path {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(p) {
                let _ = f.write_all(format_entry(&e).as_bytes());
            }
        }
        self.entries.push(e);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        if let Some(p) = &self.path {
            let _ = crate::sys::write_atomic_private(p, b"");
        }
    }

    pub fn flush(&mut self) {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(p) = &self.path {
            // Ensure buffered/queued writes reach disk.
            if let Ok(f) = std::fs::OpenOptions::new().append(true).create(true).mode(0o600).open(p) {
                let _ = f.sync_all();
            }
        }
    }
    /// Most recent entry starting with `prefix`, preferring ones run in `cwd`.
    pub fn suggest(&self, prefix: &str, cwd: &str) -> Option<&str> {
        let mut fallback = None;
        for e in self.entries.iter().rev() {
            if e.cmd.len() > prefix.len() && e.cmd.starts_with(prefix) && !e.cmd.contains('\n') {
                if e.cwd == cwd {
                    return Some(&e.cmd);
                }
                if fallback.is_none() {
                    fallback = Some(e.cmd.as_str());
                }
            }
        }
        fallback
    }
}
