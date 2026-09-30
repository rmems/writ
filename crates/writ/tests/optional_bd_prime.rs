#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("writ-optional-bd-prime-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/optional-bd-prime.sh")
        .canonicalize()
        .unwrap()
}

fn run_with_bin_dir(bin_dir: &std::path::Path) -> std::process::Output {
    let path = format!("{}:/usr/bin:/bin", bin_dir.display());
    Command::new("bash")
        .arg(script())
        .env("PATH", path)
        .output()
        .unwrap()
}

#[test]
fn missing_bd_is_success() {
    let empty = TestDir::new();
    let output = run_with_bin_dir(&empty.0);
    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn present_bd_is_invoked() {
    let dir = TestDir::new();
    let stub = dir.0.join("bd");
    fs::write(&stub, "#!/usr/bin/env bash\necho PRIME-RAN \"$@\"\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let output = run_with_bin_dir(&dir.0);
    assert!(
        output.status.success(),
        "status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PRIME-RAN"), "stdout={stdout}");
}

#[test]
fn bd_failure_is_not_masked() {
    let dir = TestDir::new();
    let stub = dir.0.join("bd");
    fs::write(&stub, "#!/usr/bin/env bash\nexit 3\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let output = run_with_bin_dir(&dir.0);
    assert_eq!(output.status.code(), Some(3));
}
