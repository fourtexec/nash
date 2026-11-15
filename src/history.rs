//! Command history with context: every entry remembers where it ran, when,
//! how long it took and how it exited.

use std::io::Write;

#[derive(Clone)]
pub struct Entry {
    pub cmd: String,
    pub cwd: String,
    pub ts: i64,
    pub status: i32,
    pub dur_ms: u64,
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
                self.entries.push(Entry { cmd: unescape(cmd), cwd: cwd.to_string(), ts: ts.parse().unwrap_or(0), status: st.parse().unwrap_or(0), dur_ms: dur.parse().unwrap_or(0) });
            }
        }
        if self.entries.len() > self.max {
            let cut = self.entries.len() - self.max;
            self.entries.drain(..cut);
        }
        if lines > self.max * 2 {
            // compact the file
            let body: String = self.entries.iter().map(format_entry).collect();
            let tmp = format!("{}.tmp{}", path, std::process::id());
            if std::fs::write(&tmp, body).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    pub fn add(&mut self, e: Entry) {
        if let Some(last) = self.entries.last() {
            if last.cmd == e.cmd && last.cwd == e.cwd {
                let n = self.entries.len() - 1;
                self.entries[n] = e;
                return;
            }
        }
        if let Some(p) = &self.path {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = f.write_all(format_entry(&e).as_bytes());
            }
        }
        self.entries.push(e);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        if let Some(p) = &self.path {
            let _ = std::fs::write(p, "");
        }
    }

    pub fn flush(&mut self) {}

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
