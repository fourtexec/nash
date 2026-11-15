//! `nash update`: look up a release in the manifest on fourtexec.site,
//! download the binary built for this platform and replace the running one.
//!
//! The manifest (JSON) lists releases, newest first within a channel
//! (`stable` or `dev`). Each release has binaries tagged with the platform
//! they are for (`linux`/`macos`/`android`, `amd64`/`arm64`), a download link
//! and a SHA-256. Files are fetched with curl or wget, so nash itself needs
//! no network code.
//!
//! `nash update PATH` installs a binary the user already has instead.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

const DEFAULT_URL: &str = "https://fourtexec.site/nash/manifest.json";

const HELP: &str = "\
usage: nash update [--check] [--force] [--list] [dev | stable | VERSION]
       nash update [--check] PATH

Updates nash from the release manifest on fourtexec.site:
  1. reads the manifest and picks a release that has a binary for this
     platform (operating system and CPU)
  2. downloads that binary and verifies its checksum
  3. replaces the nash binary that is running

  nash update           the newest stable release
  nash update dev       the newest development build
  nash update VERSION   exactly that release, older ones too (0.1.0, dev7)

  --check   only report whether the update would change anything
            (exit status 0: up to date, 1: update available)
  --force   reinstall even if this version is already installed
  --list    list the releases available for this platform

Needs curl or wget. NASH_UPDATE_URL overrides the manifest location.

