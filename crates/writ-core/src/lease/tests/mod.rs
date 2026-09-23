use super::*;
use crate::error::{Error, PolicyCode};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

mod crash;
mod cycles;
mod grant;

struct RepoHarness {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    store: LeaseStore,
    worktree: PathBuf,
    start: String,
}

impl RepoHarness {
    fn new() -> Self {
        let temp = tempdir().unwrap();
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
            _temp: temp,
            repo,
            store,
            worktree,
            start,
        }
    }

    fn request(&self) -> AllocateRequest<'_> {
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

    fn key(&self) -> JobKey<'_> {
        JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
        }
    }

    fn inspect_req(&self) -> InspectRequest<'_> {
        InspectRequest {
            repo_root: &self.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            worktree_path: &self.worktree,
            branch: Some("hive/job-1"),
        }
    }

    fn prepare_and_add_worktree(&self) -> Lease {
        let prepared = self.store.prepare_allocate(self.request()).unwrap();
        self.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                self.worktree.to_str().unwrap(),
                &self.start,
            ],
        );
        prepared
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

fn later_commit(repo: &Path) -> String {
    git(repo, &["commit", "--allow-empty", "-m", "later"]);
    git(repo, &["rev-parse", "HEAD"])
}

fn assert_policy(err: Error, code: PolicyCode) {
    match err {
        Error::PolicyViolation { code: got, .. } if got == code => {}
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

fn assert_outcome(
    store: &LeaseStore,
    key: JobKey<'_>,
    repo: &Path,
    expected: &str,
) -> ReconcileOutcome {
    let outcome = store.reconcile(key, repo).unwrap().unwrap();
    assert_eq!(outcome.as_str(), expected);
    outcome
}
