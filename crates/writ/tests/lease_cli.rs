mod common;

use std::fs;
use std::path::Path;
use std::process::Output;

use common::{TestDir, add_origin, git, init_repo, json, writ};

fn add_worktree(repo: &Path, path: &Path, branch: &str) {
    let start = git(repo, &["rev-parse", "HEAD"]);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            "--",
            path.to_str().unwrap(),
            &start,
        ],
    );
}

fn register_job(root: &Path, path: &Path, job: &str) -> Output {
    writ(
        root,
        &[
            "--json",
            "worktree",
            "register",
            path.to_str().unwrap(),
            "--job",
            job,
        ],
    )
}

fn inspect_job(root: &Path, repo: &Path, branch: &str, job: &str) -> Output {
    writ(
        root,
        &[
            "--json",
            "lease",
            "inspect",
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            branch,
            "acme",
            "sample",
            job,
        ],
    )
}

#[test]
fn inspect_reports_identity_without_mutating_after_register() {
    let root = TestDir::new("lease-cli");
    let repo = init_repo(&root.path);
    add_origin(&repo);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let path = root.path.join("checkouts/job-inspect");
    add_worktree(&repo, &path, "hive/job-inspect");
    assert!(
        register_job(&root.path, &path, "job-inspect")
            .status
            .success()
    );

    let inspected = inspect_job(&root.path, &repo, "hive/job-inspect", "job-inspect");
    let envelope = json(&inspected);
    assert!(inspected.status.success());
    assert_eq!(
        (
            envelope["command"].as_str(),
            envelope["data"]["requested_start_point"].as_str(),
            envelope["data"]["resolved_start_commit"].as_str(),
            envelope["data"]["classification"].as_str(),
            envelope["data"]["lease_present"].as_bool(),
            envelope["data"]["worktree_registered"].as_bool(),
        ),
        (
            Some("lease.inspect"),
            Some("refs/heads/hive/job-inspect"),
            Some(start.as_str()),
            Some("matching"),
            Some(true),
            Some(true),
        )
    );
    assert_eq!(
        git(&repo, &["rev-parse", "refs/heads/hive/job-inspect"]),
        start
    );

    let reconciled = writ(
        &root.path,
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
    assert_eq!(json(&reconciled)["data"]["outcome"], "already_active");
}

#[test]
fn inspect_distinguishes_missing_lease_from_live_worktree() {
    let root = TestDir::new("lease-cli");
    let repo = init_repo(&root.path);
    let path = root.path.join("worktrees/acme/sample/foreign");
    add_worktree(&repo, &path, "hive/foreign");

    let inspected = writ(
        &root.path,
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
    assert_eq!(
        (
            envelope["data"]["classification"].as_str(),
            envelope["data"]["lease_present"].as_bool(),
            envelope["data"]["path_exists"].as_bool(),
        ),
        (Some("missing_lease"), Some(false), Some(true))
    );
    assert!(path.exists());
}
