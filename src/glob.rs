//! Shell pattern matching (with extended globs) and pathname expansion.

const MAX_EXTGLOB_DEPTH: u32 = 64;
const MAX_GLOB_DEPTH: usize = 128;
const MAX_GLOB_RESULTS: usize = 100_000;
const MAX_MEMO_CELLS: usize = 4_000_000;

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

fn compile_depth(p: &[char], depth: u32) -> (Vec<Tok>, bool) {
    let mut out = Vec::with_capacity(p.len());
    let mut has_ext = false;
    let mut i = 0;
    while i < p.len() {
        let c = p[i];
        if depth < MAX_EXTGLOB_DEPTH && matches!(c, '?' | '*' | '+' | '@' | '!') && i + 1 < p.len() && p[i + 1] == '(' {
            if let Some(end) = find_close(p, i + 1) {
                let alts = split_alts(&p[i + 2..end])
                    .into_iter()
                    .map(|a| compile_depth(a, depth + 1).0)
                    .collect();
                out.push(Tok::Ext(c, alts));
                has_ext = true;
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
    (out, has_ext)
}

#[inline]
fn match_one_tok(tok: &Tok, c: char) -> bool {
    match tok {
        Tok::Ch(ch) => *ch == c,
        Tok::Any => true,
        Tok::Set(neg, ranges, classes) => {
            let hit = ranges.iter().any(|(a, b)| *a <= c && c <= *b) || classes.iter().any(|f| f(c));
            hit != *neg
        }
        _ => false,
    }
}

/// Iterative O(|t| * |s|) matcher for patterns without `Tok::Ext`.
/// Backtracking only to the most recent `Tok::Star` is exact because all other
/// tokens consume exactly one character.
fn match_no_ext(t: &[Tok], s: &[char]) -> bool {
    let (mut ti, mut si) = (0usize, 0usize);
    let (mut star_t, mut star_s) = (None::<usize>, 0usize);
    while si < s.len() {
        if ti < t.len() {
            match &t[ti] {
                Tok::Star => {
                    star_t = Some(ti);
                    ti += 1;
                    star_s = si;
                    continue;
                }
                tok if match_one_tok(tok, s[si]) => {
                    ti += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(st) = star_t {
            ti = st + 1;
            star_s += 1;
            si = star_s;
        } else {
            return false;
        }
    }
    while ti < t.len() && matches!(t[ti], Tok::Star) {
        ti += 1;
    }
    ti == t.len()
}

/// Memoized matcher for patterns that contain `Tok::Ext`.
fn m_memo(t: &[Tok], s: &[char], ti: usize, si: usize, memo: &mut [u8]) -> bool {
    let stride = s.len() + 1;
    let idx = ti * stride + si;
    if !memo.is_empty() {
        let cached = memo[idx];
        if cached != 0 {
            return cached == 2;
        }
    }
    let res = match t.get(ti) {
        None => si == s.len(),
        Some(Tok::Star) => {
            if ti + 1 == t.len() {
                true
            } else {
                // Star can consume 0 chars (advance ti) or 1+ chars (advance si)
                m_memo(t, s, ti + 1, si, memo) || (si < s.len() && m_memo(t, s, ti, si + 1, memo))
            }
        }
        Some(Tok::Ext(kind, alts)) => {
            let rem = &s[si..];
            let any = |sub: &[char]| alts.iter().any(|a| match_tokens(a, sub));
            match kind {
                '@' => (0..=rem.len()).any(|k| any(&rem[..k]) && m_memo(t, s, ti + 1, si + k, memo)),
                '?' => m_memo(t, s, ti + 1, si, memo) || (0..=rem.len()).any(|k| any(&rem[..k]) && m_memo(t, s, ti + 1, si + k, memo)),
                '*' => m_memo(t, s, ti + 1, si, memo) || (1..=rem.len()).any(|k| any(&rem[..k]) && m_memo(t, s, ti, si + k, memo)),
                '+' => (0..=rem.len()).any(|k| any(&rem[..k]) && (m_memo(t, s, ti + 1, si + k, memo) || (k > 0 && m_memo(t, s, ti, si + k, memo)))),
                _ => (0..=rem.len()).any(|k| !any(&rem[..k]) && m_memo(t, s, ti + 1, si + k, memo)),
            }
        }
        Some(tok) => si < s.len() && match_one_tok(tok, s[si]) && m_memo(t, s, ti + 1, si + 1, memo),
    };
    if !memo.is_empty() {
        memo[idx] = if res { 2 } else { 1 };
    }
    res
}

fn match_tokens_with_flag(t: &[Tok], has_ext: bool, s: &[char]) -> bool {
    if !has_ext {
        return match_no_ext(t, s);
    }
    let cells = (t.len() + 1).saturating_mul(s.len() + 1);
    let mut memo = if cells <= MAX_MEMO_CELLS { vec![0u8; cells] } else { Vec::new() };
    m_memo(t, s, 0, 0, &mut memo)
}

fn match_tokens(t: &[Tok], s: &[char]) -> bool {
    let has_ext = t.iter().any(|tok| matches!(tok, Tok::Ext(_, _)));
    match_tokens_with_flag(t, has_ext, s)
}

/// Zero-allocation iterative byte matcher for patterns with `*` and optionally `?`
/// (when `allow_q` is true, caller guarantees both `p` and `s` are ASCII).
fn match_simple_bytes(p: &[u8], s: &[u8], allow_q: bool) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star_p, mut star_s) = (None::<usize>, 0usize);
    while si < s.len() {
        if pi < p.len() && ((allow_q && p[pi] == b'?') || (p[pi] != b'*' && p[pi] == s[si])) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star_p = Some(pi);
            pi += 1;
            star_s = si;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_s += 1;
            si = star_s;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

pub struct Pattern {
    toks: Vec<Tok>,
    has_ext: bool,
}

impl Pattern {
    pub fn new(p: &str) -> Pattern {
        let cs: Vec<char> = p.chars().collect();
        let (toks, has_ext) = compile_depth(&cs, 0);
        Pattern { toks, has_ext }
    }
    pub fn matches(&self, s: &str) -> bool {
        let cs: Vec<char> = s.chars().collect();
        match_tokens_with_flag(&self.toks, self.has_ext, &cs)
    }
    fn literal(&self) -> Option<String> {
        let mut s = String::new();
        for t in &self.toks {
            match t {
                Tok::Ch(c) => s.push(*c),
                _ => return None,
            }
        }
        Some(s)
    }
}

pub fn matches(pat: &str, s: &str) -> bool {
    let pb = pat.as_bytes();
    let mut has_complex = false;
    let mut has_q = false;
    let mut has_star = false;
    for &b in pb {
        match b {
            b'\\' | b'[' | b'(' => {
                has_complex = true;
                break;
            }
            b'?' => has_q = true,
            b'*' => has_star = true,
            _ => {}
        }
    }
    if !has_complex {
        if !has_star && !has_q {
            return pat == s;
        }
        if !has_q {
            return match_simple_bytes(pb, s.as_bytes(), false);
        }
        if pat.is_ascii() && s.is_ascii() {
            return match_simple_bytes(pb, s.as_bytes(), true);
        }
    }
    Pattern::new(pat).matches(s)
}

/// `${var#pat}` / `${var%pat}` and friends.
pub fn trim(s: &str, pat: &str, prefix: bool, longest: bool) -> String {
    let p = Pattern::new(pat);
    let cs: Vec<char> = s.chars().collect();
    let n = cs.len();
    if longest {
        for k in (0..=n).rev() {
            if prefix {
                if match_tokens_with_flag(&p.toks, p.has_ext, &cs[..k]) {
                    return cs[k..].iter().collect();
                }
            } else if match_tokens_with_flag(&p.toks, p.has_ext, &cs[n - k..]) {
                return cs[..n - k].iter().collect();
            }
        }
    } else {
        for k in 0..=n {
            if prefix {
                if match_tokens_with_flag(&p.toks, p.has_ext, &cs[..k]) {
                    return cs[k..].iter().collect();
                }
            } else if match_tokens_with_flag(&p.toks, p.has_ext, &cs[n - k..]) {
                return cs[..n - k].iter().collect();
            }
        }
    }
    s.to_string()
}

/// `${var/pat/rep}`; mode: '/' first, 'a' all, '#' anchored at start, '%' anchored at end.
pub fn replace(s: &str, pat: &str, rep: &str, mode: char) -> String {
    let p = Pattern::new(pat);
    if p.toks.is_empty() {
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
                if match_tokens_with_flag(&p.toks, p.has_ext, &cs[..k]) {
                    return format!("{}{}", rep, cs[k..].iter().collect::<String>());
                }
            }
            s.to_string()
        }
        '%' => {
            for k in 0..=n {
                if match_tokens_with_flag(&p.toks, p.has_ext, &cs[k..]) {
                    return format!("{}{}", cs[..k].iter().collect::<String>(), rep);
                }
            }
            s.to_string()
        }
        _ => {
            let first_ch = match p.toks.first() {
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
                        if match_tokens_with_flag(&p.toks, p.has_ext, &cs[i..j]) {
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
    let b = p.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'*' | b'?' | b'[' => return true,
            b'+' | b'@' | b'!' if b.get(i + 1) == Some(&b'(') => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn unescape(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
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

fn walk(base: &str, comps: &[String], out: &mut Vec<String>, dotglob: bool, depth: usize) {
    if depth >= MAX_GLOB_DEPTH || out.len() >= MAX_GLOB_RESULTS {
        return;
    }
    let comp = &comps[0];
    let rest = &comps[1..];
    if comp == "**" {
        if rest.is_empty() {
            fn all(base: &str, out: &mut Vec<String>, dotglob: bool, depth: usize) {
                if depth >= MAX_GLOB_DEPTH || out.len() >= MAX_GLOB_RESULTS {
                    return;
                }
                for (name, is_dir) in list_dir(base) {
                    if out.len() >= MAX_GLOB_RESULTS {
                        return;
                    }
                    if name.starts_with('.') && !dotglob {
                        continue;
                    }
                    let path = format!("{}{}", base, name);
                    out.push(path.clone());
                    if is_dir {
                        all(&format!("{}/", path), out, dotglob, depth + 1);
                    }
                }
            }
            all(base, out, dotglob, depth);
        } else {
            walk(base, rest, out, dotglob, depth + 1);
            for (name, is_dir) in list_dir(base) {
                if out.len() >= MAX_GLOB_RESULTS {
                    return;
                }
                if is_dir && (dotglob || !name.starts_with('.')) {
                    walk(&format!("{}{}/", base, name), comps, out, dotglob, depth + 1);
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
            walk(&format!("{}/", path), rest, out, dotglob, depth + 1);
        }
        return;
    }
    let pat = Pattern::new(comp);
    let want_dot = comp.starts_with('.') || comp.starts_with("\\.");
    for (name, _) in list_dir(base) {
        if out.len() >= MAX_GLOB_RESULTS {
            return;
        }
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
            walk(&format!("{}/", path), rest, out, dotglob, depth + 1);
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
    walk(&base, comps, &mut out, dotglob, 0);
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redos_resistance() {
        // Pathological multi-star pattern (previously exponential).
        let pat = "*a".repeat(25) + "*b";
        let input = "a".repeat(60) + "c";
        assert!(!matches(&pat, &input));

        // Same via compiled Pattern with character class (non-ASCII-fast-path).
        let pat_set = "*[a]".repeat(25) + "*b";
        assert!(!Pattern::new(&pat_set).matches(&input));

        // Pathological extglob repetition.
        let ext_input = "a".repeat(35) + "c";
        assert!(!matches("*(a|aa)b", &ext_input));
    }

    #[test]
    fn extglob_and_depth_safety() {
        assert!(matches("@(foo|bar)", "foo"));
        assert!(matches("@(foo|bar)", "bar"));
        assert!(!matches("@(foo|bar)", "baz"));
        assert!(matches("+(ab)", "ababab"));
        assert!(!matches("+(ab)", ""));
        assert!(matches("?(a)b", "b"));
        assert!(matches("?(a)b", "ab"));
        assert!(matches("!(foo)", "bar"));
        assert!(!matches("!(foo)", "foo"));

        // Deeply nested extglob does not overflow stack.
        let deep = "@(".repeat(100) + "a" + &")".repeat(100);
        let _ = matches(&deep, "a");
    }
}
