//! `nash install`: copy the binary into place, register it in /etc/shells and
//! make it the user's login shell.

use crate::sys;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

const HELP: &str = "\
usage: nash install [--prefix DIR] [--no-chsh]

Installs nash as your main shell:
  1. copies the nash binary to DIR/bin (default /usr/local, or ~/.local
     when /usr/local/bin is not writable)
  2. adds it to /etc/shells
  3. makes it your login shell with chsh

  --prefix DIR   install into DIR/bin
  --no-chsh      only install the binary, do not change the login shell
";

fn writable_dir(dir: &str) -> bool {
    sys::is_dir(dir) && sys::access(dir, libc::W_OK)
}

fn run(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).status().map(|s| s.success()).unwrap_or(false)
}

fn has(cmd: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {} >/dev/null 2>&1", cmd)]).status().map(|s| s.success()).unwrap_or(false)
}

pub fn install(args: &[String]) -> i32 {
    let mut prefix: Option<String> = None;
    let mut chsh = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prefix" => {
                i += 1;
                match args.get(i) {
                    Some(p) => prefix = Some(p.clone()),
                    None => {
                        err!("nash install: --prefix requires a directory\n");
                        return 2;
                    }
                }
            }
            "--no-chsh" => chsh = false,
            "-h" | "--help" => {
                out!("{}", HELP);
                return 0;
            }
            other => {
                err!("nash install: unknown option `{}'\n{}", other, HELP);
                return 2;
            }
        }
        i += 1;
    }
    let (user, home) = sys::current_user();
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty()).or(home).unwrap_or_else(|| "/".into());
    let root = unsafe { libc::geteuid() } == 0;

    // 1. the binary
    let bindir = match prefix {
        Some(p) => format!("{}/bin", p.trim_end_matches('/')),
        None if root || writable_dir("/usr/local/bin") => "/usr/local/bin".to_string(),
        None => format!("{}/.local/bin", home),
    };
    let dest = format!("{}/nash", bindir);
    let src = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(e) => {
            err!("nash install: cannot locate the running binary: {}\n", e);
            return 1;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&bindir) {
        err!("nash install: cannot create {}: {}\n", bindir, e);
        return 1;
    }
    let same = std::fs::canonicalize(&dest).map(|d| d == src).unwrap_or(false);
    if same {
        out!("nash is already installed at {}\n", dest);
    } else {
        // copy next to the target and rename, so a running nash is never truncated
        let tmp = format!("{}.new{}", dest, std::process::id());
        let r = std::fs::copy(&src, &tmp).and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))).and_then(|_| std::fs::rename(&tmp, &dest));
        if let Err(e) = r {
            let _ = std::fs::remove_file(&tmp);
            err!("nash install: cannot install to {}: {}\n", dest, e);
            if !root {
                err!("try: sudo {} install\n", src.display());
            }
            return 1;
        }
        out!("installed {}\n", dest);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    if !path.split(':').any(|d| d.trim_end_matches('/') == bindir) {
        out!("note: {} is not in your PATH; add it in ~/.profile:\n    export PATH=\"{}:$PATH\"\n", bindir, bindir);
    }
    if !chsh {
        return 0;
    }

    // 2. /etc/shells
    let shells = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    if !shells.lines().any(|l| l.trim() == dest) {
        let line = format!("{}{}\n", if shells.is_empty() || shells.ends_with('\n') { "" } else { "\n" }, dest);
        let appended = std::fs::OpenOptions::new().append(true).create(true).open("/etc/shells").and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes())).is_ok();
        if appended {
            out!("added {} to /etc/shells\n", dest);
        } else if sys::isatty(0) && has("sudo") {
            out!("adding {} to /etc/shells (needs sudo)\n", dest);
            if !run("sudo", &["sh", "-c", &format!("echo '{}' >> /etc/shells", dest)]) {
                err!("nash install: could not update /etc/shells\n");
                return 1;
            }
        } else {
            err!("nash install: /etc/shells is not writable. Run as root:\n    echo '{}' >> /etc/shells\nthen:\n    chsh -s {}\n", dest, dest);
            return 1;
        }
    }

    // 3. login shell
    let current = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned()
        }
    };
    if current == dest {
        out!("nash is already your login shell\n");
        return 0;
    }
    let ok = if has("chsh") {
        if root {
            run("chsh", &["-s", &dest, &user])
        } else {
            out!("changing your login shell (chsh may ask for your password)\n");
            run("chsh", &["-s", &dest])
        }
    } else if root && has("usermod") {
        run("usermod", &["-s", &dest, &user])
    } else {
        false
    };
    if !ok {
        err!("nash install: could not change the login shell. Do it by hand:\n    chsh -s {}\n", dest);
        return 1;
    }
    out!("nash is now the login shell for {} (was {})\nopen a new terminal or log in again to use it\nto go back: chsh -s {}\n", user, if current.is_empty() { "unknown" } else { &current }, if current.is_empty() { "/bin/sh" } else { &current });
    0
}
