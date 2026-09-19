use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("writ-coord-cli-{}-{id}", std::process::id()));
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

fn create_job(root: &Path, repo: &Path, job_id: &str, branch: &str) -> PathBuf {
    let created = writ(
        root,
        &[
            "--json",
            "worktree",
            "create",
            "--schema-version",
            "2",
            "--repo",
            repo.to_str().unwrap(),
            "--start-point",
            "refs/heads/trunk",
            "acme",
            "sample",
            job_id,
            branch,
        ],
    );
    assert!(created.status.success(), "{:?}", created.stderr);
    let envelope = json(&created);
    PathBuf::from(envelope["data"]["path"].as_str().unwrap())
}

#[test]
fn two_processes_share_store_and_exchange_overlap_help_handoff() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let path_a = create_job(&root.0, &repo, "job-a", "hive/job-a");
    let path_b = create_job(&root.0, &repo, "job-b", "hive/job-b");
    let wip = path_a.join("wip.txt");
    fs::write(&wip, "do not delete").unwrap();

    let announce_a = thread::spawn({
        let root = root.0.clone();
        move || {
            writ(
                &root,
                &[
                    "--json",
                    "coord",
                    "announce",
                    "acme",
                    "sample",
                    "job-a",
                    "--agent",
                    "agent-a",
                    "--session",
                    "sess-a",
                    "--intent",
                    "own coord.rs",
                    "--path",
                    "crates/writ-core/src/coord.rs",
                ],
            )
        }
    });
    let announce_b = thread::spawn({
        let root = root.0.clone();
        move || {
            writ(
                &root,
                &[
                    "--json",
                    "coord",
                    "announce",
                    "acme",
                    "sample",
                    "job-b",
                    "--agent",
                    "agent-b",
                    "--session",
                    "sess-b",
                    "--intent",
                    "own coord module",
                    "--path",
                    "crates/writ-core/src",
                ],
            )
        }
    });
    let first = announce_a.join().unwrap();
    let second = announce_b.join().unwrap();
    assert!(first.status.success(), "{:?}", first.stderr);
    assert!(second.status.success(), "{:?}", second.stderr);
    let overlaps = json(&first)["data"]["overlaps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .chain(
            json(&second)["data"]["overlaps"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        )
        .collect::<Vec<_>>();
    assert!(
        overlaps.iter().any(|overlap| overlap["advisory"] == true
            && (overlap["job_id"] == "job-a" || overlap["job_id"] == "job-b")),
        "expected advisory overlap, got {overlaps:?}"
    );

    let inbox = writ(
        &root.0,
        &["--json", "coord", "inbox", "acme", "sample", "job-a"],
    );
    assert!(inbox.status.success(), "{:?}", inbox.stderr);
    let messages = json(&inbox)["data"]["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message["kind"] == "overlap" || message["kind"] == "intent")
    );

    let paused = writ(
        &root.0,
        &[
            "--json",
            "coord",
            "pause",
            "acme",
            "sample",
            "job-a",
            "--agent",
            "agent-a",
            "--body",
            "owner stalled",
        ],
    );
    assert!(paused.status.success(), "{:?}", paused.stderr);
    assert_eq!(json(&paused)["data"]["help"]["kind"], "help");
    assert!(json(&paused)["data"]["claim"]["paused_at"].is_number());

    let seize = writ(
        &root.0,
        &[
            "--json", "coord", "announce", "acme", "sample", "job-a", "--agent", "agent-b",
            "--intent", "seize",
        ],
    );
    assert!(!seize.status.success());
    assert_eq!(json(&seize)["error"]["code"], "COORD_CLAIM_HELD");

    let handoff = writ(
        &root.0,
        &[
            "--json",
            "coord",
            "handoff",
            "acme",
            "sample",
            "job-a",
            "--agent",
            "agent-a",
            "--to-agent",
            "agent-b",
            "--generation",
            "1",
            "--body",
            "transfer paused assignment",
        ],
    );
    assert!(handoff.status.success(), "{:?}", handoff.stderr);
    let handoff_id = json(&handoff)["data"]["id"].as_i64().unwrap();

    let ack = writ(
        &root.0,
        &[
            "--json",
            "coord",
            "ack",
            "acme",
            "sample",
            "job-a",
            "--id",
            &handoff_id.to_string(),
            "--agent",
            "agent-b",
            "--session",
            "sess-b",
        ],
    );
    assert!(ack.status.success(), "{:?}", ack.stderr);
    let transferred = json(&ack);
    assert_eq!(transferred["data"]["claim"]["agent_id"], "agent-b");
    assert_eq!(transferred["data"]["claim"]["owner_generation"], 2);
    assert!(transferred["data"]["claim"]["paused_at"].is_null());
    assert_eq!(fs::read_to_string(&wip).unwrap(), "do not delete");
    assert!(path_b.exists());
}

#[test]
fn two_processes_can_list_the_same_claims() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    create_job(&root.0, &repo, "job-a", "hive/job-a");
    create_job(&root.0, &repo, "job-b", "hive/job-b");
    let announced = writ(
        &root.0,
        &[
            "--json",
            "coord",
            "announce",
            "acme",
            "sample",
            "job-a",
            "--agent",
            "agent-a",
            "--path",
            "README.md",
        ],
    );
    assert!(announced.status.success(), "{:?}", announced.stderr);
    let listed = writ(&root.0, &["--json", "coord", "list"]);
    assert!(listed.status.success(), "{:?}", listed.stderr);
    assert_eq!(json(&listed)["data"]["claims"].as_array().unwrap().len(), 1);
}
