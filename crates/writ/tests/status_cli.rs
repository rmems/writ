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

#[test]
fn status_json_reports_two_registered_checkouts_distinctly() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let job_a = root.0.join("job-a");
    let job_b = root.0.join("job-b");
    add_worktree(&repo, &job_a, "hive/a");
    add_worktree(&repo, &job_b, "hive/b");

    let register_a = writ(
        &root.0,
        &[
            "--json",
            "worktree",
            "register",
            job_a.to_str().unwrap(),
            "--job",
            "job-a",
        ],
    );
    let register_b = writ(
        &root.0,
        &[
            "--json",
            "worktree",
            "register",
            job_b.to_str().unwrap(),
            "--job",
            "job-b",
        ],
    );
    assert!(
        register_a.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&register_a.stderr)
    );
    assert!(
        register_b.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&register_b.stderr)
    );

    let output = writ(&root.0, &["--json", "status"]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope = json(&output);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["command"], "cli.status");
    assert_eq!(envelope["data"]["source"], "lease_store");
    let jobs = envelope["data"]["jobs"].as_array().expect("jobs");
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0]["job_id"], "job-a");
    assert_eq!(jobs[1]["job_id"], "job-b");
    assert_ne!(jobs[0]["worktree_path"], jobs[1]["worktree_path"]);
    assert_eq!(jobs[0]["collaboration_state"], "running");
    assert_eq!(jobs[0]["lease_mode"], "WRITER_LOCKED");
    assert_eq!(jobs[0]["process_state"], "unknown");
    assert_eq!(jobs[0]["ci_class"], "unknown");
    assert!(jobs[0]["ownership_generation"].is_null());
    assert!(jobs[0]["declared_paths"].is_null());
    assert!(jobs[0]["blocker"].is_null());
    assert!(jobs[0]["handoff"].is_null());
    assert_eq!(jobs[0]["head_source"], "checkout");
    assert!(jobs[0]["head"].as_str().is_some_and(|h| !h.is_empty()));
    assert_eq!(jobs[0]["recovery_needed"], false);

    let human = writ(&root.0, &["status"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("job-a"));
    assert!(text.contains("job-b"));
    assert!(text.contains("running"));
    assert!(!text.contains("completed"));

    let unregister = writ(
        &root.0,
        &["--json", "worktree", "unregister", job_a.to_str().unwrap()],
    );
    assert!(unregister.status.success());
    let after = json(&writ(&root.0, &["--json", "jobs"]));
    let jobs = after["data"]["jobs"].as_array().expect("jobs");
    let released = jobs
        .iter()
        .find(|job| job["job_id"] == "job-a")
        .expect("released job-a");
    assert_eq!(released["collaboration_state"], "unassigned");
    assert_eq!(released["lease_mode"], "UNASSIGNED");
    assert_ne!(released["collaboration_state"], "completed");
    let live = jobs
        .iter()
        .find(|job| job["job_id"] == "job-b")
        .expect("live job-b");
    assert_eq!(live["collaboration_state"], "running");
}

#[test]
fn empty_lease_store_is_ok_with_empty_participants() {
    let root = TestDir::new();
    let output = writ(&root.0, &["--json", "status"]);
    assert!(output.status.success());
    let envelope = json(&output);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["source"], "lease_store");
    assert_eq!(envelope["data"]["jobs"].as_array().unwrap().len(), 0);
    assert_eq!(envelope["data"]["agents"].as_array().unwrap().len(), 0);

    let human = writ(&root.0, &["status"]);
    assert_eq!(
        String::from_utf8_lossy(&human.stdout),
        "No collaboration participants.\n"
    );
}
