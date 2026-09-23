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
use crate::lease::{
    AllocateRequest, AllocationState, JobKey, Lease, LeaseGrant, LeaseStore, ReconcileOutcome,
    attention_error,
};

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
    /// Repo segment derived from `origin_slug`, else the name of the directory
    /// containing the git common dir — so linked worktrees of one repository
    /// share an identity while standalone clones stay distinct.
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
        if self.resume_or_refresh(&info, job_id)? {
            return Ok(info);
        }
        self.commit_new_registration(&info, job_id)?;
        Ok(info)
    }

    fn resume_or_refresh(&self, info: &CheckoutInfo, job_id: &str) -> Result<bool> {
        let key = registration_job_key(info, job_id);
        let Some(existing) = self.leases.find_job(key)? else {
            return Ok(false);
        };
        match existing.allocation_state {
            AllocationState::Active | AllocationState::Released => {
                self.leases.grant(registration_grant(info, job_id))?;
                Ok(true)
            }
            AllocationState::Tombstoned => Err(tombstoned_job_error(info, job_id)),
            AllocationState::Prepared
            | AllocationState::Mutating
            | AllocationState::NeedsAttention
            | AllocationState::Aborted
            | AllocationState::Unknown => self.resume_interrupted(info, job_id, &existing),
        }
    }

    fn resume_interrupted(
        &self,
        info: &CheckoutInfo,
        job_id: &str,
        existing: &Lease,
    ) -> Result<bool> {
        if !registration_matches_lease(info, existing) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::LeaseConflict,
                message: format!(
                    "interrupted lease for {}/{}/{job_id} protects `{}` on `{}`; refusing to resume it for `{}` on `{}`",
                    info.owner,
                    info.repo_name,
                    existing.worktree_path,
                    existing.branch,
                    info.path.display(),
                    info.branch.as_deref().unwrap_or(DETACHED_BRANCH)
                ),
            });
        }
        let Some(outcome) = self
            .leases
            .reconcile(registration_job_key(info, job_id), &info.common_dir)?
        else {
            return Ok(false);
        };
        match outcome {
            ReconcileOutcome::Promoted { .. } | ReconcileOutcome::AlreadyActive { .. } => Ok(true),
            ReconcileOutcome::Retry { .. } => Ok(false),
            ReconcileOutcome::NeedsAttention { lease, inspection } => {
                Err(attention_error(&lease, &inspection))
            }
            ReconcileOutcome::Released { .. } => {
                self.leases.grant(registration_grant(info, job_id))?;
                Ok(true)
            }
            ReconcileOutcome::Tombstoned { lease, .. } => Err(tombstoned_op_error(&lease)),
        }
    }

    fn commit_new_registration(&self, info: &CheckoutInfo, job_id: &str) -> Result<()> {
        let requested_start_point = info
            .branch
            .as_deref()
            .map(|name| format!("refs/heads/{name}"))
            .unwrap_or_else(|| "HEAD".to_owned());
        let prepared = self.leases.prepare_allocate(AllocateRequest {
            repo: &info.common_dir,
            owner: &info.owner,
            repo_name: &info.repo_name,
            job_id,
            branch: info.branch.as_deref().unwrap_or(DETACHED_BRANCH),
            worktree_path: &info.path,
            requested_start_point: &requested_start_point,
            start_commit: info.head_commit.as_deref().unwrap_or(""),
            ttl: None,
        })?;
        self.leases.mark_mutating(&prepared.operation_id)?;
        self.leases.commit_allocate(&prepared.operation_id)?;
        Ok(())
    }

    /// Release the coordination record for `path` without touching the
    /// checkout itself. Returns the released lease when one was held.
    ///
    /// Only a row whose recorded `worktree_path` matches `path` is released, so
    /// stale state for a different path cannot steal a live registration.
    pub fn unregister(&self, path: &Path) -> Result<Option<Lease>> {
        self.leases.release_by_path(&checkout_path_key(path)?)
    }

    /// Currently held (unreleased) registrations.
    pub fn registered(&self) -> Result<Vec<Lease>> {
        self.leases.list_active()
    }
}

