use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("writ-lease-cli-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
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

fn init_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "trunk"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    repo
}

fn writ(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .env("WRIT_WORKTREE_BASE", root.join("worktrees"))
        .env("WRIT_LEASE_PATH", root.join("leases.db"))
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not a JSON envelope: {error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn inspect_reports_identity_without_mutating_after_register() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/sample.git",
        ],
    );
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let path = root.0.join("checkouts/job-inspect");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "hive/job-inspect",
            "--",
            path.to_str().unwrap(),
            &start,
        ],
    );
    let registered = writ(
        &root.0,
        &[
            "--json",
            "worktree",
            "register",
            path.to_str().unwrap(),
            "--job",
            "job-inspect",
        ],
    );
    assert!(registered.status.success(), "{:?}", registered.stderr);

    let inspected = writ(
        &root.0,
        &[
            "--json",
            "lease",
            "inspect",
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "hive/job-inspect",
            "acme",
            "sample",
            "job-inspect",
        ],
    );
    let envelope = json(&inspected);
    assert!(inspected.status.success());
    assert_eq!(envelope["command"], "lease.inspect");
    assert_eq!(
        envelope["data"]["requested_start_point"],
        "refs/heads/hive/job-inspect"
    );
    assert_eq!(envelope["data"]["resolved_start_commit"], start);
    assert_eq!(envelope["data"]["classification"], "matching");
    assert_eq!(envelope["data"]["lease_present"], true);
    assert_eq!(envelope["data"]["worktree_registered"], true);
    assert_ne!(
        envelope["data"]["requested_start_point"],
        envelope["data"]["resolved_start_commit"]
    );
    assert_eq!(
        git(&repo, &["rev-parse", "refs/heads/hive/job-inspect"]),
        start
    );

    let reconciled = writ(
        &root.0,
        &[
            "--json",
            "lease",
            "reconcile",
            "--repo",
            repo.to_str().unwrap(),
            "acme",
            "sample",
            "job-inspect",
        ],
    );
    let outcome = json(&reconciled);
    assert!(reconciled.status.success());
    assert_eq!(outcome["data"]["outcome"], "already_active");
}

#[test]
fn inspect_distinguishes_missing_lease_from_live_worktree() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let path = root.0.join("worktrees/acme/sample/foreign");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "hive/foreign",
            "--",
            path.to_str().unwrap(),
            &start,
        ],
    );

    let inspected = writ(
        &root.0,
        &[
            "--json",
            "lease",
            "inspect",
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "hive/foreign",
            "--path",
            path.to_str().unwrap(),
            "acme",
            "sample",
            "foreign",
        ],
    );
    let envelope = json(&inspected);
    assert!(inspected.status.success());
    assert_eq!(envelope["data"]["classification"], "missing_lease");
    assert_eq!(envelope["data"]["lease_present"], false);
    assert_eq!(envelope["data"]["path_exists"], true);
    assert!(path.exists());
}
