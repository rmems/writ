use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("writ-status-cli-{}-{id}", std::process::id()));
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
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/sample.git",
        ],
    );
    git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    repo
}

fn writ(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .env("WRIT_LEASE_PATH", root.join("leases.db"))
        .env("WRIT_WORKTREE_BASE", root.join("worktrees"))
        .env("WRIT_ALLOWED_OWNERS", "acme")
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON: {error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn add_worktree(repo: &Path, path: &Path, branch: &str) {
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().unwrap(),
            "trunk",
        ],
    );
}

fn register(root: &Path, path: &Path, job: &str) {
    let output = writ(
        root,
        &[
            "--json",
            "worktree",
            "register",
            path.to_str().unwrap(),
            "--job",
            job,
        ],
    );
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn two_registered() -> (TestDir, PathBuf, PathBuf) {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let job_a = root.0.join("job-a");
    let job_b = root.0.join("job-b");
    add_worktree(&repo, &job_a, "hive/a");
    add_worktree(&repo, &job_b, "hive/b");
    register(&root.0, &job_a, "job-a");
    register(&root.0, &job_b, "job-b");
    (root, job_a, job_b)
}

fn status_envelope(root: &Path) -> serde_json::Value {
    let output = writ(root, &["--json", "status"]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    json(&output)
}

fn assert_ok_envelope(envelope: &serde_json::Value, command: &str) {
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["schema_version"], 2);
    assert_eq!(envelope["command"], command);
}

fn jobs_array(envelope: &serde_json::Value) -> &[serde_json::Value] {
    envelope["data"]["jobs"].as_array().expect("jobs")
}

fn assert_two_job_ids(jobs: &[serde_json::Value]) {
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0]["job_id"], "job-a");
    assert_eq!(jobs[1]["job_id"], "job-b");
}

fn assert_running_lease(job: &serde_json::Value) {
    assert_eq!(job["collaboration_state"], "running");
    assert_eq!(job["lease_mode"], "WRITER_LOCKED");
    assert_eq!(job["process_state"], "unknown");
}

fn assert_unknown_ci_and_paths(job: &serde_json::Value) {
    assert_eq!(job["ci_class"], "unknown");
    assert!(job["ownership_generation"].is_null());
    assert!(job["declared_paths"].is_null());
}

fn assert_unknown_handoff_fields(job: &serde_json::Value) {
    assert!(job["blocker"].is_null());
    assert!(job["handoff"].is_null());
}

fn assert_live_head(job: &serde_json::Value) {
    assert_eq!(job["head_source"], "checkout");
    assert!(job["head"].as_str().is_some_and(|h| !h.is_empty()));
    assert_eq!(job["recovery_needed"], false);
}

fn job_named<'a>(jobs: &'a [serde_json::Value], id: &str) -> &'a serde_json::Value {
    jobs.iter()
        .find(|job| job["job_id"] == id)
        .unwrap_or_else(|| panic!("missing {id}"))
}

#[test]
fn status_json_reports_two_registered_checkouts_distinctly() {
    let (root, _, _) = two_registered();
    let envelope = status_envelope(&root.0);
    assert_ok_envelope(&envelope, "cli.status");
    assert_eq!(envelope["data"]["source"], "lease_store");
    let jobs = jobs_array(&envelope);
    assert_two_job_ids(jobs);
    assert_ne!(jobs[0]["worktree_path"], jobs[1]["worktree_path"]);
    assert_running_lease(&jobs[0]);
    assert_unknown_ci_and_paths(&jobs[0]);
    assert_unknown_handoff_fields(&jobs[0]);
    assert_live_head(&jobs[0]);
}

#[test]
fn human_status_lists_both_participants_without_github_completion() {
    let (root, _, _) = two_registered();
    let human = writ(&root.0, &["status"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("job-a"));
    assert!(text.contains("job-b"));
    assert!(!text.contains("completed"));
}

#[test]
fn unregister_keeps_unassigned_distinct_from_completed() {
    let (root, job_a, _) = two_registered();
    let unregister = writ(
        &root.0,
        &["--json", "worktree", "unregister", job_a.to_str().unwrap()],
    );
    assert!(unregister.status.success());
    let after = json(&writ(&root.0, &["--json", "jobs"]));
    let jobs = jobs_array(&after);
    let released = job_named(jobs, "job-a");
    assert_eq!(released["collaboration_state"], "unassigned");
    assert_eq!(released["lease_mode"], "UNASSIGNED");
    assert_eq!(job_named(jobs, "job-b")["collaboration_state"], "running");
}

#[test]
fn empty_lease_store_json_is_ok() {
    let root = TestDir::new();
    let envelope = json(&writ(&root.0, &["--json", "status"]));
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["source"], "lease_store");
    assert_eq!(envelope["data"]["jobs"].as_array().unwrap().len(), 0);
}

#[test]
fn empty_lease_store_human_is_not_a_watchlist() {
    let root = TestDir::new();
    let human = writ(&root.0, &["status"]);
    assert_eq!(
        String::from_utf8_lossy(&human.stdout),
        "No collaboration participants.\n"
    );
}
