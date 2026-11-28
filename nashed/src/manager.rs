//! Install, update, self-update, list, and uninstall commands for `nashed`.

use crate::release::{self, Fetcher, ResolveError};
use crate::sys;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

const LOGO_START: &str = "# >>> nash logo >>>";
const LOGO_END: &str = "# <<< nash logo <<<";
const PATH_START: &str = "# >>> nash >>>";
const PATH_END: &str = "# <<< nash <<<";

const INSTALL_HELP: &str = "\
usage: nashed install [OPTIONS] [stable | dev | VERSION | PATH]

Installs the nash shell from fourtexec/nash GitHub releases:
  nashed install            install the latest stable release of nash
  nashed install dev        install the latest development build of nash
  nashed install VERSION    install a specific version (e.g. 1.0.1, dev7)
  nashed install PATH       install a local nash binary (argument with `/')

options:
  --prefix DIR   install into DIR/bin (default: /usr/local, ~/.local, or $PREFIX on Termux)
  --no-chsh      do not add nash to /etc/shells or change the login shell
  --no-logo      do not add the startup logo hook to ~/.bashrc
  --force        reinstall even if the requested version is already installed
  -h, --help     show this help
";

const UPDATE_HELP: &str = "\
usage: nashed update [OPTIONS] [stable | dev | VERSION | PATH]
       nashed update self [OPTIONS] [stable | dev | VERSION | PATH]

Updates nash (or nashed itself with `self`) from fourtexec/nash GitHub releases:
  nashed update             update nash to the latest stable release
  nashed update dev         update nash to the latest development build
  nashed update VERSION     switch nash to a specific version (1.0.1, dev7)
  nashed update PATH        replace nash with a local binary (argument with `/')
  nashed update self        update nashed itself to the latest stable release
  nashed update self dev    update nashed itself to the latest development build

options:
  --prefix DIR   target nash inside DIR/bin
  --check        only check if an update is available (exit 0: up to date, 1: update available)
  --force        reinstall even if this version is already installed
  --list         list releases available for this platform
  --chsh         also register nash in /etc/shells and set it as login shell
  --no-chsh      do not update /etc/shells or the login shell
  -h, --help     show this help
";

const UNINSTALL_HELP: &str = "\
usage: nashed uninstall [--prefix DIR]

Removes the installed nash binary, managed rc blocks, and /etc/shells entries.
";

fn user_home() -> (String, String) {
    let (user, pw_home) = sys::current_user();
    let sudo_active = sys::geteuid() == 0
        && std::env::var("SUDO_USER")
            .map(|u| !u.is_empty() && u != "root")
            .unwrap_or(false);
    let home = if sudo_active {
        pw_home
            .or_else(|| std::env::var("HOME").ok().filter(|h| !h.is_empty()))
            .unwrap_or_else(|| "/".into())
    } else {
        std::env::var("HOME")
            .ok()
            .filter(|h| !h.is_empty())
            .or(pw_home)
            .unwrap_or_else(|| "/".into())
    };
    (user, home)
}

fn same_file(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (Path::new(a).canonicalize(), Path::new(b).canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a.trim_end_matches('/') == b.trim_end_matches('/'),
    }
}

fn is_nash_shell_path(path: &str) -> bool {
    let t = path.trim();
    t == "nash" || t.ends_with("/nash")
}

fn parent_writable(path: &str) -> bool {
    Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .map(sys::writable_dir)
        .unwrap_or(false)
}

fn is_android_termux() -> bool {
    cfg!(target_os = "android") || std::env::var("TERMUX_VERSION").map(|v| !v.is_empty()).unwrap_or(false)
}

fn default_bindir(prefix: Option<&str>, home: &str) -> String {
    if let Some(p) = prefix {
        return format!("{}/bin", p.trim_end_matches('/'));
    }
    if is_android_termux() {
        let p = std::env::var("PREFIX")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/data/data/com.termux/files/usr".to_string());
        return format!("{}/bin", p.trim_end_matches('/'));
    }
    if sys::geteuid() == 0 || sys::writable_dir("/usr/local/bin") {
        "/usr/local/bin".to_string()
    } else {
        format!("{}/.local/bin", home.trim_end_matches('/'))
    }
}

/// Locate an existing installed `nash` binary (checking `PATH`, configured login
/// shell, and standard install locations).
fn find_installed_nash(prefix: Option<&str>, home: &str, user: &str) -> Option<String> {
    if let Some(p) = prefix {
        let cand = format!("{}/bin/nash", p.trim_end_matches('/'));
        return sys::is_executable(&cand).then_some(cand);
    }
    if let Some(found) = sys::find_in_path("nash") {
        return Some(found);
    }
    let login = sys::get_login_shell_for(user);
    if is_nash_shell_path(&login) && sys::is_executable(&login) {
        return Some(login);
    }
    let mut cands = vec![
        format!("{}/.local/bin/nash", home.trim_end_matches('/')),
        "/usr/local/bin/nash".to_string(),
    ];
    if let Ok(p) = std::env::var("PREFIX") {
        if !p.is_empty() {
            cands.push(format!("{}/bin/nash", p.trim_end_matches('/')));
        }
    }
    cands.push("/data/data/com.termux/files/usr/bin/nash".to_string());
    cands.into_iter().find(|c| sys::is_executable(c))
}

/// Choose `(bindir, dest)` for `nashed install`. If no `--prefix` is given and
/// an existing `nash` binary (login shell or in `PATH`) is writable, reuse its
/// directory so `nashed install` updates it in place instead of creating a
/// second copy.
fn choose_install_dest(prefix: Option<&str>, home: &str, user: &str) -> (String, String) {
    if let Some(p) = prefix {
        let bindir = default_bindir(Some(p), home);
        let dest = format!("{}/nash", bindir.trim_end_matches('/'));
        return (bindir, dest);
    }
    let login = sys::get_login_shell_for(user);
    if is_nash_shell_path(&login) && sys::is_executable(&login) && parent_writable(&login) {
        if let Some(dir) = Path::new(&login).parent().and_then(|p| p.to_str()) {
            return (dir.to_string(), login);
        }
    }
    if let Some(found) = sys::find_in_path("nash") {
        if parent_writable(&found) {
            if let Some(dir) = Path::new(&found).parent().and_then(|p| p.to_str()) {
                return (dir.to_string(), found);
            }
        }
    }
    let bindir = default_bindir(None, home);
    let dest = format!("{}/nash", bindir.trim_end_matches('/'));
    (bindir, dest)
}

fn bashrc_logo_block(nash_bin: &str) -> String {
    let quoted = sys::shell_quote(nash_bin);
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

pub fn strip_managed_block(content: &str, start_marker: &str, end_marker: &str) -> String {
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

pub fn install_bashrc_logo(home: &str, nash_bin: &str) -> std::io::Result<String> {
    let bashrc = format!("{}/.bashrc", home.trim_end_matches('/'));
    let existing = std::fs::read_to_string(&bashrc).unwrap_or_default();
    let mut updated = strip_managed_block(&existing, LOGO_START, LOGO_END);
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

fn run_cmd(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

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

fn register_login_shell(dest: &str, user: &str) -> bool {
    let root = sys::geteuid() == 0;
    if is_android_termux() {
        if sys::find_in_path("chsh").is_some() {
            return run_cmd("chsh", &["-s", "nash"]);
        }
        return false;
    }

    let shells = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    if !shells.lines().any(|l| l.trim() == dest) {
        let line = format!(
            "{}{}\n",
            if shells.is_empty() || shells.ends_with('\n') { "" } else { "\n" },
            dest
        );
        let appended = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open("/etc/shells")
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .is_ok();
        if appended {
            println!("added {} to /etc/shells", dest);
        } else if sys::isatty(0) && sys::find_in_path("sudo").is_some() {
            println!("adding {} to /etc/shells (needs sudo)", dest);
            if !append_etc_shells_sudo(&line) {
                eprintln!("nashed: could not update /etc/shells");
                return false;
            }
        } else {
            let quoted = sys::shell_quote(dest);
            eprintln!(
                "nashed: /etc/shells is not writable. Run as root:\n    printf '%s\\n' {} >> /etc/shells\nthen:\n    chsh -s {}",
                quoted, quoted
            );
            return false;
        }
    }

    let current = sys::get_login_shell_for(user);
    if !current.is_empty() && same_file(&current, dest) {
        println!("nash is already the login shell for {} ({})", user, dest);
        return true;
    }

    if !sys::isatty(0) && !root {
        println!(
            "to make nash your login shell, run:\n    chsh -s {}",
            sys::shell_quote(dest)
        );
        return false;
    }

    let ok = if sys::find_in_path("chsh").is_some() {
        if root {
            run_cmd("chsh", &["-s", dest, user])
        } else {
            println!("changing your login shell to {} (chsh may ask for your password)", dest);
            run_cmd("chsh", &["-s", dest])
        }
    } else if root && sys::find_in_path("usermod").is_some() {
        run_cmd("usermod", &["-s", dest, user])
    } else if sys::isatty(0) && sys::find_in_path("sudo").is_some() && sys::find_in_path("usermod").is_some() {
        println!("changing your login shell to {} (needs sudo)", dest);
        run_cmd("sudo", &["usermod", "-s", dest, user])
    } else {
        false
    };
    if !ok {
        eprintln!(
            "nashed: could not change the login shell. Do it by hand:\n    chsh -s {}",
            sys::shell_quote(dest)
        );
        return false;
    }
    println!(
        "nash is now the login shell for {} ({}, was {})\nopen a new terminal or log in again to use it\nto go back: chsh -s {}",
        user,
        dest,
        if current.is_empty() { "unknown" } else { &current },
        if current.is_empty() { "/bin/sh" } else { &current }
    );
    true
}

fn warn_shadowing_nash(dest: &str) {
    if let Some(in_path) = sys::find_in_path("nash") {
        if !same_file(&in_path, dest) {
            eprintln!(
                "warning: another nash binary at {} shadows {} in your PATH",
                in_path, dest
            );
        }
    }
}

/// Keep the login shell and any existing `~/.bashrc` logo hook aligned with
/// `dest` after `nashed update`.
fn sync_after_nash_update(dest: &str, home: &str, user: &str, force_chsh: bool, no_chsh: bool) {
    // If ~/.bashrc already has a managed logo hook, make sure it points to `dest`.
    let bashrc = format!("{}/.bashrc", home.trim_end_matches('/'));
    if let Ok(content) = std::fs::read_to_string(&bashrc) {
        if content.contains(LOGO_START) {
            let _ = install_bashrc_logo(home, dest);
        }
    }

    let login_shell = sys::get_login_shell_for(user);
    if !no_chsh {
        if force_chsh {
            register_login_shell(dest, user);
        } else if is_nash_shell_path(&login_shell) && !same_file(&login_shell, dest) {
            let old_ver = release::binary_version(&login_shell, "nash")
                .map(|v| format!(" (nash {})", v))
                .unwrap_or_default();
            println!(
                "note: your login shell is {}{}, updating login shell to {}",
                login_shell, old_ver, dest
            );
            let changed = register_login_shell(dest, user);
            if Path::new(&login_shell).exists() && !same_file(&login_shell, dest) {
                if changed {
                    println!(
                        "note: an older nash binary still exists at {}; you can remove it to avoid duplicates",
                        login_shell
                    );
                } else {
                    eprintln!(
                        "note: your login shell still points to {}; update it with `chsh -s {}' or update that binary directly",
                        login_shell,
                        sys::shell_quote(dest)
                    );
                }
            }
        }
    }

    warn_shadowing_nash(dest);
}

/// Download, SHA-256 verify, ELF/Mach-O inspect, smoke-test `--version`, and
/// atomically install `binary` (`"nash"` or `"nashed"`) to `dest`.
fn download_and_install(
    fetch: &Fetcher,
    binary: &str,
    asset: &str,
    build: &release::Build,
    dest: &str,
    ctx: &str,
) -> Result<String, i32> {
    let dest_dir = Path::new(dest)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(".");
    if let Err(e) = std::fs::create_dir_all(dest_dir) {
        eprintln!("{}: cannot create {}: {}", ctx, dest_dir, e);
        return Err(1);
    }
    let prefix = format!(".{}-download-", binary);
    let tmp = match sys::create_temp_file(dest_dir, &prefix, 0o600) {
        Ok((path, file)) => {
            drop(file);
            path
        }
        Err(e) => {
            eprintln!("{}: cannot write to {}: {}", ctx, dest, e);
            if sys::geteuid() != 0 {
                eprintln!("try running with sudo or pass --prefix ~/.local");
            }
            return Err(1);
        }
    };
    let fail = |msg: String| -> i32 {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("{}: {}", ctx, msg);
        1
    };

    println!("downloading {} {} ({})", binary, build.version, asset);
    if let Err(e) = fetch.file(&build.url, &tmp) {
        return Err(fail(format!("cannot download {}: {}", build.url, e)));
    }
    let want_hash = match fetch.expected_sha256(build, asset) {
        Ok(h) => h,
        Err(e) => return Err(fail(e)),
    };
    match release::sha256_file(&tmp) {
        Ok(got) if got == want_hash => {}
        Ok(got) => {
            return Err(fail(format!(
                "checksum mismatch for {} (expected {}, got {})",
                asset, want_hash, got
            )));
        }
        Err(e) => return Err(fail(e)),
    }
    if let Err(e) = release::inspect(Path::new(&tmp)) {
        return Err(fail(format!("the downloaded {} {}", asset, e)));
    }
    if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
        return Err(fail(format!("cannot make {} executable: {}", tmp, e)));
    }
    let verified_ver = match release::binary_version(&tmp, binary) {
        Ok(v) => v,
        Err(e) => {
            return Err(fail(format!(
                "the downloaded binary {} ({})",
                e, asset
            )));
        }
    };
    if let Err(e) = std::fs::rename(&tmp, dest) {
        return Err(fail(format!("cannot install to {}: {}", dest, e)));
    }
    Ok(verified_ver)
}

/// Install a local binary `src_path` to `dest` after verifying platform and `--version`.
fn install_local_binary(
    src_path: &str,
    dest: &str,
    binary: &str,
    current: Option<&str>,
    check: bool,
    ctx: &str,
) -> Result<String, i32> {
    let src = Path::new(src_path);
    if let Err(e) = release::inspect(src) {
        eprintln!("{}: {} {}", ctx, src_path, e);
        return Err(1);
    }
    if let (Ok(sc), Ok(dc)) = (src.canonicalize(), Path::new(dest).canonicalize()) {
        if sc == dc {
            eprintln!("{}: {} is already installed at {}", ctx, src_path, dest);
            return Err(1);
        }
    }
    let (tmp_dir, tmp_mode) = if check {
        let dir = std::env::var("TMPDIR")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/tmp".to_string());
        (dir, 0o700u32)
    } else {
        let dir = Path::new(dest)
            .parent()
            .and_then(|p| p.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(".")
            .to_string();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("{}: cannot create {}: {}", ctx, dir, e);
            return Err(1);
        }
        (dir, 0o755u32)
    };
    let tmp_prefix = format!(".{}-local-", binary);
    let (tmp, mut dst_file) = match sys::create_temp_file(&tmp_dir, &tmp_prefix, tmp_mode) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("{}: cannot write to {}: {}", ctx, dest, e);
            return Err(1);
        }
    };
    let fail = |msg: String| -> i32 {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("{}: {}", ctx, msg);
        1
    };
    let copy_res = std::fs::File::open(src)
        .and_then(|mut src_file| std::io::copy(&mut src_file, &mut dst_file))
        .and_then(|_| dst_file.flush());
    drop(dst_file);
    if let Err(e) = copy_res {
        return Err(fail(format!("cannot copy {} to {}: {}", src_path, tmp, e)));
    }
    if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(tmp_mode)) {
        return Err(fail(format!("cannot make {} executable: {}", tmp, e)));
    }
    let version = match release::binary_version(&tmp, binary) {
        Ok(v) => v,
        Err(e) => return Err(fail(format!("{} {}", src_path, e))),
    };
    if check {
        let _ = std::fs::remove_file(&tmp);
        match current {
            Some(c) => println!(
                "{} is {} {} for this system (you have {})",
                src_path, binary, version, c
            ),
            None => println!("{} is {} {} for this system", src_path, binary, version),
        }
        return Ok(version);
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        return Err(fail(format!("cannot replace {}: {}", dest, e)));
    }
    Ok(version)
}

fn post_install_nash(dest: &str, bindir: &str, home: &str, user: &str, add_logo: bool, chsh: bool) {
    let path = std::env::var("PATH").unwrap_or_default();
    if !path.split(':').any(|d| d.trim_end_matches('/') == bindir) {
        println!(
            "note: {} is not in your PATH; add it in ~/.profile:\n    export PATH=\"{}:$PATH\"",
            bindir, bindir
        );
    }
    if add_logo {
        match install_bashrc_logo(home, dest) {
            Ok(rc) => println!("added nash startup logo to {}", rc),
            Err(e) => eprintln!("nashed install: could not update ~/.bashrc: {}", e),
        }
    }
    let prev_login = sys::get_login_shell_for(user);
    if chsh {
        register_login_shell(dest, user);
    }
    if is_nash_shell_path(&prev_login)
        && !same_file(&prev_login, dest)
        && Path::new(&prev_login).exists()
    {
        println!(
            "note: an older nash binary still exists at {}; you can remove it to avoid duplicates",
            prev_login
        );
    }
    warn_shadowing_nash(dest);
}

pub fn cmd_install(args: &[String]) -> i32 {
    let mut prefix: Option<String> = None;
    let mut chsh = true;
    let mut add_logo = true;
    let mut force = false;
    let mut want: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prefix" => {
                i += 1;
                match args.get(i) {
                    Some(p) => prefix = Some(p.clone()),
                    None => {
                        eprintln!("nashed install: --prefix requires a directory");
                        return 2;
                    }
                }
            }
            "--no-chsh" => chsh = false,
            "--no-logo" => add_logo = false,
            "--force" => force = true,
            "-h" | "--help" => {
                print!("{}", INSTALL_HELP);
                return 0;
            }
            other if other.starts_with('-') || want.is_some() => {
                eprintln!("nashed install: unexpected argument `{}'\n{}", other, INSTALL_HELP);
                return 2;
            }
            other => want = Some(other.to_string()),
        }
        i += 1;
    }

    let (user, home) = user_home();
    let (bindir, dest) = choose_install_dest(prefix.as_deref(), &home, &user);
    if !sys::is_safe_shell_path(&dest) {
        eprintln!("nashed install: install path contains invalid control characters");
        return 2;
    }

    if let Some(local_path) = want.as_deref().filter(|w| w.contains('/')) {
        let current = release::binary_version(&dest, "nash").ok();
        return match install_local_binary(
            local_path,
            &dest,
            "nash",
            current.as_deref(),
            false,
            "nashed install",
        ) {
            Ok(ver) => {
                println!("installed nash {} to {}", ver, dest);
                post_install_nash(&dest, &bindir, &home, &user, add_logo, chsh);
                0
            }
            Err(code) => code,
        };
    }

    let base_url = match release::api_base_url() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("nashed install: {}", e);
            return 2;
        }
    };
    let asset = match release::asset_name("nash") {
        Some(a) => a,
        None => {
            eprintln!(
                "nashed install: no release binaries for {}-{}; build from source instead",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            return 1;
        }
    };
    let fetch = match Fetcher::new() {
        Some(f) => f,
        None => {
            eprintln!("nashed install: needs curl or wget");
            return 1;
        }
    };

    let build = match fetch.resolve(&base_url, &asset, want.as_deref()) {
        Ok(Some(b)) => b,
        Ok(None) => {
            match want.as_deref() {
                None | Some("stable") => eprintln!("nashed install: no stable release with {} yet", asset),
                Some("dev") => eprintln!("nashed install: no dev release with {} yet", asset),
                Some(v) => eprintln!(
                    "nashed install: no release `{}' with {}; see `nashed list'",
                    v, asset
                ),
            }
            return 1;
        }
        Err(ResolveError::BadVersion(v)) => {
            eprintln!(
                "nashed install: `{}' is not a version (1.0.1, dev7), `dev' or `stable'",
                v
            );
            return 2;
        }
        Err(ResolveError::Network(e)) => {
            eprintln!("nashed install: cannot ask GitHub for releases: {}", e);
            return 1;
        }
    };

    if !force {
        if let Ok(cur) = release::binary_version(&dest, "nash") {
            let channel = matches!(want.as_deref(), None | Some("stable") | Some("dev"));
            let same = if channel {
                release::not_older(&cur, &build.version)
            } else {
                cur == build.version
            };
            if same {
                println!("nash {} is already installed at {}", cur, dest);
                post_install_nash(&dest, &bindir, &home, &user, add_logo, chsh);
                return 0;
            }
        }
    }

    match download_and_install(&fetch, "nash", &asset, &build, &dest, "nashed install") {
        Ok(_) => {
            println!("installed nash {} to {}", build.version, dest);
            post_install_nash(&dest, &bindir, &home, &user, add_logo, chsh);
            0
        }
        Err(code) => code,
    }
}

