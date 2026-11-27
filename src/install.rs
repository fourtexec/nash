//! `nash install`: copy the binary into place, register it in /etc/shells and
//! make it the user's login shell.

use crate::sys;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

const HELP: &str = "\
usage: nash install [--prefix DIR] [--no-chsh] [--no-logo]

Installs nash as your main shell:
  1. copies the nash binary to DIR/bin (default /usr/local, or ~/.local
     when /usr/local/bin is not writable)
  2. adds the nash texted startup logo to ~/.bashrc
  3. adds it to /etc/shells
  4. makes it your login shell with chsh

  --prefix DIR   install into DIR/bin
  --no-chsh      only install the binary and ~/.bashrc hook, do not change the login shell
  --no-logo      do not add the startup logo to ~/.bashrc
";

const LOGO_START: &str = "# >>> nash logo >>>";
const LOGO_END: &str = "# <<< nash logo <<<";

/// Build the idempotent `.bashrc` snippet that prints the nash texted logo on
/// interactive shell startup.
fn bashrc_logo_block(nash_bin: &str) -> String {
    let quoted = crate::expand::shell_quote(nash_bin);
    format!(
        "{start}\n\
         case $- in\n\
             *i*)\n\
                 if [ \"${{NASH_NO_LOGO:-0}}\" != \"1\" ] && [ -x {bin} ]; then\n\
                     {bin} --logo\n\
                 fi\n\
                 ;;\n\
         esac\n\
         {end}\n",
        start = LOGO_START,
        bin = quoted,
        end = LOGO_END,
    )
}

