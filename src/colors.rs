//! File-type colours: a rich default `LS_COLORS`, colour-enabling aliases for
//! `ls` and friends, and lookups used to colour the completion menu.

use crate::shell::Shell;
use std::collections::HashMap;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::rc::Rc;

/// (colour, extensions) groups used to build the default LS_COLORS.
const GROUPS: &[(&str, &str)] = &[
    ("1;31", "tar tgz gz bz2 xz zst zip 7z rar deb rpm jar iso dmg tbz2 txz lz4 cab apk"),
    ("35", "jpg jpeg png gif bmp svg webp ico tif tiff heic avif psd xcf"),
    ("1;35", "mp4 mkv avi mov webm flv wmv m4v mpg mpeg"),
    ("36", "mp3 flac wav ogg opus m4a aac wma mid"),
    ("33", "md txt pdf doc docx odt rtf tex epub rst org xls xlsx ods csv ppt pptx"),
    ("92", "rs c h cc cpp hpp go py rb js ts jsx tsx java kt swift cs php lua pl hs ml ex erl zig nim scala sql r jl dart vue"),
    ("32", "sh bash zsh nash fish awk sed ps1 bat"),
    ("93", "json toml yaml yml ini conf cfg xml html htm css scss lock env plist"),
    ("90", "bak tmp swp swo log old orig pyc o a so class cache"),
];

pub fn default_ls_colors() -> String {
    let mut s = String::from("rs=0:di=1;34:ln=1;36:or=1;31:pi=33:so=1;35:bd=1;33:cd=1;33:ex=1;32:su=37;41:sg=30;43:tw=30;42:ow=34;42:st=37;44:*~=90:*Makefile=1;33:*Dockerfile=1;33:*README=1;33:*LICENSE=33:");
    for (color, exts) in GROUPS {
        for e in exts.split(' ') {
            s.push_str(&format!("*.{}={}:", e, color));
        }
    }
    s
}

/// Called for interactive shells before the rc file is read, so everything
/// here can be overridden (`unalias ls`, `export LS_COLORS=...`).
pub fn setup(sh: &mut Shell) {
    if sh.get("NO_COLOR").is_some() || sh.get("TERM").as_deref() == Some("dumb") {
        return;
    }
    if sh.get("LS_COLORS").map(|v| v.is_empty()).unwrap_or(true) {
        let _ = sh.set_str("LS_COLORS", default_ls_colors());
        sh.export_var("LS_COLORS");
    }
    let quiet = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
    let a = Rc::make_mut(&mut sh.aliases);
    let mut add = |k: &str, v: &str| {
        a.entry(k.to_string()).or_insert_with(|| v.to_string());
    };
    if quiet("ls", &["--color=auto", "-d", "/"]) {
        // GNU ls
        add("ls", "ls --color=auto");
        for c in ["dir", "vdir"] {
            add(c, &format!("{} --color=auto", c));
        }
    } else {
        // BSD / macOS ls
        add("ls", "ls -G");
        let _ = sh.set_str("CLICOLOR", "1");
        sh.export_var("CLICOLOR");
    }
    let a = Rc::make_mut(&mut sh.aliases);
    let mut add = |k: &str, v: &str| {
        a.entry(k.to_string()).or_insert_with(|| v.to_string());
    };
    add("la", "ls -A");
    add("ll", "ls -lh");
    add("lla", "ls -lAh");
    add("l", "ls -CF");
    if quiet("grep", &["--color=auto", "-q", "x", "/dev/null"]) || quiet("sh", &["-c", "echo x | grep --color=auto -q x"]) {
        for c in ["grep", "egrep", "fgrep"] {
            add(c, &format!("{} --color=auto", c));
        }
    }
    if quiet("sh", &["-c", "diff --color=auto /dev/null /dev/null"]) {
        add("diff", "diff --color=auto");
    }
    if quiet("sh", &["-c", "command -v tree"]) {
        add("tree", "tree -C");
    }
}

/// Parsed LS_COLORS.
pub struct LsColors {
    types: HashMap<String, String>,
    suffixes: Vec<(String, String)>,
}

impl LsColors {
    pub fn parse(s: &str) -> LsColors {
        let mut types = HashMap::new();
        let mut suffixes = Vec::new();
        for item in s.split(':') {
            if let Some((k, v)) = item.split_once('=') {
                if v.is_empty() || !v.chars().all(|c| c.is_ascii_digit() || c == ';') {
                    continue;
                }
                match k.strip_prefix('*') {
                    Some(suf) => suffixes.push((suf.to_lowercase(), v.to_string())),
                    None => {
                        types.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }
        // longest suffix wins (*.tar.gz before *.gz)
        suffixes.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        LsColors { types, suffixes }
    }

    fn ty(&self, k: &str) -> Option<String> {
        self.types.get(k).filter(|v| *v != "0" && *v != "00").cloned()
    }

    /// SGR parameters for a file, or an empty string for the default colour.
    pub fn color_for(&self, path: &str, name: &str) -> String {
        let r = (|| {
            let lm = std::fs::symlink_metadata(path).ok()?;
            let ft = lm.file_type();
            if ft.is_symlink() {
                return if std::fs::metadata(path).is_err() { self.ty("or").or_else(|| self.ty("ln")) } else { self.ty("ln") };
            }
            let mode = lm.permissions().mode();
            if ft.is_dir() {
                let (sticky, ow) = (mode & 0o1000 != 0, mode & 0o002 != 0);
                return match (sticky, ow) {
                    (true, true) => self.ty("tw"),
                    (false, true) => self.ty("ow"),
                    (true, false) => self.ty("st"),
                    _ => None,
                }
                .or_else(|| self.ty("di"));
            }
            if ft.is_fifo() {
                return self.ty("pi");
            }
            if ft.is_socket() {
                return self.ty("so");
            }
            if ft.is_block_device() {
                return self.ty("bd");
            }
            if ft.is_char_device() {
                return self.ty("cd");
            }
            if mode & 0o4000 != 0 {
                if let Some(c) = self.ty("su") {
                    return Some(c);
                }
            }
            if mode & 0o2000 != 0 {
                if let Some(c) = self.ty("sg") {
                    return Some(c);
                }
            }
            if mode & 0o111 != 0 {
                if let Some(c) = self.ty("ex") {
                    return Some(c);
                }
            }
            let lower = name.to_lowercase();
            self.suffixes.iter().find(|(s, _)| lower.ends_with(s.as_str())).map(|(_, c)| c.clone()).or_else(|| self.ty("fi"))
        })();
        r.unwrap_or_default()
    }
}