fn update_target(
    binary: &str,
    dest: &str,
    current: Option<&str>,
    want: Option<&str>,
    check: bool,
    force: bool,
    list: bool,
    ctx: &str,
) -> i32 {
    if let Some(local_path) = want.filter(|w| w.contains('/')) {
        return match install_local_binary(local_path, dest, binary, current, check, ctx) {
            Ok(ver) => {
                if !check {
                    match current {
                        Some(cur) => println!("updated {} from {} {} to {} {}", dest, binary, cur, binary, ver),
                        None => println!("installed {} {} to {}", binary, ver, dest),
                    }
                }
                0
            }
            Err(code) => code,
        };
    }

    let base_url = match release::api_base_url() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("{}: {}", ctx, e);
            return 2;
        }
    };
    let asset = match release::asset_name(binary) {
        Some(a) => a,
        None => {
            eprintln!(
                "{}: no release binaries for {}-{}; build from source instead",
                ctx,
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            return 1;
        }
    };
    let fetch = match Fetcher::new() {
        Some(f) => f,
        None => {
            eprintln!("{}: needs curl or wget", ctx);
            return 1;
        }
    };

    if list {
        return match fetch.recent(&base_url, &asset) {
            Ok(available) => {
                for b in &available {
                    let mark = if current == Some(b.version.as_str()) {
                        "  (installed)"
                    } else {
                        ""
                    };
                    println!("{:<12} {}{}", b.version, b.channel, mark);
                }
                0
            }
            Err(e) => {
                eprintln!("{}: cannot ask GitHub for releases: {}", ctx, e);
                1
            }
        };
    }

    let channel = match want {
        None | Some("stable") => Some("stable"),
        Some("dev") => Some("dev"),
        Some(_) => None,
    };
    let build = match fetch.resolve(&base_url, &asset, want) {
        Ok(Some(b)) => b,
        Ok(None) => {
            match channel {
                Some(c) => eprintln!("{}: no {} release with {} yet", ctx, c, asset),
                None => eprintln!(
                    "{}: no release `{}' with {}; see `nashed list{}'",
                    ctx,
                    want.unwrap_or(""),
                    asset,
                    if binary == "nashed" { " self" } else { "" }
                ),
            }
            return 1;
        }
        Err(ResolveError::BadVersion(v)) => {
            eprintln!(
                "{}: `{}' is not a version (1.0.1, dev7), `dev' or `stable'",
                ctx, v
            );
            return 2;
        }
        Err(ResolveError::Network(e)) => {
            eprintln!("{}: cannot ask GitHub for releases: {}", ctx, e);
            return 1;
        }
    };

    let latest = build.version.as_str();
    let up_to_date = match current {
        Some(cur) => match channel {
            Some(_) => release::not_older(cur, latest),
            None => cur == latest,
        },
        None => false,
    };

    if up_to_date && !force {
        println!("{} {} is up to date", binary, current.unwrap_or(latest));
        return 0;
    }
    if check {
        if up_to_date {
            println!("{} {} is up to date", binary, current.unwrap_or(latest));
            return 0;
        }
        match current {
            Some(cur) => println!(
                "{} {} is available (you have {}); run `{}{}'",
                binary,
                latest,
                cur,
                ctx,
                want.map(|w| format!(" {}", w)).unwrap_or_default()
            ),
            None => println!(
                "{} {} is available ({} is not installed); run `nashed install{}'",
                binary,
                latest,
                binary,
                want.map(|w| format!(" {}", w)).unwrap_or_default()
            ),
        }
        return 1;
    }

    match download_and_install(&fetch, binary, &asset, &build, dest, ctx) {
        Ok(_) => {
            match current {
                Some(cur) => println!("updated {} from {} to {}", dest, cur, latest),
                None => println!("installed {} {} to {}", binary, latest, dest),
            }
            0
        }
        Err(code) => code,
    }
}

