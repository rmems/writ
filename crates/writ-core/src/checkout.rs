//! Registration of harness-owned checkouts.
//!
//! Writ no longer manages worktree creation, placement, or deletion. An agent
//! host (Cursor, Claude Code, Codex, plain `git worktree add`, or a standalone
//! clone) creates the checkout; writ registers it so the shared lease store can
//! coordinate intent and ownership. Registration is coordination-state only: it
//! never creates, moves, renames, fetches, resets, or deletes anything.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::error::{Error, PolicyCode, Result};
use crate::git_cmd::git_in;
use crate::lease::{Lease, LeaseGrant, LeaseStore};

/// Sentinel branch value recorded for a checkout on a detached HEAD.
///
/// The record also carries `head_commit` and the inspection result's
/// `detached` state, so a real branch cannot be confused with this sentinel.
pub const DETACHED_BRANCH: &str = "(detached)";

/// Observed state of an existing checkout. Purely descriptive: populated by
/// [`inspect_checkout`], which performs no mutation and no store writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutInfo {
    /// Canonical top-level of the checkout.
    pub path: PathBuf,
    /// `--git-common-dir` resolved to an absolute path. For a standalone clone
    /// this is its own `.git`; it is not claimed to be shared with any other
    /// host's checkout.
    pub common_dir: PathBuf,
    /// True when the checkout is a linked worktree (its git dir sits under
    /// another repository's `worktrees/` admin dir). False for a primary
    /// checkout or a standalone clone.
    pub linked_worktree: bool,
    /// Short branch name, or `None` on a detached HEAD.
    pub branch: Option<String>,
    /// Commit `HEAD` resolves to; `None` on an unborn branch.
    pub head_commit: Option<String>,
    /// GitHub `owner/repo` slug when `origin` parses as one.
    pub origin_slug: Option<String>,
    /// Owner segment derived from `origin_slug`, else `local`.
    pub owner: String,
    /// Repo segment derived from `origin_slug`, else the directory name.
    pub repo_name: String,
    /// True when `git status --porcelain` reports any change (including
    /// untracked files).
    pub dirty: bool,
}

/// Registry over the shared lease store for already-created checkouts.
#[derive(Debug)]
pub struct CheckoutRegistry {
    leases: LeaseStore,
}

impl CheckoutRegistry {
    /// Open a registry on the default lease store path.
    pub fn new() -> Result<Self> {
        Self::with_store(LeaseStore::open(crate::paths::lease_store_path())?)
    }

    /// Open a registry on an explicit store (tests inject a temp path).
    pub fn with_store(leases: LeaseStore) -> Result<Self> {
        Ok(Self { leases })
    }

    /// Borrow the underlying lease store.
    #[must_use]
    pub fn lease_store(&self) -> &LeaseStore {
        &self.leases
    }

    /// Register an existing checkout for coordination.
    ///
    /// Writes or refreshes the lease row keyed on `(owner, repo_name, job_id)`
    /// and returns the observed state. Nothing under `path` is created,
    /// modified, or deleted; the checkout may live anywhere, may share a git
    /// common dir with other worktrees, or may be a standalone clone.
    pub fn register(&self, path: &Path, job_id: &str) -> Result<CheckoutInfo> {
        let info = inspect_checkout(path)?;
        self.leases.grant(LeaseGrant {
            repo: &info.common_dir,
            owner: &info.owner,
            repo_name: &info.repo_name,
            job_id,
            branch: info.branch.as_deref().unwrap_or(DETACHED_BRANCH),
            worktree_path: &info.path,
            start_commit: info.head_commit.as_deref().unwrap_or(""),
        })?;
        Ok(info)
    }

    /// Release the coordination record for `path` without touching the
    /// checkout itself. Returns the released lease when one was held.
    ///
    /// Only a row whose recorded `worktree_path` matches `path` is released, so
    /// stale state for a different path cannot steal a live registration.
    pub fn unregister(&self, path: &Path) -> Result<Option<Lease>> {
        let canonical = canonicalize(path)?;
        self.leases.release_by_path(&canonical)
    }

