//! GitHub release discovery, HTTPS downloading, pure-Rust SHA-256 verification,
//! and ELF/Mach-O binary validation for `nash` and `nashed`.

use crate::sys;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

pub const DEFAULT_URL: &str = "https://api.github.com/repos/fourtexec/nash";

pub fn api_base_url() -> Result<String, String> {
    let url = std::env::var("NASH_UPDATE_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    let url = url.trim_end_matches('/').to_string();
    validate_https_url(&url)?;
    Ok(url)
}

/// Ensure a remote update URL uses HTTPS and contains no whitespace or control
/// characters that could cause header/argument injection.
pub fn validate_https_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("refusing non-HTTPS URL: {}", url))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.starts_with('.') {
        return Err(format!("invalid URL host: {}", url));
    }
    if url.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err(format!("invalid characters in URL: {}", url));
    }
    Ok(())
}

/// Release asset name for `binary` (`"nash"` or `"nashed"`) on this platform.
pub fn asset_name(binary: &str) -> Option<String> {
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
    Some(format!("{}-{}-{}", binary, os, arch))
}

/// A parsed JSON value; just enough for GitHub API responses.
#[derive(Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Json::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn arr(&self, key: &str) -> &[Json] {
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
                while self.i < self.s.len()
                    && matches!(self.s[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    self.i += 1;
                }
                Json::Num(std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()?)
            }
        };
        self.depth -= 1;
        Some(v)
    }
}

pub fn parse_json(text: &str) -> Option<Json> {
    let mut p = JsonParser { s: text.as_bytes(), i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

/// A release that has a binary for this platform.
#[derive(Debug, PartialEq)]
pub struct Build {
    /// `1.0.1` or `dev7`
    pub version: String,
    /// `stable` or `dev`, by tag name
    pub channel: &'static str,
    /// download URL for the binary
    pub url: String,
    /// SHA-256 recorded by GitHub, if present
    pub sha256: Option<String>,
    /// URL of the release `SHA256SUMS` file, if present
    pub sums_url: Option<String>,
}

/// Extract a `Build` for `asset` from a GitHub release JSON object.
pub fn build_of(release: &Json, asset: &str) -> Option<Build> {
    if release.get("draft") == Some(&Json::Bool(true)) {
        return None;
    }
    let tag = release.str("tag_name")?;
    let (version, channel) = if dev_number(tag).is_some() {
        (tag, "dev")
    } else {
        let v = tag.strip_prefix('v')?;
        parse_version(v)?;
        (v, "stable")
    };
    let file = |name: &str| {
        release
            .arr("assets")
            .iter()
            .find(|a| a.str("name") == Some(name) && a.str("state").unwrap_or("uploaded") == "uploaded")
    };
    let binary = file(asset)?;
    let digest = binary
        .str("digest")
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()));
    Some(Build {
        version: version.to_string(),
        channel,
        url: binary.str("browser_download_url")?.to_string(),
        sha256: digest.map(|h| h.to_lowercase()),
        sums_url: file("SHA256SUMS")
            .and_then(|a| a.str("browser_download_url"))
            .map(|u| u.to_string()),
    })
}

/// Releases with a binary for `asset`: stable first, newest first by version number.
pub fn builds(listing: &Json, asset: &str) -> Vec<Build> {
    let mut found: Vec<Build> = match listing {
        Json::Arr(releases) => releases.iter().filter_map(|r| build_of(r, asset)).collect(),
        _ => Vec::new(),
    };
    found.sort_by(|a, b| {
        let key = |x: &Build| (x.channel == "stable", parse_version(&x.version), dev_number(&x.version));
        key(b).cmp(&key(a))
    });
    found
}

/// Find the SHA-256 hash of `asset` inside a `SHA256SUMS` file.
pub fn checksum_in(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let hash = it.next()?;
        let name = it.next()?.trim_start_matches('*');
        (name == asset && hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_lowercase())
    })
}

/// `dev12` -> `Some(12)`.
pub fn dev_number(s: &str) -> Option<u64> {
    s.strip_prefix("dev")?.parse().ok()
}

/// Is `current` at least as new as `target` within the same channel?
pub fn not_older(current: &str, target: &str) -> bool {
    if current == target {
        return true;
    }
    match (dev_number(current), dev_number(target)) {
        (Some(c), Some(t)) => c >= t,
        (None, None) => {
            matches!((parse_version(current), parse_version(target)), (Some(c), Some(t)) if c >= t)
        }
        _ => false,
    }
}

