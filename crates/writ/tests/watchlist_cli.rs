use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use writ_core::lease::{LeaseGrant, LeaseStore};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("writ-watchlist-cli-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn writ(root: &TestDir, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .env("WRIT_LEASE_PATH", root.0.join("leases.db"))
        .env("WRIT_ALLOWED_OWNERS", "acme")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn seed_lease(root: &TestDir) {
    let store = LeaseStore::open(root.0.join("leases.db")).unwrap();
    let wt = root.0.join("checkout");
    fs::create_dir_all(&wt).unwrap();
    store
        .grant(LeaseGrant {
            repo: &root.0.join("repo"),
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            branch: "hive/job-1",
            worktree_path: &wt,
            start_commit: "abc123",
        })
        .unwrap();
}

#[test]
fn list_json_reads_lease_store() {
    let root = TestDir::new();
    seed_lease(&root);
    let (code, stdout, stderr) = writ(&root, &["--json", "watchlist", "list"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("cli.watchlist.list"));
    assert!(stdout.contains("job-1"));
    assert!(stdout.contains("running"));
    assert!(!stdout.contains("watchlist.json"));
}

#[test]
fn add_does_not_create_json_store() {
    let root = TestDir::new();
    let (code, stdout, _stderr) = writ(&root, &["--json", "watchlist", "add"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"persisted\":false"));
    assert!(!root.0.join("watchlist.json").exists());
}

#[test]
fn empty_list_is_ok() {
    let root = TestDir::new();
    let (code, stdout, stderr) = writ(&root, &["watchlist", "list"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("writ worktree register"));
}