    /// Currently held (unreleased) registrations.
    pub fn registered(&self) -> Result<Vec<Lease>> {
        self.leases.list_active()
    }
}

/// Read-only inspection of an existing checkout.
///
/// Resolves the git top-level and common dir, the real `HEAD` and branch (or
/// explicit detached state), the repository identity, and dirty state. Never
/// writes to the lease store or the working tree.
pub fn inspect_checkout(path: &Path) -> Result<CheckoutInfo> {
    let toplevel = git_checked(
        path,
        &["rev-parse", "--show-toplevel"],
        TopLevelFailure::NotACheckout,
    )?;
    let path = canonicalize(Path::new(toplevel.trim()))?;

    let git_dir = git_checked(&path, &["rev-parse", "--git-dir"], TopLevelFailure::Git)?;
    let common_dir = git_checked(
        &path,
        &["rev-parse", "--git-common-dir"],
        TopLevelFailure::Git,
    )?;
    let git_dir = absolutize(&path, git_dir.trim());
    let common_dir = absolutize(&path, common_dir.trim());
    // A linked worktree's git dir lives under `<common>/worktrees/<name>`; a
    // primary checkout or standalone clone has git dir == common dir.
    let linked_worktree = git_dir != common_dir;

    let branch = git_optional(&path, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let head_commit = git_optional(
        &path,
        &["rev-parse", "--verify", "--end-of-options", "HEAD"],
    )
    .map(|value| value.trim().to_owned())
    .filter(|value| !value.is_empty());
    let dirty = git_optional(&path, &["status", "--porcelain"])
        .is_some_and(|status| !status.trim().is_empty());

    let origin_slug = crate::git_safe::origin_github_slug(&path).ok();
    let (owner, repo_name) = match origin_slug.as_deref().and_then(|s| s.split_once('/')) {
        Some((owner, repo)) => (owner.to_owned(), repo.to_owned()),
        None => (
            "local".to_owned(),
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "checkout".to_owned()),
        ),
    };

    Ok(CheckoutInfo {
        path,
        common_dir,
        linked_worktree,
        branch,
        head_commit,
        origin_slug,
        owner,
        repo_name,
        dirty,
    })
}

fn canonicalize(path: &Path) -> Result<PathBuf> {
    crate::paths::canonicalize_for_tools(path).map_err(|e| Error::Io {
        context: "canonicalize checkout path",
        source: e,
    })
}

fn absolutize(base: &Path, candidate: &str) -> PathBuf {
    let candidate = Path::new(candidate);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    crate::paths::canonicalize_for_tools(&joined).unwrap_or(joined)
}

enum TopLevelFailure {
    NotACheckout,
    Git,
}

