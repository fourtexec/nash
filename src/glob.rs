//! Shell pattern matching (with extended globs) and pathname expansion.

#[derive(Clone)]
enum Tok {
    Ch(char),
    Any,
    Star,
    Set(bool, Vec<(char, char)>, Vec<fn(char) -> bool>),
    Ext(char, Vec<Vec<Tok>>),
}

fn find_close(p: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open + 1;
    while i < p.len() {
        match p[i] {
            '\\' => i += 1,
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_alts(p: &[char]) -> Vec<&[char]> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '\\' => i += 1,
            '(' => depth += 1,
            ')' => depth -= 1,
            '|' if depth == 0 => {
                out.push(&p[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&p[start.min(p.len())..]);
    out
}

fn class_fn(name: &str) -> Option<fn(char) -> bool> {
    Some(match name {
        "alpha" => |c: char| c.is_alphabetic(),
        "digit" => |c: char| c.is_ascii_digit(),
        "alnum" => |c: char| c.is_alphanumeric(),
        "upper" => |c: char| c.is_uppercase(),
        "lower" => |c: char| c.is_lowercase(),
        "space" => |c: char| c.is_whitespace(),
        "blank" => |c: char| c == ' ' || c == '\t',
        "punct" => |c: char| c.is_ascii_punctuation(),
        "xdigit" => |c: char| c.is_ascii_hexdigit(),
        "cntrl" => |c: char| c.is_control(),
        "print" => |c: char| !c.is_control(),
        "graph" => |c: char| !c.is_control() && c != ' ',
        _ => return None,
    })
}

fn parse_set(p: &[char], start: usize) -> Option<(Tok, usize)> {
    let mut j = start + 1;
    let mut neg = false;
    if j < p.len() && (p[j] == '!' || p[j] == '^') {
        neg = true;
        j += 1;
    }
    let mut ranges = Vec::new();
    let mut classes = Vec::new();
    let mut first = true;
    loop {
        if j >= p.len() {
            return None;
        }
        let mut c = p[j];
        if c == ']' && !first {
            return Some((Tok::Set(neg, ranges, classes), j + 1));
        }
        first = false;
        if c == '[' && j + 1 < p.len() && p[j + 1] == ':' {
            if let Some(end) = (j + 2..p.len().saturating_sub(1)).find(|&k| p[k] == ':' && p[k + 1] == ']') {
                let name: String = p[j + 2..end].iter().collect();
                if let Some(f) = class_fn(&name) {
                    classes.push(f);
                    j = end + 2;
                    continue;
                }
            }
        }
        if c == '\\' && j + 1 < p.len() {
            j += 1;
            c = p[j];
        }
        if j + 2 < p.len() && p[j + 1] == '-' && p[j + 2] != ']' {
            let mut hi = p[j + 2];
            j += 2;
            if hi == '\\' && j + 1 < p.len() {
                j += 1;
                hi = p[j];
            }
            ranges.push((c, hi));
        } else {
            ranges.push((c, c));
        }
        j += 1;
    }
}

fn compile(p: &[char]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < p.len() {
        let c = p[i];
        if matches!(c, '?' | '*' | '+' | '@' | '!') && i + 1 < p.len() && p[i + 1] == '(' {
            if let Some(end) = find_close(p, i + 1) {
                let alts = split_alts(&p[i + 2..end]).into_iter().map(compile).collect();
                out.push(Tok::Ext(c, alts));
                i = end + 1;
                continue;
            }
        }
        match c {
            '\\' if i + 1 < p.len() => {
                out.push(Tok::Ch(p[i + 1]));
                i += 2;
                continue;
            }
            '?' => out.push(Tok::Any),
            '*' => {
                if !matches!(out.last(), Some(Tok::Star)) {
                    out.push(Tok::Star);
                }
            }
            '[' => match parse_set(p, i) {
                Some((t, next)) => {
                    out.push(t);
                    i = next;
                    continue;
                }
                None => out.push(Tok::Ch('[')),
            },
            _ => out.push(Tok::Ch(c)),
        }
        i += 1;
    }
    out
}

fn m(mut t: &[Tok], mut s: &[char]) -> bool {
    loop {
        match t.first() {
            None => return s.is_empty(),
            Some(Tok::Ch(c)) => {
                if s.first() != Some(c) {
                    return false;
                }
            }
            Some(Tok::Any) => {
                if s.is_empty() {
                    return false;
                }
            }
            Some(Tok::Set(neg, ranges, classes)) => {
                let c = match s.first() {
                    Some(c) => *c,
                    None => return false,
                };
                let hit = ranges.iter().any(|(a, b)| *a <= c && c <= *b) || classes.iter().any(|f| f(c));
                if hit == *neg {
                    return false;
                }
            }
            Some(Tok::Star) => {
                let rest = &t[1..];
                if rest.is_empty() {
                    return true;
                }
                if let Some(Tok::Ch(c)) = rest.first() {
                    for k in 0..s.len() {
                        if s[k] == *c && m(rest, &s[k..]) {
                            return true;
                        }
                    }
                    return false;
                }
                for k in 0..=s.len() {
                    if m(rest, &s[k..]) {
                        return true;
                    }
                }
                return false;
            }
            Some(Tok::Ext(kind, alts)) => {
                let rest = &t[1..];
                let any = |sub: &[char]| alts.iter().any(|a| m(a, sub));
                return match kind {
                    '@' => (0..=s.len()).any(|k| any(&s[..k]) && m(rest, &s[k..])),
                    '?' => m(rest, s) || (0..=s.len()).any(|k| any(&s[..k]) && m(rest, &s[k..])),
                    '*' => m(rest, s) || (1..=s.len()).any(|k| any(&s[..k]) && m(t, &s[k..])),
                    '+' => (0..=s.len()).any(|k| any(&s[..k]) && (m(rest, &s[k..]) || (k > 0 && m(t, &s[k..])))),
                    _ => (0..=s.len()).any(|k| !any(&s[..k]) && m(rest, &s[k..])),
                };
            }
        }
        t = &t[1..];
        s = &s[1..];
    }
}

pub struct Pattern(Vec<Tok>);

impl Pattern {
    pub fn new(p: &str) -> Pattern {
        let cs: Vec<char> = p.chars().collect();
        Pattern(compile(&cs))
    }
    pub fn matches(&self, s: &str) -> bool {
        let cs: Vec<char> = s.chars().collect();
        m(&self.0, &cs)
    }
    fn literal(&self) -> Option<String> {
        let mut s = String::new();
        for t in &self.0 {
            match t {
                Tok::Ch(c) => s.push(*c),
                _ => return None,
            }
        }
        Some(s)
    }
}

pub fn matches(pat: &str, s: &str) -> bool {
    Pattern::new(pat).matches(s)
}

/// `${var#pat}` / `${var%pat}` and friends.
pub fn trim(s: &str, pat: &str, prefix: bool, longest: bool) -> String {
    let p = Pattern::new(pat);
    let cs: Vec<char> = s.chars().collect();
    let n = cs.len();
    let order: Vec<usize> = if longest { (0..=n).rev().collect() } else { (0..=n).collect() };
    for k in order {
        if prefix {
            if m(&p.0, &cs[..k]) {
                return cs[k..].iter().collect();
            }
        } else if m(&p.0, &cs[n - k..]) {
            return cs[..n - k].iter().collect();
        }
    }
    s.to_string()
}

/// `${var/pat/rep}`; mode: '/' first, 'a' all, '#' anchored at start, '%' anchored at end.
pub fn replace(s: &str, pat: &str, rep: &str, mode: char) -> String {
    let p = Pattern::new(pat);
    if p.0.is_empty() {
        return match mode {
            '#' => format!("{}{}", rep, s),
            '%' => format!("{}{}", s, rep),
            _ => s.to_string(),
        };
    }
    if let Some(lit) = p.literal() {
        return match mode {
            'a' => s.replace(&lit, rep),
            '/' => s.replacen(&lit, rep, 1),
            '#' => match s.strip_prefix(&lit) {
                Some(r) => format!("{}{}", rep, r),
                None => s.to_string(),
            },
            _ => match s.strip_suffix(&lit) {
                Some(r) => format!("{}{}", r, rep),
                None => s.to_string(),
            },
        };
    }
    let cs: Vec<char> = s.chars().collect();
    let n = cs.len();
    match mode {
        '#' => {
            for k in (0..=n).rev() {
                if m(&p.0, &cs[..k]) {
                    return format!("{}{}", rep, cs[k..].iter().collect::<String>());
                }
            }
            s.to_string()
        }
        '%' => {
            for k in 0..=n {
                if m(&p.0, &cs[k..]) {
                    return format!("{}{}", cs[..k].iter().collect::<String>(), rep);
                }
            }
            s.to_string()
        }
        _ => {
            let first_ch = match p.0.first() {
                Some(Tok::Ch(c)) => Some(*c),
                _ => None,
            };
            let mut out = String::new();
            let mut i = 0;
            let mut done = false;
            while i < n {
                let mut matched = None;
                if !done && first_ch.map(|c| c == cs[i]).unwrap_or(true) {
                    for j in (i + 1..=n).rev() {
                        if m(&p.0, &cs[i..j]) {
                            matched = Some(j);
                            break;
                        }
                    }
                }
                match matched {
                    Some(j) => {
                        out.push_str(rep);
                        i = j;
                        if mode != 'a' {
                            done = true;
                        }
                    }
                    None => {
                        out.push(cs[i]);
                        i += 1;
                    }
                }
            }
            out
        }
    }
}

/// Does this pattern component contain active glob characters?
pub fn has_magic(p: &str) -> bool {
    let cs: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '\\' => i += 1,
            '*' | '?' | '[' => return true,
            '+' | '@' | '!' if cs.get(i + 1) == Some(&'(') => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn unescape(p: &str) -> String {
    let mut out = String::new();
    let mut it = p.chars();
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

fn list_dir(dir: &str) -> Vec<(String, bool)> {
    let d = if dir.is_empty() { "." } else { dir };
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            v.push((name, is_dir));
        }
    }
    v.sort();
    v
}

fn walk(base: &str, comps: &[String], out: &mut Vec<String>, dotglob: bool) {
    let comp = &comps[0];
    let rest = &comps[1..];
    if comp == "**" {
        if rest.is_empty() {
            fn all(base: &str, out: &mut Vec<String>, dotglob: bool) {
                for (name, is_dir) in list_dir(base) {
                    if name.starts_with('.') && !dotglob {
                        continue;
                    }
                    let path = format!("{}{}", base, name);
                    out.push(path.clone());
                    if is_dir {
                        all(&format!("{}/", path), out, dotglob);
                    }
                }
            }
            all(base, out, dotglob);
        } else {
            walk(base, rest, out, dotglob);
            for (name, is_dir) in list_dir(base) {
                if is_dir && (dotglob || !name.starts_with('.')) {
                    walk(&format!("{}{}/", base, name), comps, out, dotglob);
                }
            }
        }
        return;
    }
    if !has_magic(comp) {
        let path = format!("{}{}", base, unescape(comp));
        if rest.is_empty() {
            let check = if path.is_empty() { "." } else { &path };
            if std::fs::symlink_metadata(check).is_ok() {
                out.push(path);
            }
        } else if comp.is_empty() || crate::sys::is_dir(&path) {
            walk(&format!("{}/", path), rest, out, dotglob);
        }
        return;
    }
    let pat = Pattern::new(comp);
    let want_dot = comp.starts_with('.') || comp.starts_with("\\.");
    for (name, _) in list_dir(base) {
        if name.starts_with('.') && !want_dot && !dotglob {
            continue;
        }
        if !pat.matches(&name) {
            continue;
        }
        let path = format!("{}{}", base, name);
        if rest.is_empty() {
            out.push(path);
        } else if crate::sys::is_dir(&path) {
            walk(&format!("{}/", path), rest, out, dotglob);
        }
    }
}

/// Expand a pathname pattern. Returns an empty vector if nothing matches.
pub fn glob(pat: &str, dotglob: bool) -> Vec<String> {
    // split on unescaped '/'
    let mut comps: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut it = pat.chars().peekable();
    let mut depth = 0;
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            '/' if depth <= 0 => comps.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    comps.push(cur);
    let mut out = Vec::new();
    let (base, comps) = if comps.len() > 1 && comps[0].is_empty() {
        let mut k = 1;
        while k < comps.len() - 1 && comps[k].is_empty() {
            k += 1;
        }
        ("/".to_string(), &comps[k..])
    } else {
        (String::new(), &comps[..])
    };
    if comps.len() == 1 && comps[0].is_empty() {
        return out;
    }
    walk(&base, comps, &mut out, dotglob);
    out.sort();
    out.dedup();
    out
}
