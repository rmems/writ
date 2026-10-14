mod crash;
mod fix_cycle;
mod inspect;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

pub(super) struct RepoHarness {
    pub temp: tempfile::TempDir,
    pub repo: PathBuf,
    pub store: LeaseStore,
    pub worktree: PathBuf,
    pub start: String,
}

impl RepoHarness {
    pub(super) fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test User"]);
        git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
        let start = git(&repo, &["rev-parse", "HEAD"]);
        let store = LeaseStore::open(temp.path().join("leases.db")).unwrap();
        let worktree = temp.path().join("worktrees/acme/sample/job-1");
        Self {
            temp,
            repo,
            store,
            worktree,
            start,
        }
    }

    pub(super) fn request(&self) -> AllocateRequest<'_> {
        AllocateRequest {
            repo: &self.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            branch: "hive/job-1",
            worktree_path: &self.worktree,
            requested_start_point: "refs/heads/main",
            start_commit: &self.start,
            ttl: None,
        }
    }

    pub(super) fn key(&self) -> JobKey<'_> {
        JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
        }
    }

    pub(super) fn inspect_req(&self) -> InspectRequest<'_> {
        InspectRequest {
            repo_root: &self.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            worktree_path: &self.worktree,
            branch: Some("hive/job-1"),
        }
    }
}

pub(super) fn git(repo: &Path, args: &[&str]) -> String {
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

pub(super) fn later_commit(repo: &Path) -> String {
    git(repo, &["commit", "--allow-empty", "-m", "later"]);
    git(repo, &["rev-parse", "HEAD"])
}

pub(super) fn add_job_worktree(harness: &RepoHarness) {
    git(
        &harness.repo,
        &[
            "worktree",
            "add",
            "-b",
            "hive/job-1",
            "--",
            harness.worktree.to_str().unwrap(),
            &harness.start,
        ],
    );
}

pub(super) fn prepare_and_mutate(harness: &RepoHarness) -> String {
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
    harness.store.mark_mutating(&prepared.operation_id).unwrap();
    prepared.operation_id
}

pub(super) fn activate(harness: &RepoHarness) -> String {
    let operation_id = prepare_and_mutate(harness);
    add_job_worktree(harness);
    harness.store.commit_allocate(&operation_id).unwrap();
    operation_id
}