pub fn cmd_update(args: &[String]) -> i32 {
    // `nashed update self ...` updates `nashed` itself.
    if args.first().map(|a| a.as_str()) == Some("self") {
        return cmd_update_self(&args[1..]);
    }

    let mut check = false;
    let mut force = false;
    let mut list = false;
    let mut force_chsh = false;
    let mut no_chsh = false;
    let mut prefix: Option<String> = None;
    let mut want: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--check" => check = true,
            "--force" => force = true,
            "--list" => list = true,
            "--chsh" => force_chsh = true,
            "--no-chsh" => no_chsh = true,
            "--prefix" => {
                i += 1;
                match args.get(i) {
                    Some(p) => prefix = Some(p.clone()),
                    None => {
                        eprintln!("nashed update: --prefix requires a directory");
                        return 2;
                    }
                }
            }
            "-h" | "--help" => {
                print!("{}", UPDATE_HELP);
                return 0;
            }
            other if other.starts_with('-') || want.is_some() => {
                eprintln!("nashed update: unexpected argument `{}'\n{}", other, UPDATE_HELP);
                return 2;
            }
            other => want = Some(other.to_string()),
        }
        i += 1;
    }

    let (user, home) = user_home();
    let dest = match find_installed_nash(prefix.as_deref(), &home, &user) {
        Some(p) => p,
        None => {
            let bindir = default_bindir(prefix.as_deref(), &home);
            format!("{}/nash", bindir.trim_end_matches('/'))
        }
    };
    if !sys::is_safe_shell_path(&dest) {
        eprintln!("nashed update: install path contains invalid control characters");
        return 2;
    }
    let current = if sys::is_executable(&dest) {
        release::binary_version(&dest, "nash").ok()
    } else {
        None
    };

    let code = update_target(
        "nash",
        &dest,
        current.as_deref(),
        want.as_deref(),
        check,
        force,
        list,
        "nashed update",
    );
    if code == 0 && !check && !list {
        sync_after_nash_update(&dest, &home, &user, force_chsh, no_chsh);
    }
    code
}

