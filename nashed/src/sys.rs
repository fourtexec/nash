//! Thin OS helpers for `nashed`.

use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;

pub fn geteuid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}

pub fn getpid() -> i32 {
    unsafe { libc::getpid() }
}

pub fn isatty(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    unsafe { libc::isatty(fd) != 0 }
}

pub fn is_dir(path: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

pub fn access(path: &str, mode: i32) -> bool {
    let Ok(c) = CString::new(path) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), mode) == 0 }
}

pub fn is_executable(path: &str) -> bool {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        _ => return false,
    }
    access(path, libc::X_OK)
}

pub fn writable_dir(dir: &str) -> bool {
    is_dir(dir) && access(dir, libc::W_OK)
}

/// Locate an executable on `PATH` without spawning a shell.
pub fn find_in_path(cmd: &str) -> Option<String> {
    if cmd.is_empty() {
        return None;
    }
    if cmd.contains('/') {
        return is_executable(cmd).then(|| cmd.to_string());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let full = if dir.is_empty() {
            cmd.to_string()
        } else {
            format!("{}/{}", dir.trim_end_matches('/'), cmd)
        };
        if is_executable(&full) {
            return Some(full);
        }
    }
    None
}

/// Generate a non-zero 64-bit random value from `/dev/urandom`, falling back
/// to high-resolution time and process ID if `/dev/urandom` is unavailable.
pub fn random_u64() -> u64 {
    let mut buf = [0u8; 8];
    if let Ok(mut f) = File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            let v = u64::from_ne_bytes(buf);
            if v != 0 {
                return v;
            }
        }
    }
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let v = t ^ ((getpid() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    if v == 0 {
        0x517C_C1B7_2722_0A95
    } else {
        v
    }
}

/// Exclusively create a new temporary file with an unpredictable name and
/// explicit permissions (`O_CREAT | O_EXCL`), preventing symlink attacks.
pub fn create_temp_file(dir: &str, prefix: &str, mode: u32) -> std::io::Result<(String, File)> {
    let dir = dir.trim_end_matches('/');
    let dir = if dir.is_empty() { "." } else { dir };
    let pid = getpid();
    for _ in 0..64 {
        let rand = random_u64();
        let path = format!("{}/{}{}-{:016x}", dir, prefix, pid, rand);
        match OpenOptions::new().write(true).create_new(true).mode(mode).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "failed to create unique temporary file",
    ))
}

fn passwd_for_name(name: &str) -> Option<(String, String)> {
    let c = CString::new(name).ok()?;
    unsafe {
        let pw = libc::getpwnam(c.as_ptr());
        if pw.is_null() {
            return None;
        }
        let dir = if (*pw).pw_dir.is_null() {
            String::new()
        } else {
            CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned()
        };
        let shell = if (*pw).pw_shell.is_null() {
            String::new()
        } else {
            CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned()
        };
        Some((dir, shell))
    }
}

pub fn current_user() -> (String, Option<String>) {
    // When invoked under `sudo`, target the invoking user rather than root so
    // `chsh` and `~/.bashrc` apply to the user's account.
    if geteuid() == 0 {
        if let Ok(sudo_user) = std::env::var("SUDO_USER") {
            if !sudo_user.is_empty() && sudo_user != "root" {
                if let Some((dir, _)) = passwd_for_name(&sudo_user) {
                    let home = if dir.is_empty() { None } else { Some(dir) };
                    return (sudo_user, home);
                }
            }
        }
    }
    unsafe {
        let pw = libc::getpwuid(libc::geteuid());
        if pw.is_null() {
            return (
                std::env::var("USER").unwrap_or_else(|_| "user".into()),
                None,
            );
        }
        let name = if (*pw).pw_name.is_null() {
            std::env::var("USER").unwrap_or_else(|_| "user".into())
        } else {
            CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
        };
        let dir = if (*pw).pw_dir.is_null() {
            None
        } else {
            Some(CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
        };
        (name, dir)
    }
}

/// Return the configured login shell for `user`, or empty string if unknown.
pub fn get_login_shell_for(user: &str) -> String {
    if !user.is_empty() {
        if let Some((_, shell)) = passwd_for_name(user) {
            if !shell.is_empty() {
                return shell;
            }
        }
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            String::new()
        } else {
            CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned()
        }
    }
}

/// Return the current user's configured login shell, or empty string if unknown.
pub fn get_login_shell() -> String {
    let (user, _) = current_user();
    get_login_shell_for(&user)
}

/// Safely single-quote a string for POSIX shell snippets.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-' | b'_'))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Reject control characters or newlines in a shell path before writing to
/// `/etc/shells` or passing to `chsh`.
pub fn is_safe_shell_path(path: &str) -> bool {
    !path.is_empty() && !path.bytes().any(|b| b < 0x20 || b == 0x7f)
}