With a PATH (an argument with a `/' in it, such as ./nash-linux-x86_64)
nothing is downloaded: the nash binary at PATH is checked the same way (built
for this system, runs and reports itself as nash) and installed. --check then
only runs the checks (exit status 0: PATH can be installed, 1: it cannot).
";

fn has(cmd: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {} >/dev/null 2>&1", cmd)]).status().map(|s| s.success()).unwrap_or(false)
}

/// The tags a binary for this platform may carry: (operating system, CPU).
fn platform() -> Option<(&'static [&'static str], &'static [&'static str])> {
    let os: &[&str] = match std::env::consts::OS {
        "linux" => &["linux"],
        "macos" => &["macos", "darwin"],
        "android" => &["android", "termux"],
        _ => return None,
    };
    let arch: &[&str] = match std::env::consts::ARCH {
        "x86_64" => &["amd64", "amd", "x86_64", "x64"],
        "aarch64" => &["arm64", "arm", "aarch64"],
        _ => return None,
    };
    Some((os, arch))
}

/// A parsed JSON value; just enough for the manifest.
#[derive(Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Json::Str(s)) => Some(s),
            _ => None,
        }
    }
    fn arr(&self, key: &str) -> &[Json] {
        match self.get(key) {
            Some(Json::Arr(a)) => a,
            _ => &[],
        }
    }
}

struct JsonParser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            return true;
        }
        false
    }
    fn word(&mut self, w: &str, v: Json) -> Option<Json> {
        if self.s[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            return Some(v);
        }
        None
    }
    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }
    fn string(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'"' | b'\\' | b'/' => e as char,
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + lo.checked_sub(0xDC00)?;
                            }
                            char::from_u32(cp)?
                        }
                        _ => return None,
                    };
                    out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
    fn value(&mut self) -> Option<Json> {
        self.ws();
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let v = match *self.s.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut members = Vec::new();
                if !self.eat(b'}') {
                    loop {
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return None;
                        }
                        members.push((k, self.value()?));
                        if self.eat(b'}') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Json::Obj(members)
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                if !self.eat(b']') {
                    loop {
                        items.push(self.value()?);
                        if self.eat(b']') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Json::Arr(items)
            }
            b'"' => Json::Str(self.string()?),
            b't' => self.word("true", Json::Bool(true))?,
            b'f' => self.word("false", Json::Bool(false))?,
            b'n' => self.word("null", Json::Null)?,
            _ => {
                let start = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.i += 1;
                }
                Json::Num(std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()?)
            }
        };
        self.depth -= 1;
        Some(v)
    }
}

fn parse_json(text: &str) -> Option<Json> {
    let mut p = JsonParser { s: text.as_bytes(), i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

/// A binary of a release that runs on this platform.
#[derive(Debug, PartialEq)]
struct Build {
    version: String,
    channel: String,
    name: String,
    url: String,
    sha256: String,
}

/// The releases of the manifest that have a binary for the platform, in
/// manifest order (newest first within a channel). A binary fits when all of
/// its tags name this platform and it names both the OS and the CPU.
fn builds(manifest: &Json, os: &[&str], arch: &[&str]) -> Vec<Build> {
    let mut found = Vec::new();
    for release in manifest.arr("releases") {
        let (version, channel) = match (release.str("version"), release.str("channel")) {
            (Some(v), Some(c)) => (v, c),
            _ => continue,
        };
        for binary in release.arr("binaries") {
            let tags: Vec<String> = binary.arr("tags").iter().filter_map(|t| if let Json::Str(s) = t { Some(s.to_lowercase()) } else { None }).collect();
            let is = |set: &[&str], t: &String| set.contains(&t.as_str());
            let fits = tags.iter().all(|t| is(os, t) || is(arch, t)) && tags.iter().any(|t| is(os, t)) && tags.iter().any(|t| is(arch, t));
            if let (true, Some(url), Some(sha256)) = (fits, binary.str("url"), binary.str("sha256")) {
                found.push(Build {
                    version: version.trim_start_matches('v').to_string(),
                    channel: channel.to_string(),
                    name: binary.str("name").unwrap_or("nash").to_string(),
                    url: url.to_string(),
                    sha256: sha256.to_lowercase(),
                });
                break;
            }
        }
    }
    found
}

/// `dev12` -> 12.
fn dev_number(s: &str) -> Option<u64> {
    s.strip_prefix("dev")?.parse().ok()
}

/// Is `current` at least as new as `target`, both of the same channel?
fn not_older(current: &str, target: &str) -> bool {
    if current == target {
        return true;
    }
    match (dev_number(current), dev_number(target)) {
        (Some(c), Some(t)) => c >= t,
        (None, None) => matches!((parse_version(current), parse_version(target)), (Some(c), Some(t)) if c >= t),
        _ => false,
    }
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

/// What kind of file a header belongs to: (operating system, CPU), in the
/// vocabulary of `std::env::consts`. Err: not a native executable at all.
fn identify(h: &[u8]) -> Result<(&'static str, String), String> {
    if h.starts_with(b"#!") {
        return Err("is a script, not a nash binary".to_string());
    }
    if h.starts_with(b"MZ") {
        return Ok(("windows", "unknown".to_string()));
    }
    if h.starts_with(b"\x7fELF") {
        if h.len() < 20 {
            return Err("is a truncated ELF file".to_string());
        }
        let machine = if h[5] == 2 { u16::from_be_bytes([h[18], h[19]]) } else { u16::from_le_bytes([h[18], h[19]]) };
        let arch = match (machine, h[4]) {
            (3, _) => "x86".to_string(),
            (40, _) => "arm".to_string(),
            (62, _) => "x86_64".to_string(),
            (183, _) => "aarch64".to_string(),
            (243, 1) => "riscv32".to_string(),
            (243, _) => "riscv64".to_string(),
            (m, _) => format!("unknown (ELF machine {})", m),
        };
        return Ok(("linux", arch));
    }
    // Mach-O, 64-bit then 32-bit, as stored by a little-endian machine
    if h.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) || h.starts_with(&[0xce, 0xfa, 0xed, 0xfe]) {
        if h.len() < 8 {
            return Err("is a truncated Mach-O file".to_string());
        }
        let arch = match u32::from_le_bytes([h[4], h[5], h[6], h[7]]) {
            0x0100_0007 => "x86_64".to_string(),
            0x0100_000c => "aarch64".to_string(),
            7 => "x86".to_string(),
            12 => "arm".to_string(),
            c => format!("unknown (Mach-O cpu {:#x})", c),
        };
        return Ok(("macos", arch));
    }
    Err("is not an executable binary".to_string())
}

/// Is a binary for (os, arch) one this system can be expected to run?
fn check_platform(os: &str, arch: &str) -> Result<(), String> {
    let my_os = std::env::consts::OS;
    let my_arch = std::env::consts::ARCH;
    // Android runs ELF binaries too; whether this one starts is tested later.
    let my_family = if my_os == "android" { "linux" } else { my_os };
    let name = |os: &str| match os {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        "android" => "Android",
        other => return other.to_string(),
    }
    .to_string();
    if os != my_family {
        return Err(format!("is a {} binary, but this system is {}", name(os), name(my_os)));
    }
    if arch != my_arch {
        return Err(format!("is built for {}, but this system is {}", arch, my_arch));
    }
    Ok(())
}

/// Check the file at `path` without running it.
fn inspect(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Err("is a directory".to_string());
    }
    if !meta.is_file() {
        return Err("is not a regular file".to_string());
    }
    let mut header = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(64).read_to_end(&mut header)).map_err(|e| e.to_string())?;
    if header.is_empty() {
        return Err("is empty".to_string());
    }
    let (os, arch) = identify(&header)?;
    check_platform(os, &arch)
}

/// Run `bin --version`; the version it reports if it is a working nash.
fn nash_version(bin: &str) -> Result<String, String> {
    let out = Command::new(bin).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().map_err(|e| format!("does not run on this system: {}", e))?;
    if !out.status.success() {
        return Err("does not run on this system".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    match line.strip_prefix("nash ") {
        Some(v) if !v.is_empty() => Ok(v.to_string()),
        _ => Err("runs, but is not nash".to_string()),
    }
}

/// `nash update PATH`: install a nash binary that is already on this machine.
fn install_file(path: &str, check: bool) -> i32 {
    let current = crate::version();

    // 1. is it a binary for this system? Nothing has been written yet.
    let src = Path::new(path);
    if let Err(e) = inspect(src) {
        err!("nash update: {} {}\n", path, e);
        return 1;
    }
    let dest = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(e) => {
            err!("nash update: cannot locate the running binary: {}\n", e);
            return 1;
        }
    };
    if src.canonicalize().map(|p| p == dest).unwrap_or(false) {
        err!("nash update: {} is the nash that is already running\n", path);
        return 1;
    }
    let dest = dest.to_string_lossy().into_owned();

    // 2. copy it next to the target (so the final rename is atomic) and make
    //    sure the copy really runs here as nash
    let tmp = if check {
        // --check must work without write access to the install directory
        let dir = std::env::var("TMPDIR").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/tmp".to_string());
        format!("{}/nash-update-check{}", dir.trim_end_matches('/'), std::process::id())
    } else {
        format!("{}.new{}", dest, std::process::id())
    };
    let fail = |msg: String| -> i32 {
        let _ = std::fs::remove_file(&tmp);
        err!("nash update: {}\n", msg);
        1
    };
    if let Err(e) = std::fs::copy(src, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        if check {
            err!("nash update: cannot copy {} to {}: {}\n", path, tmp, e);
        } else {
            err!("nash update: cannot write to {}: {}\n", dest, e);
            if e.kind() == std::io::ErrorKind::PermissionDenied && unsafe { libc::geteuid() } != 0 {
                err!("try: sudo {} update {}\n", dest, path);
            }
        }
        return 1;
    }
    if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
        return fail(format!("cannot make {} executable: {}", tmp, e));
    }
    let version = match nash_version(&tmp) {
        Ok(v) => v,
        Err(e) => return fail(format!("{} {}", path, e)),
    };
    if check {
        let _ = std::fs::remove_file(&tmp);
        out!("{} is nash {} for this system (you have {}); run `nash update {}' to install it\n", path, version, current, path);
        return 0;
    }

    // 3. swap it in; a nash that is running keeps its old file
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        return fail(format!("cannot replace {}: {}", dest, e));
    }
    out!("updated {} from nash {} to nash {}\n", dest, current, version);
    0
}