fn git_checked(path: &Path, args: &[&str], failure: TopLevelFailure) -> Result<String> {
    let output = git_in(path, args).map_err(|e| Error::Io {
        context: "inspect checkout",
        source: e,
    })?;
    match (output.status.success(), failure) {
        (true, _) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        (false, TopLevelFailure::NotACheckout) => Err(Error::PolicyViolation {
            code: PolicyCode::GitDirUnavailable,
            message: format!(
                "not a git checkout: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }),
        (false, TopLevelFailure::Git) => Err(git_command_error(args, &output)),
    }
}

fn git_optional(path: &Path, args: &[&str]) -> Option<String> {
    let output = git_in(path, args).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_command_error(args: &[&str], output: &Output) -> Error {
    Error::GitCommand {
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn head(dir: &Path) -> String {
        String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned()
    }

    fn init_repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("origin-repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "test"]);
        std::fs::write(repo.join("file.txt"), "base\n").unwrap();
        git(&repo, &["add", "file.txt"]);
        git(&repo, &["commit", "--quiet", "-m", "base"]);
        (tmp, repo)
    }

    #[test]
    fn inspect_primary_checkout_reports_branch_and_clean_state() {
        let (_tmp, repo) = init_repo();
        let info = inspect_checkout(&repo).unwrap();
        assert_eq!(info.path, canonicalize(&repo).unwrap());
        assert!(!info.linked_worktree);
        assert!(info.branch.is_some());
        assert!(info.head_commit.is_some());
        assert!(!info.dirty);
    }

    #[test]
    fn inspect_linked_worktree_and_dirty_state() {
        let (_tmp, repo) = init_repo();
        let wt = _tmp.path().join("custom-wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/x",
                wt.to_str().unwrap(),
            ],
        );
        std::fs::write(wt.join("wip.txt"), "uncommitted\n").unwrap();

        let info = inspect_checkout(&wt).unwrap();
        assert!(info.linked_worktree);
        assert_eq!(info.branch.as_deref(), Some("feature/x"));
        assert!(info.dirty);
        assert_ne!(info.common_dir, info.path.join(".git"));
    }

    #[test]
    fn inspect_detached_head_records_explicit_detached_state() {
        let (_tmp, repo) = init_repo();
        let expected = head(&repo);
        git(&repo, &["checkout", "--quiet", "--detach", &expected]);
        let info = inspect_checkout(&repo).unwrap();
        assert!(info.branch.is_none());
        assert_eq!(info.head_commit.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn register_unregister_round_trip_leaves_files_and_branch_untouched() {
        let (tmp, repo) = init_repo();
        let wt = tmp.path().join("elsewhere/wt-a");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "job/a",
                wt.to_str().unwrap(),
            ],
        );
        std::fs::write(wt.join("wip.txt"), "keep me\n").unwrap();

        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();
        let info = registry.register(&wt, "job-a").unwrap();
        assert_eq!(info.branch.as_deref(), Some("job/a"));

        let lease = registry.unregister(&wt).unwrap().unwrap();
        assert_eq!(lease.mode, crate::lease::LeaseMode::Unassigned);
        assert!(
            wt.join("wip.txt").exists(),
            "unregister must not delete WIP"
        );
        git(
            &repo,
            &["show-ref", "--verify", "--quiet", "refs/heads/job/a"],
        );
    }

    #[test]
    fn two_independent_checkouts_share_one_store() {
        let (tmp, repo) = init_repo();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();

        let wt1 = tmp.path().join("anywhere/one");
        let wt2 = tmp.path().join("somewhere-else/two");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "job/one",
                wt1.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "job/two",
                wt2.to_str().unwrap(),
            ],
        );

        registry.register(&wt1, "job-one").unwrap();
        registry.register(&wt2, "job-two").unwrap();
        assert_eq!(registry.registered().unwrap().len(), 2);
    }

    #[test]
    fn standalone_clone_registers_without_shared_git_dir() {
        let (tmp, repo) = init_repo();
        let clone = tmp.path().join("clones/standalone");
        git(
            tmp.path(),
            &[
                "clone",
                "--quiet",
                repo.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );

        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();
        let info = registry.register(&clone, "clone-job").unwrap();
        assert!(!info.linked_worktree);
        assert_eq!(info.common_dir, canonicalize(&clone.join(".git")).unwrap());
    }

    #[test]
    fn branch_ahead_of_base_registers_without_reset() {
        let (tmp, repo) = init_repo();
        let wt = tmp.path().join("wt-ahead");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "ahead",
                wt.to_str().unwrap(),
            ],
        );
        std::fs::write(wt.join("new.txt"), "job work\n").unwrap();
        git(&wt, &["add", "new.txt"]);
        git(&wt, &["commit", "--quiet", "-m", "job work"]);
        let ahead_commit = head(&wt);
        assert_ne!(ahead_commit, head(&repo));

        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();
        registry.register(&wt, "ahead-job").unwrap();
        let lease = registry.registered().unwrap().into_iter().next().unwrap();
        assert_eq!(lease.start_commit, ahead_commit);
        assert_eq!(head(&wt), ahead_commit, "register must not move HEAD");
    }

    #[test]
    fn unregister_without_lease_is_a_no_op() {
        let (tmp, repo) = init_repo();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();
        assert!(registry.unregister(&repo).unwrap().is_none());
    }

    #[test]
    fn register_rejects_non_checkout() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();
        assert!(registry.register(tmp.path(), "x").is_err());
    }
}