/// Parse `v1.2.3` / `1.2.3` into comparable numeric components.
pub fn parse_version(s: &str) -> Option<Vec<u64>> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    if s.is_empty() {
        return None;
    }
    s.split('.').map(|p| p.parse::<u64>().ok()).collect()
}

pub struct Fetcher {
    curl: bool,
}

impl Fetcher {
    pub fn new() -> Option<Fetcher> {
        if sys::find_in_path("curl").is_some() {
            Some(Fetcher { curl: true })
        } else if sys::find_in_path("wget").is_some() {
            Some(Fetcher { curl: false })
        } else {
            None
        }
    }

    fn command(&self, url: &str, dest: &str) -> Command {
        let mut c;
        if self.curl {
            c = Command::new("curl");
            c.args([
                "-fsSL",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--retry",
                "2",
                "-o",
                dest,
                "--",
                url,
            ]);
        } else {
            c = Command::new("wget");
            c.args(["-q", "-O", dest, "--", url]);
        }
        c
    }

    pub fn text(&self, url: &str) -> Result<String, String> {
        validate_https_url(url)?;
        let out = self
            .command(url, "-")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if msg.is_empty() {
                "download failed".to_string()
            } else {
                msg
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn file(&self, url: &str, dest: &str) -> Result<(), String> {
        validate_https_url(url)?;
        let out = self
            .command(url, dest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if msg.is_empty() {
                "download failed".to_string()
            } else {
                msg
            });
        }
        Ok(())
    }

    pub fn json(&self, base_url: &str, endpoint: &str) -> Result<Json, String> {
        let text = self.text(&format!("{}/{}", base_url, endpoint))?;
        parse_json(&text).ok_or_else(|| "GitHub did not answer with a list of releases".to_string())
    }

    pub fn recent(&self, base_url: &str, asset: &str) -> Result<Vec<Build>, String> {
        self.json(base_url, "releases?per_page=100")
            .map(|listing| builds(&listing, asset))
    }

    /// Resolve a target (`None` / `Some("stable")` / `Some("dev")` / `Some(version)`)
    /// into a downloadable `Build`.
    pub fn resolve(
        &self,
        base_url: &str,
        asset: &str,
        want: Option<&str>,
    ) -> Result<Option<Build>, ResolveError> {
        let channel = match want {
            None | Some("stable") => Some("stable"),
            Some("dev") => Some("dev"),
            Some(_) => None,
        };
        match (channel, want) {
            (Some("stable"), _) => {
                if let Some(b) = self
                    .json(base_url, "releases/latest")
                    .ok()
                    .and_then(|r| build_of(&r, asset))
                    .filter(|b| b.channel == "stable")
                {
                    return Ok(Some(b));
                }
                self.recent(base_url, asset)
                    .map(|all| all.into_iter().find(|b| b.channel == "stable"))
                    .map_err(ResolveError::Network)
            }
            (Some(_), _) => self
                .recent(base_url, asset)
                .map(|all| all.into_iter().find(|b| b.channel == "dev"))
                .map_err(ResolveError::Network),
            (None, Some(name)) => {
                let name = name.strip_prefix('v').unwrap_or(name);
                if dev_number(name).is_none() && parse_version(name).is_none() {
                    return Err(ResolveError::BadVersion(name.to_string()));
                }
                let tag = if dev_number(name).is_some() {
                    name.to_string()
                } else {
                    format!("v{}", name)
                };
                Ok(self
                    .json(base_url, &format!("releases/tags/{}", tag))
                    .ok()
                    .and_then(|r| build_of(&r, asset)))
            }
            (None, None) => Ok(None),
        }
    }

    pub fn expected_sha256(&self, build: &Build, asset: &str) -> Result<String, String> {
        match (&build.sha256, &build.sums_url) {
            (Some(h), _) => Ok(h.clone()),
            (None, Some(sums)) => {
                let text = self
                    .text(sums)
                    .map_err(|e| format!("cannot download SHA256SUMS: {}", e))?;
                checksum_in(&text, asset).ok_or_else(|| {
                    format!("SHA256SUMS of {} has no entry for {}", build.version, asset)
                })
            }
            (None, None) => Err(format!(
                "release {} has no checksum for {}",
                build.version, asset
            )),
        }
    }
}

pub enum ResolveError {
    BadVersion(String),
    Network(String),
}

/// Pure-Rust streaming SHA-256 (FIPS 180-4) so checksum verification is always
/// enforced without depending on external hashing binaries.
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
            ],
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(Self::K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            if data.len() < need {
                self.buf[self.buf_len..self.buf_len + data.len()].copy_from_slice(data);
                self.buf_len += data.len();
                return;
            }
            self.buf[self.buf_len..64].copy_from_slice(&data[..need]);
            let block = self.buf;
            Self::compress(&mut self.state, &block);
            self.buf_len = 0;
            data = &data[need..];
        }
        while data.len() >= 64 {
            let block: &[u8; 64] = data[..64].try_into().unwrap();
            Self::compress(&mut self.state, block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finalize_hex(mut self) -> String {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            self.buf[self.buf_len..64].fill(0);
            let block = self.buf;
            Self::compress(&mut self.state, &block);
            self.buf_len = 0;
        }
        self.buf[self.buf_len..56].fill(0);
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        Self::compress(&mut self.state, &block);
        let mut out = String::with_capacity(64);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for word in self.state {
            for b in word.to_be_bytes() {
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
        }
        out
    }
}

/// SHA-256 of a file, computed in pure Rust so verification never fails open.
pub fn sha256_file(path: &str) -> Result<String, String> {
    let mut f =
        std::fs::File::open(path).map_err(|e| format!("cannot read {} for checksum: {}", path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("cannot read {} for checksum: {}", path, e)),
        }
    }
    Ok(hasher.finalize_hex())
}