pub fn update(args: &[String]) -> i32 {
    let mut check = false;
    let mut force = false;
    let mut list = false;
    let mut want: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--check" => check = true,
            "--force" => force = true,
            "--list" => list = true,
            "-h" | "--help" => {
                out!("{}", HELP);
                return 0;
            }
            other if other.starts_with('-') || want.is_some() => {
                err!("nash update: unexpected argument `{}'\n{}", other, HELP);
                return 2;
            }
            other => want = Some(other),
        }
    }
    // A path is a binary that is already here; anything else names a release.
    if let Some(path) = want.filter(|w| w.contains('/')) {
        return install_file(path, check);
    }
    let current = crate::version();
    let url = std::env::var("NASH_UPDATE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| DEFAULT_URL.to_string());
    let (os, arch) = match platform() {
        Some(p) => p,
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

    // 1. the manifest, and the release asked for
    let manifest = match fetch.text(&url) {
        Ok(text) => match parse_json(&text) {
            Some(m) => m,
            None => {
                err!("nash update: {} is not a release manifest\n", url);
                return 1;
            }
        },
        Err(e) => {
            err!("nash update: cannot download the release manifest: {}\n", e);
            return 1;
        }
    };
    let available = builds(&manifest, os, arch);
    if list {
        for b in &available {
            out!("{:<12} {}{}\n", b.version, b.channel, if b.version == current { "  (installed)" } else { "" });
        }
        return 0;
    }
    // `nash update` and `nash update dev` take the newest of a channel and
    // never go back to an older one; a version by name is taken as it is.
    let channel = match want {
        None | Some("stable") => Some("stable"),
        Some("dev") => Some("dev"),
        Some(_) => None,
    };
    let build = match channel {
        Some(c) => available.iter().find(|b| b.channel == c),
        None => {
            let v = want.unwrap_or("").trim_start_matches('v');
            available.iter().find(|b| b.version == v)
        }
    };
    let build = match build {
        Some(b) => b,
        None => {
            match channel {
                Some(c) => err!("nash update: no {} release for {}/{} in the manifest\n", c, os[0], arch[0]),
                None => err!("nash update: no release `{}' for {}/{}; see `nash update --list'\n", want.unwrap_or(""), os[0], arch[0]),
            }
            return 1;
        }
    };
    let latest = build.version.as_str();
    let asset = build.name.as_str();
    let up_to_date = match channel {
        Some(_) => not_older(current, latest),
        None => current == latest,
    };
    if up_to_date && !force {
        out!("nash {} is up to date\n", current);
        return 0;
    }
    if check {
        if up_to_date {
            out!("nash {} is up to date\n", current);
            return 0;
        }
        out!("nash {} is available (you have {}); run `nash update{}'\n", latest, current, want.map(|w| format!(" {}", w)).unwrap_or_default());
        return 1;
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
    if let Err(e) = fetch.file(&build.url, &tmp) {
        return fail(format!("cannot download {}: {}", build.url, e));
    }

    // 3. verify: checksum, then make sure it really runs as nash
    let want = build.sha256.clone();
    match sha256(&tmp) {
        Some(got) if got == want => {}
        Some(got) => return fail(format!("checksum mismatch for {} (expected {}, got {})", asset, want, got)),
        None => err!("nash update: warning: no sha256sum or shasum, checksum not verified\n"),
    }
    if let Err(e) = inspect(Path::new(&tmp)) {
        return fail(format!("the downloaded {} {}", asset, e));
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
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(parse_version(" 0.10.0\n"), Some(vec![0, 10, 0]));
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("1.0") < parse_version("1.0.1"));
        assert_eq!(parse_version("<html>"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn newer_or_same() {
        assert!(not_older("0.1.1", "0.1.0.7"));
        assert!(!not_older("0.1.0", "0.1.1"));
        assert!(not_older("dev10", "dev9"));
        assert!(!not_older("dev9", "dev10"));
        // switching channel is always a change
        assert!(!not_older("dev9", "0.1.0"));
        assert!(!not_older("9.0.0", "dev1"));
    }

    #[test]
    fn json() {
        let v = parse_json(r#" {"a": [1, -2.5e1, true, null], "s": "x\n\"é😀", "o": {}} "#).unwrap();
        assert_eq!(v.arr("a"), &[Json::Num(1.0), Json::Num(-25.0), Json::Bool(true), Json::Null]);
        assert_eq!(v.str("s"), Some("x\n\"é😀"));
        assert_eq!(v.get("o"), Some(&Json::Obj(vec![])));
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "\"abc", "<html>", "{} x", "nul"] {
            assert_eq!(parse_json(bad), None, "{}", bad);
        }
    }

    const MANIFEST: &str = r#"{"schema": 1, "releases": [
        {"version": "0.2.0", "channel": "stable", "binaries": [
            {"tags": ["macos", "arm64"], "name": "m", "url": "https://x/020-mac", "sha256": "AA"}]},
        {"version": "v0.1.0", "channel": "stable", "binaries": [
            {"tags": ["linux", "arm64"], "name": "a", "url": "https://x/010-arm", "sha256": "bb"},
            {"tags": ["linux", "amd64"], "name": "b", "url": "https://x/010-amd", "sha256": "cc"},
            {"tags": ["linux"], "name": "c", "url": "https://x/010-any", "sha256": "dd"}]},
        {"version": "dev7", "channel": "dev", "binaries": [
            {"tags": ["Linux", "x86_64"], "url": "https://x/dev7", "sha256": "ee"},
            {"tags": ["linux", "amd64", "musl-only"], "url": "https://x/no", "sha256": "ff"}]},
        {"version": "dev6", "channel": "dev", "binaries": [{"tags": ["linux", "amd"], "url": "https://x/nohash"}]}
    ]}"#;

    #[test]
    fn picks_binaries_by_tags() {
        let m = parse_json(MANIFEST).unwrap();
        let linux = builds(&m, &["linux"], &["amd64", "amd", "x86_64"]);
        let got: Vec<(&str, &str, &str)> = linux.iter().map(|b| (b.version.as_str(), b.channel.as_str(), b.url.as_str())).collect();
        // 0.2.0 has nothing for linux, so the newest stable here is 0.1.0
        assert_eq!(got, [("0.1.0", "stable", "https://x/010-amd"), ("dev7", "dev", "https://x/dev7")]);
        assert_eq!(linux[0].sha256, "cc");
        assert_eq!(linux[1].name, "nash");
        let mac = builds(&m, &["macos", "darwin"], &["arm64", "arm", "aarch64"]);
        assert_eq!(mac.len(), 1);
        assert_eq!((mac[0].version.as_str(), mac[0].sha256.as_str()), ("0.2.0", "aa"));
        assert!(builds(&m, &["android"], &["arm64"]).is_empty());
        assert!(builds(&parse_json("[]").unwrap(), &["linux"], &["amd64"]).is_empty());
    }

    fn elf(class: u8, machine: u16) -> Vec<u8> {
        let mut h = vec![0u8; 64];
        h[..4].copy_from_slice(b"\x7fELF");
        h[4] = class;
        h[5] = 1;
        h[18..20].copy_from_slice(&machine.to_le_bytes());
        h
    }

    fn macho(cpu: u32) -> Vec<u8> {
        let mut h = vec![0xcf, 0xfa, 0xed, 0xfe];
        h.extend_from_slice(&cpu.to_le_bytes());
        h
    }

    #[test]
    fn headers() {
        assert_eq!(identify(&elf(2, 62)), Ok(("linux", "x86_64".to_string())));
        assert_eq!(identify(&elf(2, 183)), Ok(("linux", "aarch64".to_string())));
        assert_eq!(identify(&elf(1, 40)), Ok(("linux", "arm".to_string())));
        assert_eq!(identify(&macho(0x0100_0007)), Ok(("macos", "x86_64".to_string())));
        assert_eq!(identify(&macho(0x0100_000c)), Ok(("macos", "aarch64".to_string())));
        assert_eq!(identify(b"MZ\x90\x00").map(|p| p.0), Ok("windows"));
        assert!(identify(b"#!/bin/sh\necho nash 9.9.9\n").is_err());
        assert!(identify(b"<html>").is_err());
        assert!(identify(b"\x7fELF").is_err());
    }

    #[test]
    fn platforms() {
        let os = std::env::consts::OS;
        let family = if os == "android" { "linux" } else { os };
        let arch = std::env::consts::ARCH;
        assert!(check_platform(family, arch).is_ok());
        assert!(check_platform("windows", arch).is_err());
        assert!(check_platform(family, "m68k").is_err());
        let other = if family == "linux" { "macos" } else { "linux" };
        assert!(check_platform(other, arch).is_err());
    }
}
