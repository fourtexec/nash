//! `nash update PATH`: replace the running binary with a nash binary the user
//! downloaded themselves (for example from the GitHub releases page).
//!
//! nash does no network access. Before anything is replaced the new file is
//! checked: it must be an executable built for this operating system and CPU,
//! and it must actually run here and identify itself as nash.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

const HELP: &str = "\
usage: nash update [--check] PATH

Replaces the nash binary that is running with the nash binary at PATH.
Download the binary for your platform yourself, for example from
https://github.com/fourtexec/nash/releases, then run `nash update PATH'.

Before anything is replaced, PATH is checked:
  1. it is an executable built for this operating system and CPU
  2. it runs on this system and reports itself as nash

  --check   only run the checks, do not replace anything
            (exit status 0: PATH can be installed, 1: it cannot)
";

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

pub fn update(args: &[String]) -> i32 {
    let mut check = false;
    let mut path: Option<&str> = None;
    let mut options = true;
    for a in args {
        match a.as_str() {
            "--" if options => options = false,
            "--check" if options => check = true,
            "-h" | "--help" if options => {
                out!("{}", HELP);
                return 0;
            }
            other if options && other.starts_with('-') => {
                err!("nash update: unknown option `{}'\n{}", other, HELP);
                return 2;
            }
            other if path.is_none() => path = Some(other),
            _ => {
                err!("nash update: only one binary can be given\n{}", HELP);
                return 2;
            }
        }
    }
    let path = match path {
        Some(p) => p,
        None => {
            err!("nash update: give the path to the new nash binary\n{}", HELP);
            return 2;
        }
    };
    let current = env!("CARGO_PKG_VERSION");

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

#[cfg(test)]
mod tests {
    use super::{check_platform, identify};

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