fn registration_job_key<'a>(info: &'a CheckoutInfo, job_id: &'a str) -> JobKey<'a> {
    JobKey {
        owner: &info.owner,
        repo_name: &info.repo_name,
        job_id,
    }
}

fn registration_matches_lease(info: &CheckoutInfo, lease: &Lease) -> bool {
    let branch = info.branch.as_deref().unwrap_or(DETACHED_BRANCH);
    let path_matches =
        crate::paths::same_existing_path(Path::new(&lease.worktree_path), &info.path)
            || lease.worktree_path == info.path.to_string_lossy();
    let repo_matches = crate::paths::same_existing_path(Path::new(&lease.repo), &info.common_dir)
        || lease.repo == info.common_dir.to_string_lossy();
    path_matches && repo_matches && lease.branch == branch
}

fn registration_grant<'a>(info: &'a CheckoutInfo, job_id: &'a str) -> LeaseGrant<'a> {
    LeaseGrant {
        repo: &info.common_dir,
        owner: &info.owner,
        repo_name: &info.repo_name,
        job_id,
        branch: info.branch.as_deref().unwrap_or(DETACHED_BRANCH),
        worktree_path: &info.path,
        start_commit: info.head_commit.as_deref().unwrap_or(""),
    }
}

fn tombstoned_job_error(info: &CheckoutInfo, job_id: &str) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::LeaseTombstoned,
        message: format!(
            "refusing to resurrect tombstoned lease for {}/{}/{job_id}",
            info.owner, info.repo_name
        ),
    }
}

fn tombstoned_op_error(lease: &Lease) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::LeaseTombstoned,
        message: format!(
            "refusing to resurrect tombstoned lease {}",
            lease.operation_id
        ),
    }
}

/// Read-only inspection of an existing checkout.
///
/// Resolves the git top-level and common dir, the real `HEAD` and branch (or
/// explicit detached state), the repository identity, and dirty state. Never
/// writes to the lease store or the working tree.
pub fn inspect_checkout(path: &Path) -> Result<CheckoutInfo> {
    inspect_checkout_inner(path, true)
}

/// HEAD and identity only. Skips `git status --porcelain`.
pub fn inspect_checkout_for_status(path: &Path) -> Result<CheckoutInfo> {
    inspect_checkout_inner(path, false)
}