/// Identify `(os_family, arch)` from an executable header.
pub fn identify(h: &[u8]) -> Result<(&'static str, String), String> {
    if h.starts_with(b"#!") {
        return Err("is a script, not a native binary".to_string());
    }
    if h.starts_with(b"MZ") {
        return Ok(("windows", "unknown".to_string()));
    }
    if h.starts_with(b"\x7fELF") {
        if h.len() < 20 {
            return Err("is a truncated ELF file".to_string());
        }
        let machine = if h[5] == 2 {
            u16::from_be_bytes([h[18], h[19]])
        } else {
            u16::from_le_bytes([h[18], h[19]])
        };
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

pub fn check_platform(os: &str, arch: &str) -> Result<(), String> {
    let my_os = std::env::consts::OS;
    let my_arch = std::env::consts::ARCH;
    let my_family = if my_os == "android" { "linux" } else { my_os };
    fn os_name(os: &str) -> &str {
        match os {
            "linux" => "Linux",
            "macos" => "macOS",
            "windows" => "Windows",
            "android" => "Android",
            other => other,
        }
    }
    if os != my_family {
        return Err(format!(
            "is a {} binary, but this system is {}",
            os_name(os),
            os_name(my_os)
        ));
    }
    if arch != my_arch {
        return Err(format!(
            "is built for {}, but this system is {}",
            arch, my_arch
        ));
    }
    Ok(())
}

/// Inspect a file header without executing it.
pub fn inspect(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Err("is a directory".to_string());
    }
    if !meta.is_file() {
        return Err("is not a regular file".to_string());
    }
    let mut header = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(64).read_to_end(&mut header))
        .map_err(|e| e.to_string())?;
    if header.is_empty() {
        return Err("is empty".to_string());
    }
    let (os, arch) = identify(&header)?;
    check_platform(os, &arch)
}

