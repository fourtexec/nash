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