/// Remove any `# >>> marker >>>` ... `# <<< marker <<<` section from `content`.
fn strip_managed_block(content: &str, start_marker: &str, end_marker: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut skip = false;
    for line in content.lines() {
        if line.trim() == start_marker {
            skip = true;
            continue;
        }
        if line.trim() == end_marker {
            skip = false;
            continue;
        }
        if skip {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Insert or update the nash startup logo block inside `<home>/.bashrc`.
fn install_bashrc_logo(home: &str, nash_bin: &str) -> std::io::Result<String> {
    let bashrc = format!("{}/.bashrc", home.trim_end_matches('/'));
    let existing = std::fs::read_to_string(&bashrc).unwrap_or_default();
    let cleaned = strip_managed_block(&existing, LOGO_START, LOGO_END);
    let mut updated = cleaned;
    if !updated.is_empty() && !updated.ends_with("\n\n") {
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push('\n');
    }
    updated.push_str(&bashrc_logo_block(nash_bin));
    std::fs::write(&bashrc, updated)?;
    Ok(bashrc)
}

fn writable_dir(dir: &str) -> bool {
    sys::is_dir(dir) && sys::access(dir, libc::W_OK)
}

fn run(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).status().map(|s| s.success()).unwrap_or(false)
}

fn has(cmd: &str) -> bool {
    sys::find_in_path(cmd).is_some()
}

/// Reject control characters or newlines in a shell path before writing to
/// `/etc/shells` or passing to `chsh`.
fn is_safe_shell_path(path: &str) -> bool {
    !path.is_empty() && !path.bytes().any(|b| b < 0x20 || b == 0x7f)
}

/// Append `line` to `/etc/shells` via `sudo tee -a /etc/shells` without
/// invoking a subshell or interpolating the path into a shell command.
fn append_etc_shells_sudo(line: &str) -> bool {
    let mut child = match Command::new("sudo")
        .args(["tee", "-a", "/etc/shells"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(line.as_bytes()).is_err() {
            let _ = child.wait();
            return false;
        }
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

pub fn install(args: &[String]) -> i32 {
    let mut prefix: Option<String> = None;
    let mut chsh = true;
    let mut add_logo = true;
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
            "--no-logo" => add_logo = false,
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
    let root = sys::geteuid() == 0;

    // 1. the binary
    let bindir = match prefix {
        Some(p) => format!("{}/bin", p.trim_end_matches('/')),
        None if root || writable_dir("/usr/local/bin") => "/usr/local/bin".to_string(),
        None => format!("{}/.local/bin", home),
    };
    let dest = format!("{}/nash", bindir);
    if !is_safe_shell_path(&dest) {
        err!("nash install: install path contains invalid control characters\n");
        return 2;
    }
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
    {
        // copy into an exclusively-created temp file next to the target and rename,
        // so a running nash is never truncated and symlinks cannot be abused
        let copy_res = (|| -> std::io::Result<()> {
            let (tmp, mut dst_file) = sys::create_temp_file(&bindir, ".nash-install-", 0o755)?;
            let r = std::fs::File::open(&src)
                .and_then(|mut src_file| std::io::copy(&mut src_file, &mut dst_file))
                .and_then(|_| dst_file.flush())
                .and_then(|_| {
                    drop(dst_file);
                    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                })
                .and_then(|_| std::fs::rename(&tmp, &dest));
            if r.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            r
        })();
        if let Err(e) = copy_res {
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
    if add_logo {
        match install_bashrc_logo(&home, &dest) {
            Ok(rc) => {
                out!("added nash startup logo to {}\n", rc);
            }
            Err(e) => err!("nash install: could not update ~/.bashrc: {}\n", e),
        }
    }
    if !chsh {
        return 0;
    }

    // 2. /etc/shells
    let shells = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    if !shells.lines().any(|l| l.trim() == dest) {
        let line = format!("{}{}\n", if shells.is_empty() || shells.ends_with('\n') { "" } else { "\n" }, dest);
        let appended = std::fs::OpenOptions::new().append(true).create(true).open("/etc/shells").and_then(|mut f| f.write_all(line.as_bytes())).is_ok();
        if appended {
            out!("added {} to /etc/shells\n", dest);
        } else if sys::isatty(0) && has("sudo") {
            out!("adding {} to /etc/shells (needs sudo)\n", dest);
            if !append_etc_shells_sudo(&line) {
                err!("nash install: could not update /etc/shells\n");
                return 1;
            }
        } else {
            let quoted = crate::expand::shell_quote(&dest);
            err!("nash install: /etc/shells is not writable. Run as root:\n    printf '%s\\n' {} >> /etc/shells\nthen:\n    chsh -s {}\n", quoted, quoted);
            return 1;
        }
    }

    // 3. login shell
    let current = sys::get_login_shell();
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
        err!("nash install: could not change the login shell. Do it by hand:\n    chsh -s {}\n", crate::expand::shell_quote(&dest));
        return 1;
    }
    out!("nash is now the login shell for {} (was {})\nopen a new terminal or log in again to use it\nto go back: chsh -s {}\n", user, if current.is_empty() { "unknown" } else { &current }, if current.is_empty() { "/bin/sh" } else { &current });
    0
}

pub fn uninstall(_args: &[String]) -> i32 {
    let home = std::env::var("HOME").ok().unwrap_or_else(|| "/".to_string());
    // Remove managed nash blocks added to common rc files.
    for rc in [format!("{}/.profile", home), format!("{}/.bashrc", home), format!("{}/.zshrc", home), format!("{}/.nashrc", home)] {
        if let Ok(s) = std::fs::read_to_string(&rc) {
            let out = strip_managed_block(&s, "# >>> nash >>>", "# <<< nash <<<");
            let out = strip_managed_block(&out, LOGO_START, LOGO_END);
            let _ = std::fs::write(&rc, out);
        }
    }

    // Remove nash from /etc/shells if writable.
    if let Ok(s) = std::fs::read_to_string("/etc/shells") {
        let filtered: String = s.lines().filter(|l| !l.trim().ends_with("/nash")).collect::<Vec<_>>().join("\n");
        let filtered = if s.ends_with('\n') { format!("{}\n", filtered) } else { filtered };
        let _ = std::fs::write("/etc/shells", filtered);
    }

    // Try to remove the installed binary (best effort).
    let mut candidates = vec![
        format!("{}/.local/bin/nash", home),
        "/usr/local/bin/nash".to_string(),
        "/data/data/com.termux/files/usr/bin/nash".to_string(),
    ];
    if let Ok(prefix) = std::env::var("PREFIX") {
        candidates.push(format!("{}/bin/nash", prefix));
    }
    for c in &candidates {
        let _ = std::fs::remove_file(c);
    }

    // If nash is the login shell, tell the user how to switch back.
    let current = sys::get_login_shell();
    if current.ends_with("/nash") {
        out!("nash was your login shell. Switch back with:\n    chsh -s /bin/sh\n(or another shell of your choice)\n");
    }
    out!("nash uninstalled. Open a new terminal to apply PATH changes.\n");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_path_validation() {
        assert!(is_safe_shell_path("/usr/local/bin/nash"));
        assert!(!is_safe_shell_path(""));
        assert!(!is_safe_shell_path("/tmp/foo\n/bin/sh"));
        assert!(!is_safe_shell_path("/tmp/foo\0bar"));
        assert!(!is_safe_shell_path("/tmp/foo\rbar"));
    }

    #[test]
    fn bashrc_logo_install_is_idempotent_and_removable() {
        let base = std::env::temp_dir().join(format!("nash-bashrc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.to_str().unwrap();
        let rc_path = base.join(".bashrc");
        std::fs::write(&rc_path, "export FOO=bar\n").unwrap();

        install_bashrc_logo(home, "/usr/local/bin/nash").unwrap();
        let first = std::fs::read_to_string(&rc_path).unwrap();
        assert!(first.contains("export FOO=bar"));
        assert!(first.contains(LOGO_START));
        assert!(first.contains("--logo"));
        assert_eq!(first.matches(LOGO_START).count(), 1);

        // Re-running with a new path updates in place without duplicating.
        install_bashrc_logo(home, "/opt/my bin/nash").unwrap();
        let second = std::fs::read_to_string(&rc_path).unwrap();
        assert_eq!(second.matches(LOGO_START).count(), 1);
        assert!(second.contains("'/opt/my bin/nash' --logo"));
        assert!(!second.contains("/usr/local/bin/nash"));

        // Stripping removes the block while keeping user content intact.
        let cleaned = strip_managed_block(&second, LOGO_START, LOGO_END);
        assert!(cleaned.contains("export FOO=bar"));
        assert!(!cleaned.contains(LOGO_START));
        assert!(!cleaned.contains("--logo"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