fn cmd_update_self(args: &[String]) -> i32 {
    let mut check = false;
    let mut force = false;
    let mut list = false;
    let mut want: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--check" => check = true,
            "--force" => force = true,
            "--list" => list = true,
            "-h" | "--help" => {
                print!("{}", UPDATE_HELP);
                return 0;
            }
            other if other.starts_with('-') || want.is_some() => {
                eprintln!(
                    "nashed update self: unexpected argument `{}'\n{}",
                    other, UPDATE_HELP
                );
                return 2;
            }
            other => want = Some(other.to_string()),
        }
    }

    let dest = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => {
            eprintln!("nashed update self: cannot locate the running binary: {}", e);
            return 1;
        }
    };
    let current = crate::version();
    update_target(
        "nashed",
        &dest,
        Some(current),
        want.as_deref(),
        check,
        force,
        list,
        "nashed update self",
    )
}

pub fn cmd_list(args: &[String]) -> i32 {
    let mut self_mode = false;
    let mut prefix: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "self" => self_mode = true,
            "--prefix" => {
                i += 1;
                match args.get(i) {
                    Some(p) => prefix = Some(p.clone()),
                    None => {
                        eprintln!("nashed list: --prefix requires a directory");
                        return 2;
                    }
                }
            }
            "-h" | "--help" => {
                println!("usage: nashed list [self] [--prefix DIR]\n\nLists available nash (or nashed) releases for this platform.");
                return 0;
            }
            other => {
                eprintln!("nashed list: unexpected argument `{}'", other);
                return 2;
            }
        }
        i += 1;
    }

    if self_mode {
        return cmd_update_self(&["--list".to_string()]);
    }
    let mut forward = vec!["--list".to_string()];
    if let Some(p) = prefix {
        forward.push("--prefix".to_string());
        forward.push(p);
    }
    cmd_update(&forward)
}

