//! Shared helpers for `writ` CLI integration tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub struct TestDir {
    pub path: PathBuf,
}

impl TestDir {
    pub fn new(label: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("writ-{label}-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

pub fn init_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "trunk"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    repo
}

pub fn add_origin(repo: &Path) {
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/sample.git",
        ],
    );
}

pub fn writ(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .env("WRIT_WORKTREE_BASE", root.join("worktrees"))
        .env("WRIT_LEASE_PATH", root.join("leases.db"))
        .args(args)
        .output()
        .unwrap()
}

pub fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not a JSON envelope: {error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
