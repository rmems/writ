mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::thread;

use common::{TestDir, add_origin, git, init_repo, json, writ};

struct JobSpec<'a> {
    job: &'a str,
    branch: &'a str,
}

fn create_job(root: &Path, repo: &Path, spec: JobSpec<'_>) -> PathBuf {
    let start = git(repo, &["rev-parse", "HEAD"]);
    let path = root.join("checkouts").join(spec.job);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            spec.branch,
            "--",
            path.to_str().unwrap(),
            &start,
        ],
    );
    let registered = writ(
        root,
        &[
            "--json",
            "worktree",
            "register",
            path.to_str().unwrap(),
            "--job",
            spec.job,
        ],
    );
    assert!(registered.status.success(), "{:?}", registered.stderr);
    path
}

struct AnnounceSpec<'a> {
    job: &'a str,
    agent: &'a str,
    session: &'a str,
    intent: &'a str,
    path: &'a str,
}

fn announce(root: &Path, spec: AnnounceSpec<'_>) -> Output {
    writ(
        root,
        &[
            "--json",
            "coord",
            "announce",
            "acme",
            "sample",
            spec.job,
            "--agent",
            spec.agent,
            "--session",
            spec.session,
            "--intent",
            spec.intent,
            "--path",
            spec.path,
        ],
    )
}

fn assert_advisory_overlap(first: &Output, second: &Output) {
    assert!(first.status.success(), "{:?}", first.stderr);
    assert!(second.status.success(), "{:?}", second.stderr);
    let overlaps = json(first)["data"]["overlaps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .chain(
            json(second)["data"]["overlaps"]
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
}

fn assert_cross_process_visibility(root: &Path) {
    let listed = thread::spawn({
        let root = root.to_path_buf();
        move || writ(&root, &["--json", "coord", "list"])
    });
    let inbox_job = thread::spawn({
        let root = root.to_path_buf();
        move || {
            writ(
                &root,
                &["--json", "coord", "inbox", "acme", "sample", "job-a"],
            )
        }
    });
    let listed = listed.join().unwrap();
    let inbox = inbox_job.join().unwrap();
    assert!(listed.status.success(), "{:?}", listed.stderr);
    assert_eq!(json(&listed)["data"]["claims"].as_array().unwrap().len(), 2);
    assert!(inbox.status.success(), "{:?}", inbox.stderr);
    let inbox_json = json(&inbox);
    let messages = inbox_json["data"]["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message["kind"] == "overlap" || message["kind"] == "intent")
    );
}

fn pause_job(root: &Path) {
    let paused = writ(
        root,
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
    let seize = writ(
        root,
        &[
            "--json", "coord", "announce", "acme", "sample", "job-a", "--agent", "agent-b",
            "--intent", "seize",
        ],
    );
    assert_eq!(json(&seize)["error"]["code"], "COORD_CLAIM_HELD");
}

fn ack_handoff(root: &Path) {
    let handoff = writ(
        root,
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
    let ack = writ(
        root,
        &[
            "--json",
            "coord",
            "ack",
            "acme",
            "sample",
            "job-a",
            "--id",
            &json(&handoff)["data"]["id"].as_i64().unwrap().to_string(),
            "--agent",
            "agent-b",
            "--session",
            "sess-b",
        ],
    );
    let transferred = json(&ack);
    assert_eq!(
        (
            transferred["data"]["claim"]["agent_id"].as_str(),
            transferred["data"]["claim"]["owner_generation"].as_i64(),
            transferred["data"]["claim"]["paused_at"].is_null(),
        ),
        (Some("agent-b"), Some(2), true)
    );
}

fn transfer_paused_job(root: &Path) {
    pause_job(root);
    ack_handoff(root);
}

#[test]
fn two_processes_share_store_and_exchange_overlap_help_handoff() {
    let root = TestDir::new("coord-cli");
    let repo = init_repo(&root.path);
    add_origin(&repo);
    let path_a = create_job(
        &root.path,
        &repo,
        JobSpec {
            job: "job-a",
            branch: "hive/job-a",
        },
    );
    let path_b = create_job(
        &root.path,
        &repo,
        JobSpec {
            job: "job-b",
            branch: "hive/job-b",
        },
    );
    fs::write(path_a.join("wip.txt"), "do not delete").unwrap();
    assert_advisory_overlap(
        &announce(
            &root.path,
            AnnounceSpec {
                job: "job-a",
                agent: "agent-a",
                session: "sess-a",
                intent: "own coord.rs",
                path: "crates/writ-core/src/coord.rs",
            },
        ),
        &announce(
            &root.path,
            AnnounceSpec {
                job: "job-b",
                agent: "agent-b",
                session: "sess-b",
                intent: "own coord module",
                path: "crates/writ-core/src",
            },
        ),
    );
    assert_cross_process_visibility(&root.path);
    transfer_paused_job(&root.path);
    assert_eq!(
        fs::read_to_string(path_a.join("wip.txt")).unwrap(),
        "do not delete"
    );
    assert!(path_b.exists());
}

#[test]
fn two_processes_can_list_the_same_claims() {
    let root = TestDir::new("coord-cli");
    let repo = init_repo(&root.path);
    add_origin(&repo);
    create_job(
        &root.path,
        &repo,
        JobSpec {
            job: "job-a",
            branch: "hive/job-a",
        },
    );
    create_job(
        &root.path,
        &repo,
        JobSpec {
            job: "job-b",
            branch: "hive/job-b",
        },
    );
    let announced = writ(
        &root.path,
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
    let listed = writ(&root.path, &["--json", "coord", "list"]);
    assert!(listed.status.success(), "{:?}", listed.stderr);
    assert_eq!(json(&listed)["data"]["claims"].as_array().unwrap().len(), 1);

    let human = writ(&root.path, &["coord", "list"]);
    assert!(human.status.success(), "{:?}", human.stderr);
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("job-a"), "{stdout}");
}

#[test]
fn coord_list_denies_empty_allowlist() {
    let root = TestDir::new("coord-cli");
    let denied = std::process::Command::new(env!("CARGO_BIN_EXE_writ"))
        .env("WRIT_WORKTREE_BASE", root.path.join("worktrees"))
        .env("WRIT_LEASE_PATH", root.path.join("leases.db"))
        .env_remove("WRIT_ALLOWED_OWNERS")
        .env_remove("WH_ALLOWED_OWNERS")
        .args(["--json", "coord", "list"])
        .output()
        .unwrap();
    assert!(!denied.status.success());
}