pub fn cmd_uninstall(args: &[String]) -> i32 {
    let mut prefix: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prefix" => {
                i += 1;
                match args.get(i) {
                    Some(p) => prefix = Some(p.clone()),
                    None => {
                        eprintln!("nashed uninstall: --prefix requires a directory");
                        return 2;
                    }
                }
            }
            "-h" | "--help" => {
                print!("{}", UNINSTALL_HELP);
                return 0;
            }
            other => {
                eprintln!("nashed uninstall: unknown option `{}'\n{}", other, UNINSTALL_HELP);
                return 2;
            }
        }
        i += 1;
    }

    let (_, home) = user_home();
    let home_trimmed = home.trim_end_matches('/');

    for rc in [
        format!("{}/.profile", home_trimmed),
        format!("{}/.bashrc", home_trimmed),
        format!("{}/.zshrc", home_trimmed),
        format!("{}/.nashrc", home_trimmed),
    ] {
        if let Ok(s) = std::fs::read_to_string(&rc) {
            let out = strip_managed_block(&s, PATH_START, PATH_END);
            let out = strip_managed_block(&out, LOGO_START, LOGO_END);
            let _ = std::fs::write(&rc, out);
        }
    }

    if let Ok(s) = std::fs::read_to_string("/etc/shells") {
        let filtered: String = s
            .lines()
            .filter(|l| !l.trim().ends_with("/nash"))
            .collect::<Vec<_>>()
            .join("\n");
        let filtered = if s.ends_with('\n') {
            format!("{}\n", filtered)
        } else {
            filtered
        };
        let _ = std::fs::write("/etc/shells", filtered);
    }

    let mut candidates = Vec::new();
    if let Some(p) = prefix {
        candidates.push(format!("{}/bin/nash", p.trim_end_matches('/')));
    } else {
        candidates.push(format!("{}/.local/bin/nash", home_trimmed));
        candidates.push("/usr/local/bin/nash".to_string());
        candidates.push("/data/data/com.termux/files/usr/bin/nash".to_string());
        if let Ok(p) = std::env::var("PREFIX") {
            if !p.is_empty() {
                candidates.push(format!("{}/bin/nash", p.trim_end_matches('/')));
            }
        }
    }
    for c in &candidates {
        if std::fs::remove_file(c).is_ok() {
            println!("removed {}", c);
        }
    }

    let current = sys::get_login_shell();
    if current.ends_with("/nash") {
        println!(
            "nash was your login shell. Switch back with:\n    chsh -s /bin/sh\n(or another shell of your choice)"
        );
    }
    println!("nash uninstalled. Open a new terminal to apply PATH changes.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_path_validation() {
        assert!(sys::is_safe_shell_path("/usr/local/bin/nash"));
        assert!(!sys::is_safe_shell_path(""));
        assert!(!sys::is_safe_shell_path("/tmp/foo\n/bin/sh"));
        assert!(!sys::is_safe_shell_path("/tmp/foo\0bar"));
        assert!(!sys::is_safe_shell_path("/tmp/foo\rbar"));
        assert!(is_nash_shell_path("/usr/local/bin/nash"));
        assert!(is_nash_shell_path("/home/u/.local/bin/nash"));
        assert!(!is_nash_shell_path("/bin/bash"));
        assert!(!is_nash_shell_path("/usr/local/bin/my-nash"));
        assert!(same_file("/usr/local/bin/nash", "/usr/local/bin/nash"));
        assert!(!same_file("/usr/local/bin/nash", "/home/u/.local/bin/nash"));
    }

    #[test]
    fn bashrc_logo_install_is_idempotent_and_removable() {
        let base = std::env::temp_dir().join(format!("nashed-bashrc-test-{}", std::process::id()));
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

        install_bashrc_logo(home, "/opt/my bin/nash").unwrap();
        let second = std::fs::read_to_string(&rc_path).unwrap();
        assert_eq!(second.matches(LOGO_START).count(), 1);
        assert!(second.contains("'/opt/my bin/nash' --logo"));
        assert!(!second.contains("/usr/local/bin/nash"));

        let cleaned = strip_managed_block(&second, LOGO_START, LOGO_END);
        assert!(cleaned.contains("export FOO=bar"));
        assert!(!cleaned.contains(LOGO_START));
        assert!(!cleaned.contains("--logo"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
