use std::process::Command;

#[test]
fn cli_help_version_and_security() {
    let bin = env!("CARGO_BIN_EXE_nashed");

    let o = Command::new(bin).arg("--version").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("nashed "));

    let o = Command::new(bin).arg("--help").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let help = String::from_utf8_lossy(&o.stdout);
    assert!(help.contains("install"));
    assert!(help.contains("update self"));
    assert!(help.contains("uninstall"));

    // Reject insecure non-HTTPS URLs for both nash and self updates
    for args in [
        &["update", "--list"][..],
        &["update", "self", "--list"][..],
        &["install"][..],
        &["install", "dev"][..],
        &["install", "1.0.1"][..],
    ] {
        let o = Command::new(bin)
            .args(args)
            .env("NASH_UPDATE_URL", "http://example.com/releases")
            .output()
            .unwrap();
        assert_ne!(o.status.code(), Some(0), "expected failure for {:?}", args);
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("HTTPS"),
            "expected HTTPS error for {:?}, got: {}",
            args,
            String::from_utf8_lossy(&o.stderr)
        );
    }

    // Reject prefix with control characters / newlines
    let o = Command::new(bin)
        .args(["install", "--prefix", "/tmp/bad\nprefix", "--no-chsh"])
        .output()
        .unwrap();
    assert_ne!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stderr).contains("invalid control characters"));

    // Reject a shell script pretending to be a native binary
    let tmp = std::env::temp_dir().join(format!("nashed-cli-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let fake = tmp.join("fake_nash");
    std::fs::write(&fake, "#!/bin/sh\necho nash 9.9.9\n").unwrap();
    let o = Command::new(bin)
        .args([
            "install",
            "--no-chsh",
            "--no-logo",
            "--prefix",
            tmp.to_str().unwrap(),
            fake.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_ne!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stderr).contains("script"));

    // Reject invalid version names cleanly
    let o = Command::new(bin).args(["install", "not_a_ver"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));

    // Uninstall removes binary and managed rc blocks
    let fake_home = tmp.join("home");
    let fake_bin = tmp.join("prefix").join("bin");
    std::fs::create_dir_all(&fake_home).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    let installed_nash = fake_bin.join("nash");
    std::fs::write(&installed_nash, b"dummy").unwrap();
    let bashrc = fake_home.join(".bashrc");
    std::fs::write(
        &bashrc,
        "export KEEP=1\n# >>> nash logo >>>\necho logo\n# <<< nash logo <<<\n",
    )
    .unwrap();
    let o = Command::new(bin)
        .args(["uninstall", "--prefix", tmp.join("prefix").to_str().unwrap()])
        .env("HOME", &fake_home)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(!installed_nash.exists());
    let rc_after = std::fs::read_to_string(&bashrc).unwrap();
    assert!(rc_after.contains("export KEEP=1"));
    assert!(!rc_after.contains("nash logo"));

    let _ = std::fs::remove_dir_all(&tmp);
}
