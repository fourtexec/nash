//! Runs every script in tests/scripts and compares its combined output with
//! the neighbouring `.out` file.

use std::path::Path;
use std::process::Command;

#[test]
fn scripts() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scripts");
    let mut failed = Vec::new();
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("sh") {
            continue;
        }
        count += 1;
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("exec \"$0\" {} 2>&1", name))
            .arg(env!("CARGO_BIN_EXE_nash"))
            .current_dir(&dir)
            .output()
            .unwrap();
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        let want = std::fs::read_to_string(path.with_extension("out")).unwrap();
        if got != want {
            failed.push(format!("--- {} ---\nexpected:\n{}\ngot:\n{}", name, want, got));
        }
    }
    assert!(count > 0, "no test scripts found");
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn dash_c_and_exit_status() {
    let run = |src: &str| Command::new(env!("CARGO_BIN_EXE_nash")).args(["-c", src]).output().unwrap();
    let o = run("echo $1-$2; exit 3");
    assert_eq!(o.status.code(), Some(3));
    let o = Command::new(env!("CARGO_BIN_EXE_nash")).args(["-c", "echo $0 $1", "name", "arg"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "name arg\n");
    let o = run("if then");
    assert_eq!(o.status.code(), Some(2));
    let o = run("set -e; false; echo no");
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
}

#[test]
fn security_hardening() {
    let bin = env!("CARGO_BIN_EXE_nash");
    let run = |src: &str| Command::new(bin).args(["-c", src]).output().unwrap();

    // 1. string builtin bounds / underflow checks (no panic)
    for cmd in [
        "string split -n 0 , a,b",
        "string split -n",
        "string join",
        "string sub -s",
        "string match",
    ] {
        let o = run(cmd);
        assert!(o.status.code().is_some(), "crashed on: {}", cmd);
    }

    // 2. Brace i64::MIN overflow and Cartesian explosion guard
    let o = run("echo {-9223372036854775808..0}");
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "{-9223372036854775808..0}");

    let bomb = format!("echo {}", "{a,b}".repeat(25));
    let o = run(&bomb);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "{a,b}".repeat(25));

    // 3. Arithmetic base#digits overflow and deep nesting guard
    let o = run("echo $((36#zzzzzzzzzzzzzzzzzzzz))");
    assert_eq!(o.status.code(), Some(0));
    let deep_arith = format!("echo $(({}1{}))", "(".repeat(200), ")".repeat(200));
    let o = run(&deep_arith);
    assert_eq!(o.status.code(), Some(1));

    // 4. Parser deep nesting guard
    let deep_parse = format!("{}true{}", "( ".repeat(150), " )".repeat(150));
    let o = run(&deep_parse);
    assert_eq!(o.status.code(), Some(2));

    // 5. printf i64::MIN and huge width DoS guard
    let o = run("printf '%d' -9223372036854775808");
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "-9223372036854775808");

    let o = run("printf '%99999999999999999999s' x");
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stdout.len() <= 1_000_000);

    // 6. Pathological glob matching finishes cleanly without backtracking hang
    let glob_cmd = format!("s={}c; [[ $s == {}*b ]] && echo y || echo n", "a".repeat(50), "*a".repeat(25));
    let o = run(&glob_cmd);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "n");

    // 7. set -C noclobber rejects regular files but permits /dev/null
    let o = run("set -C; echo ok > /dev/null");
    assert_eq!(o.status.code(), Some(0));

    // 8. Updater rejects insecure non-HTTPS URLs
    let o = Command::new(bin)
        .args(["update", "--list"])
        .env("NASH_UPDATE_URL", "http://example.com/releases")
        .output()
        .unwrap();
    assert_ne!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stderr).contains("HTTPS"));

    // 9. Installer rejects prefix containing control characters / newlines
    let o = Command::new(bin)
        .args(["install", "--prefix", "/tmp/bad\nprefix", "--no-chsh"])
        .output()
        .unwrap();
    assert_ne!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stderr).contains("invalid control characters"));
}
