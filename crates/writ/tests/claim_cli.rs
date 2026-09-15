use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("writ-claim-cli-{}-{id}", std::process::id()));
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
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

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not a JSON envelope: {error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn writ(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .env("WRIT_WORKTREE_BASE", root.join("worktrees"))
        .env_remove("WRIT_ALLOWED_OWNERS")
        .env_remove("WH_ALLOWED_OWNERS")
        .args(["--json"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn claim_issue_from_url_reports_job_metadata() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let output = writ(
        &root.0,
        &[
            "claim",
            "issue",
            "--repo",
            repo.to_str().unwrap(),
            "--start-point",
            &start,
            "--slug",
            "short",
            "--url",
            "https://github.com/acme/sample/issues/42",
        ],
    );
    let envelope = json(&output);
    assert!(
        output.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "claim.issue");
    assert_eq!(envelope["data"]["job_id"], "gh-42");
    assert_eq!(envelope["data"]["branch"], "hive/issue-42-short");
    assert_eq!(envelope["data"]["issue_number"], 42);
    assert_eq!(envelope["data"]["owns_branch"], true);
    assert_eq!(envelope["data"]["start_commit"], start);
    assert_eq!(
        git(
            Path::new(envelope["data"]["path"].as_str().unwrap()),
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        "hive/issue-42-short"
    );
}

#[test]
fn claim_pr_attaches_existing_head_branch() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["branch", "feature/existing-head", &start]);
    let output = writ(
        &root.0,
        &[
            "claim",
            "pr",
            "--repo",
            repo.to_str().unwrap(),
            "--start-point",
            &start,
            "--head-branch",
            "feature/existing-head",
            "acme",
            "sample",
            "9",
        ],
    );
    let envelope = json(&output);
    assert!(
        output.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(envelope["command"], "claim.pr");
    assert_eq!(envelope["data"]["job_id"], "pr-9");
    assert_eq!(envelope["data"]["branch"], "feature/existing-head");
    assert_eq!(envelope["data"]["owns_branch"], false);
    assert_eq!(envelope["data"]["pr_number"], 9);
}

#[test]
fn second_claim_for_same_issue_fails_clearly() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let args = [
        "claim",
        "issue",
        "--repo",
        repo.to_str().unwrap(),
        "--start-point",
        start.as_str(),
        "acme",
        "sample",
        "7",
    ];
    assert!(writ(&root.0, &args).status.success());
    let output = writ(&root.0, &args);
    let envelope = json(&output);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "claim.issue");
    assert_eq!(envelope["error"]["code"], "WORKTREE_ALREADY_CLAIMED");
}

#[test]
fn claim_does_not_require_a_clean_primary_checkout() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("wip.txt"), "dirty primary\n").unwrap();
    let output = writ(
        &root.0,
        &[
            "claim",
            "issue",
            "--repo",
            repo.to_str().unwrap(),
            "--start-point",
            &start,
            "acme",
            "sample",
            "3",
        ],
    );
    assert!(
        output.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = json(&output)["data"]["path"].as_str().unwrap().to_owned();
    assert!(!Path::new(&path).join("wip.txt").exists());
}