fn inspect_checkout_inner(path: &Path, include_dirty: bool) -> Result<CheckoutInfo> {
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
    let dirty = include_dirty
        && git_optional(&path, &["status", "--porcelain"])
            .is_some_and(|status| !status.trim().is_empty());

    let origin_slug = crate::git_safe::origin_github_slug(&path).ok();
    let (owner, repo_name) = match origin_slug.as_deref().and_then(|s| s.split_once('/')) {
        Some((owner, repo)) => (owner.to_owned(), repo.to_owned()),
        None => ("local".to_owned(), local_repo_name(&common_dir)),
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

/// Normalize a checkout path for lease lookups without requiring it to exist.
///
/// Registration stores the canonical top-level, but `WorktreeRemove` and
/// `unregister` may run after the harness has already deleted the checkout, so
/// canonicalization is not always possible. Falls back to absolutizing and
/// collapsing `.`/`..` lexically, then canonicalizing the longest ancestor
/// that still exists — so a deleted leaf under a symlinked parent (e.g.
/// `/var` → `/private/var` on macOS) still matches the stored canonical path.
pub fn checkout_path_key(path: &Path) -> Result<PathBuf> {
    if let Ok(canonical) = canonicalize(path) {
        return Ok(canonical);
    }
    let parts = lexical_components(&absolutize_cwd(path)?);
    Ok(resolve_surviving_prefix(&parts).unwrap_or_else(|| parts.iter().collect()))
}

fn absolutize_cwd(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|e| Error::Io {
            context: "resolve current directory",
            source: e,
        })
}

/// Collapse `.`/`..` lexically, without touching the filesystem.
fn lexical_components(absolute: &Path) -> Vec<std::ffi::OsString> {
    let mut parts = Vec::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    parts
}

/// Canonicalize the longest path prefix that still exists and append the
/// remaining components verbatim.
fn resolve_surviving_prefix(parts: &[std::ffi::OsString]) -> Option<PathBuf> {
    (1..=parts.len()).rev().find_map(|i| {
        let prefix: PathBuf = parts[..i].iter().collect();
        // Same normalization as registration: resolves symlinks, short names,
        // and strips Windows verbatim prefixes.
        let base = crate::paths::canonicalize_for_tools(&prefix).ok()?;
        Some(parts[i..].iter().fold(base, |mut acc, part| {
            acc.push(part);
            acc
        }))
    })
}

/// Display name for a checkout path suitable as a default job id: the file name
/// of the normalized path (`writ worktree register .` yields the directory's
/// real name, not "checkout").
#[must_use]
pub fn default_job_id(path: &Path) -> String {
    checkout_path_key(path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "checkout".to_owned())
}

/// Repo name for an origin-less checkout: the directory containing its git
/// common dir (strip a trailing `/.git`), so linked worktrees of one repository
/// share an identity while standalone clones stay distinct.
fn local_repo_name(common_dir: &Path) -> String {
    let base = if common_dir.file_name().is_some_and(|n| n == ".git") {
        common_dir.parent().unwrap_or(common_dir)
    } else {
        common_dir
    };
    base.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| common_dir.to_string_lossy().into_owned())
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
    fn inspect_for_status_skips_porcelain_dirty_flag() {
        let (_tmp, repo) = init_repo();
        std::fs::write(repo.join("wip.txt"), "uncommitted\n").unwrap();
        let info = inspect_checkout_for_status(&repo).unwrap();
        assert!(!info.dirty);
        assert!(inspect_checkout(&repo).unwrap().dirty);
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

    #[test]
    fn linked_worktrees_share_repo_identity_for_coordination() {
        let (_tmp, repo) = init_repo();
        let wt = _tmp.path().join("elsewhere/wt-b");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "job/b",
                wt.to_str().unwrap(),
            ],
        );
        let primary = inspect_checkout(&repo).unwrap();
        let linked = inspect_checkout(&wt).unwrap();
        assert_eq!(
            linked.repo_name, primary.repo_name,
            "linked worktrees must share the repo segment of the lease identity"
        );
        assert_eq!(linked.owner, primary.owner);
    }

    #[test]
    fn register_does_not_resume_interrupted_lease_for_a_different_checkout() {
        let (tmp, repo) = init_repo();
        let wt1 = tmp.path().join("wts/one");
        let wt2 = tmp.path().join("wts/two");
        for (wt, branch) in [(&wt1, "job/one"), (&wt2, "job/two")] {
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    branch,
                    wt.to_str().unwrap(),
                ],
            );
        }

        let store_path = tmp.path().join("leases.db");
        let registry =
            CheckoutRegistry::with_store(LeaseStore::open(&store_path).unwrap()).unwrap();
        registry.register(&wt1, "shared-job").unwrap();
        let conn = rusqlite::Connection::open(&store_path).unwrap();
        conn.execute(
            "UPDATE leases SET allocation_state = 'PREPARED', mode = 'UNASSIGNED' WHERE job_id = 'shared-job'",
            [],
        )
        .unwrap();
        drop(conn);

        let err = registry.register(&wt2, "shared-job").unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::PolicyViolation {
                code: crate::error::PolicyCode::LeaseConflict,
                ..
            }
        ));
        let info = inspect_checkout(&wt1).unwrap();
        let stored = LeaseStore::open(&store_path)
            .unwrap()
            .find_job(JobKey {
                owner: &info.owner,
                repo_name: &info.repo_name,
                job_id: "shared-job",
            })
            .unwrap()
            .unwrap();
        assert!(stored.worktree_path.contains("wts/one"));
        assert_eq!(stored.allocation_state, AllocationState::Prepared);
    }

    #[test]
    fn register_second_active_path_for_same_job_fails() {
        let (tmp, repo) = init_repo();
        let wt1 = tmp.path().join("wts/one");
        let wt2 = tmp.path().join("wts/two");
        for (wt, branch) in [(&wt1, "job/one"), (&wt2, "job/two")] {
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    branch,
                    wt.to_str().unwrap(),
                ],
            );
        }

        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();
        registry.register(&wt1, "shared-job").unwrap();

        // Same job id, different live path: conflict, never a seize.
        let err = registry.register(&wt2, "shared-job").unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::PolicyViolation {
                code: crate::error::PolicyCode::LeaseConflict,
                ..
            }
        ));

        // Re-registering the same path under the same job refreshes.
        registry.register(&wt1, "shared-job").unwrap();

        // After release, the job id can move to the other path.
        registry.unregister(&wt1).unwrap();
        registry.register(&wt2, "shared-job").unwrap();
    }

    #[test]
    fn register_keeps_unknown_allocation_state_non_retryable() {
        let (tmp, repo) = init_repo();
        let store_path = tmp.path().join("leases.db");
        let registry =
            CheckoutRegistry::with_store(LeaseStore::open(&store_path).unwrap()).unwrap();
        registry.register(&repo, "job-a").unwrap();
        let conn = rusqlite::Connection::open(&store_path).unwrap();
        conn.execute(
            "UPDATE leases SET allocation_state = 'FUTURE_STATE' WHERE job_id = 'job-a'",
            [],
        )
        .unwrap();
        drop(conn);

        assert!(matches!(
            registry.register(&repo, "job-a"),
            Err(Error::LeaseAttention(_))
        ));
        let conn = rusqlite::Connection::open(&store_path).unwrap();
        let stored: String = conn
            .query_row(
                "SELECT allocation_state FROM leases WHERE job_id = 'job-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "FUTURE_STATE");
    }

    #[test]
    fn unregister_releases_lease_after_checkout_deleted() {
        let (tmp, repo) = init_repo();
        let wt = tmp.path().join("wts/gone");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "job/gone",
                wt.to_str().unwrap(),
            ],
        );
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();
        registry.register(&wt, "gone-job").unwrap();
        assert_eq!(registry.registered().unwrap().len(), 1);

        // Harness deleted the checkout before unregister/WorktreeRemove ran.
        std::fs::remove_dir_all(&wt).unwrap();
        let lease = registry
            .unregister(&wt)
            .unwrap()
            .expect("deleted checkout must still release its lease");
        assert_eq!(lease.job_id, "gone-job");
        assert!(registry.registered().unwrap().is_empty());
    }

    #[test]
    fn register_second_job_for_same_path_fails() {
        let (tmp, repo) = init_repo();
        let wt = tmp.path().join("wts/one");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "job/one",
                wt.to_str().unwrap(),
            ],
        );

        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let registry = CheckoutRegistry::with_store(store).unwrap();
        registry.register(&wt, "job-a").unwrap();

        let err = registry.register(&wt, "job-b").unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::PolicyViolation {
                code: crate::error::PolicyCode::LeaseConflict,
                ..
            }
        ));
        assert_eq!(registry.registered().unwrap().len(), 1);

        registry.unregister(&wt).unwrap();
        registry.register(&wt, "job-b").unwrap();
        let active = registry.registered().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].job_id, "job-b");
    }

    #[test]
    fn default_job_id_uses_directory_name_of_normalized_path() {
        assert_eq!(default_job_id(Path::new(".")), env_dir_name());
        assert_eq!(
            default_job_id(Path::new("some/dir/../checkout")),
            "checkout"
        );
    }

    fn env_dir_name() -> String {
        std::env::current_dir()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }
}