/// Run `bin --version` and verify it reports `<expected_name> <version>`.
pub fn binary_version(bin: &str, expected_name: &str) -> Result<String, String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("does not run on this system: {}", e))?;
    if !out.status.success() {
        return Err("does not run on this system".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    let prefix = format!("{} ", expected_name);
    match line.strip_prefix(&prefix) {
        Some(v) if !v.is_empty() => Ok(v.to_string()),
        _ => Err(format!("runs, but is not {}", expected_name)),
    }
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
        assert!(!not_older("dev9", "0.1.0"));
        assert!(!not_older("9.0.0", "dev1"));
    }

    #[test]
    fn json() {
        let v = parse_json(r#" {"a": [1, -2.5e1, true, null], "s": "x\n\"e", "o": {}} "#).unwrap();
        assert_eq!(
            v.arr("a"),
            &[Json::Num(1.0), Json::Num(-25.0), Json::Bool(true), Json::Null]
        );
        assert_eq!(v.str("s"), Some("x\n\"e"));
        assert_eq!(v.get("o"), Some(&Json::Obj(vec![])));
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "\"abc", "<html>", "{} x", "nul"] {
            assert_eq!(parse_json(bad), None, "{}", bad);
        }
    }

    const RELEASES: &str = r#"[
        {"tag_name": "dev9", "prerelease": true, "assets": [
            {"name": "nash-linux-x86_64", "browser_download_url": "https://x/dev9/nash-linux-x86_64"},
            {"name": "nashed-linux-x86_64", "browser_download_url": "https://x/dev9/nashed-linux-x86_64"},
            {"name": "SHA256SUMS", "browser_download_url": "https://x/dev9/SHA256SUMS"}]},
        {"tag_name": "v9.0.0", "draft": true, "assets": [
            {"name": "nash-linux-x86_64", "browser_download_url": "https://x/draft"}]},
        {"tag_name": "v0.1.1", "assets": [
            {"name": "nash-macos-aarch64", "browser_download_url": "https://x/011-mac",
             "digest": "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]},
        {"tag_name": "dev10", "prerelease": true, "assets": [
            {"name": "nash-linux-x86_64", "browser_download_url": "https://x/dev10/nash-linux-x86_64", "digest": "sha256:short"},
            {"name": "nash-linux-aarch64", "state": "starter", "browser_download_url": "https://x/half"}]},
        {"tag_name": "v0.1.0", "assets": [
            {"name": "nash-linux-x86_64", "state": "uploaded", "browser_download_url": "https://x/010",
             "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}]},
        {"tag_name": "v0.1.0.7", "assets": [
            {"name": "nash-linux-x86_64", "browser_download_url": "https://x/0107"}]},
        {"tag_name": "nightly", "assets": [{"name": "nash-linux-x86_64", "browser_download_url": "https://x/n"}]},
        {"tag_name": "v.4", "assets": [{"name": "nash-linux-x86_64", "browser_download_url": "https://x/v4"}]}
    ]"#;

    #[test]
    fn picks_releases_with_the_binary() {
        let listing = parse_json(RELEASES).unwrap();
        let linux = builds(&listing, "nash-linux-x86_64");
        let got: Vec<(&str, &str, &str)> = linux
            .iter()
            .map(|b| (b.version.as_str(), b.channel, b.url.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("0.1.0.7", "stable", "https://x/0107"),
                ("0.1.0", "stable", "https://x/010"),
                ("dev10", "dev", "https://x/dev10/nash-linux-x86_64"),
                ("dev9", "dev", "https://x/dev9/nash-linux-x86_64"),
            ]
        );
        let self_builds = builds(&listing, "nashed-linux-x86_64");
        assert_eq!(self_builds.len(), 1);
        assert_eq!(self_builds[0].version, "dev9");
        assert_eq!(linux[1].sha256.as_deref(), Some("b".repeat(64).as_str()));
        assert_eq!((linux[2].sha256.as_deref(), linux[2].sums_url.as_deref()), (None, None));
        assert_eq!(
            (linux[3].sha256.as_deref(), linux[3].sums_url.as_deref()),
            (None, Some("https://x/dev9/SHA256SUMS"))
        );
        let mac = builds(&listing, "nash-macos-aarch64");
        assert_eq!(mac.len(), 1);
        assert_eq!((mac[0].version.as_str(), mac[0].sha256.clone()), ("0.1.1", Some("a".repeat(64))));
        assert!(builds(&listing, "nash-linux-aarch64").is_empty());
        assert!(builds(&parse_json("{\"message\": \"rate limit\"}").unwrap(), "nash-linux-x86_64").is_empty());
    }

    #[test]
    fn checksums() {
        let h = "c".repeat(64);
        let sums = format!(
            "{}  nash-linux-aarch64\n{} *nashed-linux-x86_64\nshort  nash-macos-x86_64\n",
            "d".repeat(64),
            h.to_uppercase()
        );
        assert_eq!(checksum_in(&sums, "nashed-linux-x86_64"), Some(h));
        assert_eq!(checksum_in(&sums, "nash-macos-x86_64"), None);
        assert_eq!(checksum_in("<html>", "nash-linux-x86_64"), None);
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

    #[test]
    fn sha256_vectors() {
        let digest = |b: &[u8]| {
            let mut h = Sha256::new();
            h.update(b);
            h.finalize_hex()
        };
        assert_eq!(digest(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(digest(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let mut h = Sha256::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            h.finalize_hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn https_url_validation() {
        assert!(validate_https_url("https://api.github.com/repos/fourtexec/nash").is_ok());
        assert!(validate_https_url("http://example.com/nash").is_err());
        assert!(validate_https_url("file:///etc/passwd").is_err());
        assert!(validate_https_url("-K/etc/passwd").is_err());
        assert!(validate_https_url("https://example.com/a b").is_err());
        assert!(validate_https_url("https:///no-host").is_err());
        let bad_hex = format!("{}  nash-linux-x86_64\n", "z".repeat(64));
        assert_eq!(checksum_in(&bad_hex, "nash-linux-x86_64"), None);
    }
}
