//! `nash update`: download the newest release from GitHub and replace the
//! running binary with it.
//!
//! A release carries one binary per platform (`nash-linux-x86_64`, ...), a
//! `VERSION` file and `SHA256SUMS`. They are fetched with curl or wget, so
//! nash itself needs no network code.

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

const DEFAULT_URL: &str = "https://github.com/fourtexec/nash/releases/latest/download";

const HELP: &str = "\
usage: nash update [--check] [--force]

Updates nash to the newest release from GitHub:
  1. looks up the latest release of fourtexec/nash
  2. downloads the binary for this platform and verifies its checksum
  3. replaces the nash binary that is running

  --check   only report whether a newer version is available
            (exit status 0: up to date, 1: update available)
  --force   reinstall even if this version is already the newest

Needs curl or wget. NASH_UPDATE_URL overrides the download location.
";

fn has(cmd: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {} >/dev/null 2>&1", cmd)]).status().map(|s| s.success()).unwrap_or(false)
}

/// The release asset built for the platform this binary was compiled for.
fn asset_name() -> Option<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        "android" => "android",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    Some(format!("nash-{}-{}", os, arch))
}

/// Parse `v1.2.3` / `1.2.3` into comparable numbers.
fn parse_version(s: &str) -> Option<Vec<u64>> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    if s.is_empty() {
        return None;
    }
    s.split('.').map(|p| p.parse::<u64>().ok()).collect()
}

struct Fetcher {
    curl: bool,
}

impl Fetcher {
    fn new() -> Option<Fetcher> {
        if has("curl") {
            Some(Fetcher { curl: true })
        } else if has("wget") {
            Some(Fetcher { curl: false })
        } else {
            None
        }
    }

    fn command(&self, url: &str, dest: &str) -> Command {
        let mut c;
        if self.curl {
            c = Command::new("curl");
            c.args(["-fsSL", "--retry", "2", "-o", dest, url]);
        } else {
            c = Command::new("wget");
            c.args(["-q", "-O", dest, url]);
        }
        c
    }

    /// Fetch a small text file.
    fn text(&self, url: &str) -> Result<String, String> {
        let out = self.command(url, "-").stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if msg.is_empty() { "download failed".to_string() } else { msg });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn file(&self, url: &str, dest: &str) -> Result<(), String> {
        let out = self.command(url, dest).stdin(Stdio::null()).stdout(Stdio::null()).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if msg.is_empty() { "download failed".to_string() } else { msg });
        }
        Ok(())
    }
}

/// SHA-256 of a file, by whichever tool the system has. None: no tool.
fn sha256(path: &str) -> Option<String> {
    for (cmd, args) in [("sha256sum", &[][..]), ("shasum", &["-a", "256"][..])] {
        if let Ok(out) = Command::new(cmd).args(args).arg(path).stderr(Stdio::null()).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                if let Some(h) = s.split_whitespace().next() {
                    return Some(h.to_lowercase());
                }
            }
        }
    }
    None
}

pub fn update(args: &[String]) -> i32 {
    let mut check = false;
    let mut force = false;
    for a in args {
        match a.as_str() {
            "--check" => check = true,
            "--force" => force = true,
            "-h" | "--help" => {
                out!("{}", HELP);
                return 0;
            }
            other => {
                err!("nash update: unknown option `{}'\n{}", other, HELP);
                return 2;
            }
        }
    }
    let current = env!("CARGO_PKG_VERSION");
    let base = std::env::var("NASH_UPDATE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| DEFAULT_URL.to_string());
    let base = base.trim_end_matches('/');
    let asset = match asset_name() {
        Some(a) => a,
        None => {
            err!("nash update: no release binaries for {}-{}; build from source instead\n", std::env::consts::OS, std::env::consts::ARCH);
            return 1;
        }
    };
    let fetch = match Fetcher::new() {
        Some(f) => f,
        None => {
            err!("nash update: needs curl or wget\n");
            return 1;
        }
    };

    // 1. the newest version
    let latest = match fetch.text(&format!("{}/VERSION", base)) {
        Ok(v) => v.trim().trim_start_matches('v').to_string(),
        Err(e) => {
            err!("nash update: cannot find the latest release: {}\n", e);
            return 1;
        }
    };
    let (new, cur) = match (parse_version(&latest), parse_version(current)) {
        (Some(n), Some(c)) => (n, c),
        _ => {
            err!("nash update: unexpected release version `{}'\n", latest.lines().next().unwrap_or(""));
            return 1;
        }
    };
    if new <= cur && !force {
        out!("nash {} is up to date\n", current);
        return 0;
    }
    if check {
        if new > cur {
            out!("nash {} is available (you have {}); run `nash update'\n", latest, current);
            return 1;
        }
        out!("nash {} is up to date\n", current);
        return 0;
    }

    // 2. download next to the target, so the final rename is atomic
    let dest = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => {
            err!("nash update: cannot locate the running binary: {}\n", e);
            return 1;
        }
    };
    let tmp = format!("{}.new{}", dest, std::process::id());
    let fail = |msg: String| -> i32 {
        let _ = std::fs::remove_file(&tmp);
        err!("nash update: {}\n", msg);
        1
    };
    if let Err(e) = std::fs::File::create(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        err!("nash update: cannot write to {}: {}\n", dest, e);
        if unsafe { libc::geteuid() } != 0 {
            err!("try: sudo {} update\n", dest);
        }
        return 1;
    }
    out!("downloading nash {} ({})\n", latest, asset);
    if let Err(e) = fetch.file(&format!("{}/{}", base, asset), &tmp) {
        return fail(format!("cannot download {}: {}", asset, e));
    }

    // 3. verify: checksum, then make sure it really runs and is that version
    let sums = match fetch.text(&format!("{}/SHA256SUMS", base)) {
        Ok(s) => s,
        Err(e) => return fail(format!("cannot download SHA256SUMS: {}", e)),
    };
    let want = sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let hash = it.next()?;
        let name = it.next()?.trim_start_matches('*');
        (name == asset).then(|| hash.to_lowercase())
    });
    let want = match want {
        Some(w) => w,
        None => return fail(format!("SHA256SUMS has no entry for {}", asset)),
    };
    match sha256(&tmp) {
        Some(got) if got == want => {}
        Some(got) => return fail(format!("checksum mismatch for {} (expected {}, got {})", asset, want, got)),
        None => err!("nash update: warning: no sha256sum or shasum, checksum not verified\n"),
    }
    if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
        return fail(format!("cannot make {} executable: {}", tmp, e));
    }
    let reported = Command::new(&tmp).arg("--version").stdin(Stdio::null()).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    match reported.as_deref() {
        Some(r) if r.starts_with("nash ") => {}
        _ => return fail(format!("the downloaded binary does not run as nash on this system")),
    }

    // 4. swap it in; a nash that is running keeps its old file
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        return fail(format!("cannot replace {}: {}", dest, e));
    }
    out!("updated {} from {} to {}\n", dest, current, latest);
    0
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(parse_version(" 0.10.0\n"), Some(vec![0, 10, 0]));
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("1.0") < parse_version("1.0.1"));
        assert_eq!(parse_version("<html>"), None);
        assert_eq!(parse_version(""), None);
    }
}
