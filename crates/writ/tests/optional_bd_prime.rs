#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "writ-optional-bd-prime-{}-{id}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
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

/// Resolve bash before replacing `PATH`, so the child does not search `/usr/bin` or `/bin`.
fn bash_exe() -> PathBuf {
    let output = Command::new("bash")
        .args(["-c", "command -v bash"])
        .output()
        .expect("resolve bash before PATH replacement");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert!(
        path.is_absolute(),
        "bash path {path:?} must be absolute so the test can drop system PATH"
    );
    path
}

fn run_with_bin_dir(bin_dir: &Path) -> std::process::Output {
    Command::new(bash_exe())
        .arg(script())
        .env("PATH", bin_dir)
        .env_remove("BASH_ENV")
        .output()
        .unwrap()
}

fn install_bd(dir: &Path, body: &str) {
    let stub = dir.join("bd");
    let contents = format!("#!{}\n{body}", bash_exe().display());
    fs::write(&stub, contents).unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();
}

#[test]
fn missing_bd_is_success() {
    let root = TestDir::new();
    let empty = root.0.join("empty");
    let decoy = root.0.join("decoy");
    fs::create_dir(&empty).unwrap();
    fs::create_dir(&decoy).unwrap();
    // A failing `bd` outside PATH must not run. PATH is only `empty`.
    install_bd(&decoy, "echo SHOULD-NOT-RUN\nexit 9\n");

    let output = run_with_bin_dir(&empty);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "status={:?} stdout={stdout} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !stdout.contains("SHOULD-NOT-RUN"),
        "decoy bd ran; stdout={stdout}"
    );
}

#[test]
fn present_bd_is_invoked() {
    let dir = TestDir::new();
    install_bd(&dir.0, "echo PRIME-RAN \"$@\"\nexit 0\n");

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
    install_bd(&dir.0, "exit 3\n");

    let output = run_with_bin_dir(&dir.0);
    assert_eq!(output.status.code(), Some(3));
}
