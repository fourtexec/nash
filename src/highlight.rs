//! Syntax highlighting for the line editor.

use crate::parser::{is_name, RESERVED};
use crate::shell::Shell;
use crate::sys;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum St {
    Plain,
    Cmd,
    BadCmd,
    Keyword,
    Str,
    Var,
    Op,
    Comment,
    Opt,
    Path,
}

pub fn sgr(st: St) -> &'static str {
    match st {
        St::Plain => "",
        St::Cmd => "\x1b[32m",
        St::BadCmd => "\x1b[31m",
        St::Keyword => "\x1b[35m",
        St::Str => "\x1b[33m",
        St::Var => "\x1b[36m",
        St::Op => "\x1b[1m",
        St::Comment => "\x1b[90m",
        St::Opt => "\x1b[94m",
        St::Path => "\x1b[4m",
    }
}

fn var_end(buf: &[char], i: usize) -> usize {
    let n = buf.len();
    let mut j = i + 1;
    if j >= n {
        return j;
    }
    if buf[j] == '{' {
        while j < n && buf[j] != '}' {
            j += 1;
        }
        return (j + 1).min(n);
    }
    if buf[j].is_ascii_alphabetic() || buf[j] == '_' {
        while j < n && (buf[j].is_ascii_alphanumeric() || buf[j] == '_') {
            j += 1;
        }
        return j;
    }
    if buf[j].is_ascii_digit() || "@*#?$!-".contains(buf[j]) {
        return j + 1;
    }
    j
}

const PREFIX_CMDS: &[&str] = &["sudo", "exec", "command", "builtin", "time", "env", "nohup", "xargs", "doas", "nice", "defer"];

pub fn highlight(sh: &mut Shell, buf: &[char]) -> Vec<St> {
    let n = buf.len();
    let mut st = vec![St::Plain; n];
    let mut i = 0;
    let mut cmd_pos = true;
    let mut redirect_next = false;
    let home = sh.home();
    while i < n {
        let c = buf[i];
        if c.is_whitespace() {
            if c == '\n' {
                cmd_pos = true;
            }
            i += 1;
            continue;
        }
        if c == '#' && (i == 0 || buf[i - 1].is_whitespace()) {
            while i < n && buf[i] != '\n' {
                st[i] = St::Comment;
                i += 1;
            }
            continue;
        }
        if "|&;()".contains(c) {
            st[i] = St::Op;
            cmd_pos = c != ')';
            if c == '&' && i + 1 < n && buf[i + 1] == '>' {
                cmd_pos = false;
            }
            i += 1;
            continue;
        }
        if (c == '<' || c == '>') && !(i + 1 < n && buf[i + 1] == '(') {
            st[i] = St::Op;
            i += 1;
            if i < n && (buf[i] == '&' || buf[i] == '>' || buf[i] == '<' || buf[i] == '|') {
                st[i] = St::Op;
                i += 1;
            }
            redirect_next = true;
            continue;
        }
        let start = i;
        let mut plain = String::new();
        let mut dynamic = false;
        let mut nested = false;
        while i < n && !buf[i].is_whitespace() && !"|&;()".contains(buf[i]) {
            match buf[i] {
                '<' | '>' => {
                    if i + 1 < n && buf[i + 1] == '(' {
                        st[i] = St::Op;
                        st[i + 1] = St::Op;
                        i += 2;
                        nested = true;
                    }
                    break;
                }
                '\\' => {
                    i += 1;
                    if i < n {
                        plain.push(buf[i]);
                        i += 1;
                    }
                }
                '\'' => {
                    let s = i;
                    i += 1;
                    while i < n && buf[i] != '\'' {
                        plain.push(buf[i]);
                        i += 1;
                    }
                    if i < n {
                        i += 1;
                    }
                    for x in st.iter_mut().take(i).skip(s) {
                        *x = St::Str;
                    }
                }
                '"' => {
                    st[i] = St::Str;
                    i += 1;
                    while i < n && buf[i] != '"' {
                        if buf[i] == '\\' && i + 1 < n {
                            plain.push(buf[i + 1]);
                            st[i] = St::Str;
                            st[i + 1] = St::Str;
                            i += 2;
                        } else if buf[i] == '$' && i + 1 < n && buf[i + 1] != '(' {
                            let e = var_end(buf, i);
                            for x in st.iter_mut().take(e).skip(i) {
                                *x = St::Var;
                            }
                            dynamic = true;
                            i = e;
                        } else {
                            plain.push(buf[i]);
                            st[i] = St::Str;
                            i += 1;
                        }
                    }
                    if i < n {
                        st[i] = St::Str;
                        i += 1;
                    }
                }
                '$' => {
                    if i + 1 < n && buf[i + 1] == '(' {
                        st[i] = St::Op;
                        st[i + 1] = St::Op;
                        i += 2;
                        nested = true;
                        break;
                    }
                    let e = var_end(buf, i);
                    for x in st.iter_mut().take(e).skip(i) {
                        *x = St::Var;
                    }
                    dynamic = true;
                    i = e;
                }
                '`' => {
                    st[i] = St::Op;
                    i += 1;
                    nested = true;
                    break;
                }
                ch => {
                    plain.push(ch);
                    i += 1;
                }
            }
        }
        let expand = |p: &str| -> String {
            if p == "~" {
                home.clone()
            } else if let Some(r) = p.strip_prefix("~/") {
                format!("{}/{}", home, r)
            } else {
                p.to_string()
            }
        };
        let style = if redirect_next {
            redirect_next = false;
            St::Plain
        } else if cmd_pos {
            if let Some(eq) = plain.find('=') {
                let name = plain[..eq].trim_end_matches('+');
                let name = name.split('[').next().unwrap_or("");
                if is_name(name) {
                    let name_len = buf[start..].iter().position(|c| *c == '=').unwrap_or(0);
                    for x in st.iter_mut().skip(start).take(name_len) {
                        if *x == St::Plain {
                            *x = St::Var;
                        }
                    }
                    if nested {
                        cmd_pos = true;
                    }
                    continue;
                }
            }
            if RESERVED.contains(&plain.as_str()) && !dynamic {
                cmd_pos = !matches!(plain.as_str(), "for" | "case" | "in" | "function" | "fn" | "esac" | "fi" | "done" | "}" | "[[");
                St::Keyword
            } else {
                cmd_pos = PREFIX_CMDS.contains(&plain.as_str());
                if dynamic || plain.is_empty() {
                    St::Plain
                } else if sh.is_known_command(&plain) || (sh.opts.autocd && (sys::is_dir(&expand(&plain)) || plain.chars().all(|c| c == '.'))) {
                    St::Cmd
                } else {
                    St::BadCmd
                }
            }
        } else if plain.starts_with('-') {
            St::Opt
        } else if !dynamic && !plain.is_empty() && plain.len() < 1024 && std::fs::symlink_metadata(expand(&plain)).is_ok() {
            St::Path
        } else {
            St::Plain
        };
        for x in st.iter_mut().take(i).skip(start) {
            if *x == St::Plain {
                *x = style;
            }
        }
        if nested {
            cmd_pos = true;
        }
    }
    st
}
