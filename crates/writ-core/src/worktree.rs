use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::error::{
    Error, PolicyCode, Result, WorktreeCreationFailure, WorktreePostconditionFailure,
};
use crate::identity::{
    BranchName, BranchRef, CommitId, JobId, Owner, Repo, StartPoint, resolve_start_commit,
};
use crate::lease::{LeaseGrant, LeaseMode, LeaseStore, ResumeKey};
use crate::paths::{canonicalize_for_tools, derive_worktree_path, worktree_base_path};

#[derive(Clone, Copy)]
struct GitArgList<'a>(&'a [&'a str]);

#[derive(Clone, Copy)]
struct IoContext(&'static str);

#[derive(Clone, Copy)]
struct PorcelainListing<'a>(&'a str);

#[derive(Clone, Copy)]
struct PostconditionCause<'a>(&'a str);

enum CreateMode {
    Fresh,
    Reclaim,
    AlreadyPresent,
}

/// Result of a worktree creation operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute path to the created worktree.
    pub path: PathBuf,
    /// The branch name associated with this worktree.
    pub branch: String,
    /// The repository root this worktree is linked to.
    pub repo_root: PathBuf,
    /// Fully resolved start commit for a creation result; absent from discovery-only listings.
    pub start_commit: Option<String>,
    /// Independently verified worker HEAD for a creation result; absent from listings.
    pub head_commit: Option<String>,
}

/// Inputs required to create one isolated worktree at an explicit start point.
#[derive(Debug, Clone, Copy)]
pub struct WorktreeCreateRequest<'a> {
    pub repo_root: &'a Path,
    pub owner: &'a str,
    pub repo: &'a str,
    pub job_id: &'a str,
    pub branch: &'a str,
    pub start_point: &'a str,
}

/// Manages isolated git worktrees for hive jobs.
///
/// The writable SQLite lease store is opened lazily. `worktree list` and
/// `worktree prune` are read-only over git state and must not require a
/// writable state/worktree base, so opening (and creating the WAL files for)
/// the lease store is deferred to the first lease-dependent operation
/// (`create`/`remove`). Manager construction only records where the store
/// lives; it does not touch it.
#[derive(Debug)]
pub struct WorktreeManager {
    base_path: Option<PathBuf>,
    /// Path the lease store lives at, used to open it on first lease-dependent
    /// use. `None` when a store was supplied directly (already in `leases`).
    lease_path: Option<PathBuf>,
    /// Lazily-opened lease store. Populated eagerly by `with_base_and_leases`
    /// (which is handed a ready store), otherwise opened on first access via
    /// [`Self::leases`].
    leases: OnceLock<LeaseStore>,
}

impl WorktreeManager {
    /// Create a new manager using the default base path and the durable lease store.
    ///
    /// The lease store is NOT opened here: `worktree list`/`prune` must work
    /// without a writable base. It is opened on first lease-dependent use.
    pub fn new() -> Result<Self> {
        Ok(Self {
            base_path: Some(worktree_base_path()?),
            lease_path: Some(crate::paths::lease_store_path()),
            leases: OnceLock::new(),
        })
    }

    /// Create a new manager with an explicit base path (for testing or overrides).
    ///
    /// The base is created if missing and stored in canonical form so OS path
    /// aliases (e.g. macOS `/var` → `/private/var`) do not trip sandbox checks.
    /// A per-base SQLite lease file is opened lazily next to the worktrees so
    /// tests do not share a process-global store and read-only commands do not
    /// create it.
    pub fn with_base(base: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base).map_err(|e| Error::Io {
            context: "create worktree base directory",
            source: e,
        })?;
        let base = canonicalize_for_tools(&base).map_err(|e| Error::Io {
            context: "canonicalize worktree base directory",
            source: e,
        })?;
        let lease_path = base.join("leases.db");
        Ok(Self {
            base_path: Some(base),
            lease_path: Some(lease_path),
            leases: OnceLock::new(),
        })
    }

    /// Create a manager with an explicit base path and an already-open lease store.
    pub fn with_base_and_leases(base: PathBuf, leases: LeaseStore) -> Result<Self> {
        fs::create_dir_all(&base).map_err(|e| Error::Io {
            context: "create worktree base directory",
            source: e,
        })?;
        // Canonicalize for OS aliases (macOS /var → /private/var) but strip
        // Windows `\\?\` so `git worktree add` accepts the path.
        let base = canonicalize_for_tools(&base).map_err(|e| Error::Io {
            context: "canonicalize worktree base directory",
            source: e,
        })?;
        let cell = OnceLock::new();
        // A ready store was supplied; install it so no lazy open ever runs.
        let _ = cell.set(leases);
        Ok(Self {
            base_path: Some(base),
            lease_path: None,
            leases: cell,
        })
    }

    /// Get the base path this manager uses.
    pub fn base_path(&self) -> Result<&Path> {
        self.base_path.as_deref().ok_or_else(|| Error::Io {
            context: "worktree base path not initialized",
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "base path not set"),
        })
    }

    /// Borrow the lease store, opening it on first use.
    ///
    /// Only lease-dependent operations (`create`/`remove`) call this, so
    /// read-only `list`/`prune` never open (or create) the store.
    fn leases(&self) -> Result<&LeaseStore> {
        if let Some(store) = self.leases.get() {
            return Ok(store);
        }
        let path = self.lease_path.as_ref().ok_or_else(|| Error::LeaseStore {
            context: "open lease store",
            message: "lease store path not initialized".to_owned(),
        })?;
        let store = LeaseStore::open(path.clone())?;
        // A concurrent caller may have populated the cell first; keep whichever
        // won and drop the loser. Either way we return the installed store.
        let _ = self.leases.set(store);
        self.leases.get().ok_or_else(|| Error::LeaseStore {
            context: "open lease store",
            message: "lease store missing after initialization".to_owned(),
        })
    }

    /// Borrow the lease store used by this manager, opening it on first use.
    ///
    /// Test/introspection accessor. Prefer the internal [`Self::leases`] in
    /// library code. Returns an error if the store cannot be opened.
    pub fn lease_store(&self) -> Result<&LeaseStore> {
        self.leases()
    }

    /// Create a new worktree for the given job.
    ///
    /// The worktree path will be: `{base}/{owner}/{repo}/{job_id}`.
    /// `start_point` is required and is resolved to a commit before any branch
    /// mutation. Existing branches are reclaimed only when a durable lease
    /// identity plus branch and upstream checks prove the ref still belongs to
    /// this job. Foreign, moved, or concurrently adopted branches stay fail-closed.
    ///
    /// This six-argument shape is a frozen compatibility wrapper. New callers
    /// should use [`Self::create_with_request`] so identity fields stay packed.
    ///
    /// CodeScene flags this as "String Heavy Function Arguments" and that finding
    /// is accepted, not suppressed. A `@codescene(disable:...)` directive used to
    /// sit here; CodeScene does not permit overriding this particular rule, so the
    /// directive was silently ignored -- `cs delta` reports "you cannot override
    /// the following rules" and the analysis API counts
    /// `total_number_of_code_health_directives: 0`. It has been removed rather
    /// than left in place, which would imply the finding was handled.
    pub fn create(
        &self,
        repo_root: &Path,
        owner: &str,
        repo: &str,
        job_id: &str,
        branch: &str,
        start_point: &str,
    ) -> Result<Worktree> {
        self.create_with_request(WorktreeCreateRequest {
            repo_root,
            owner,
            repo,
            job_id,
            branch,
            start_point,
        })
    }

    /// Create a worktree from a typed request used by CLI and orchestration adapters.
    pub fn create_with_request(&self, request: WorktreeCreateRequest<'_>) -> Result<Worktree> {
        let branch = BranchName(request.branch);
        let start_point = StartPoint(request.start_point);
        validate_worktree_branch(branch)?;
        let base = self.base_path()?;
        validate_repo_root(request.repo_root)?;

        // Resolve the caller-selected start point before any mutation. Appending
        // ^{commit} rejects trees/blobs and peels annotated tags to commits.
        let start_commit = resolve_start_commit(request.repo_root, start_point)?;
        let worktree_path = prepare_worktree_path(
            base,
            Owner(request.owner),
            Repo(request.repo),
            JobId(request.job_id),
        )?;
        let leases = self.leases()?;
        let mode = decide_create_mode(&request, CommitId(&start_commit), &worktree_path, leases)?;
        let grant = LeaseGrant {
            repo: request.repo_root,
            owner: request.owner,
            repo_name: request.repo,
            job_id: request.job_id,
            branch: request.branch,
            worktree_path: &worktree_path,
            start_commit: &start_commit,
        };
        match mode {
            // AlreadyPresent and Reclaim both required an already-proven lease
            // (`prove_resume` ran in `decide_create_mode`), so this job already
            // owns the branch. Persist identity BEFORE any git mutation so a
            // later add/verify error still leaves a reclaimable lease for the
            // owner. Residual git state is not deleted.
            CreateMode::AlreadyPresent => {
                leases.grant(grant)?;
            }
            CreateMode::Reclaim => {
                leases.grant(grant)?;
                add_worktree(&request, &worktree_path, CommitId(&start_commit), true)?;
            }
            // Fresh create must NOT persist a lease before winning the ref.
            // `git worktree add -b` fails if the branch ref already exists, so
            // it is the exclusive arbiter of who owns a brand-new branch. If we
            // granted first, two jobs racing the same absent branch would both
            // persist leases and the loser's stale lease could later prove
            // ownership of the winner's branch. Grant only AFTER `add -b` wins;
            // if the add fails, no ownership-proving lease is left behind.
            CreateMode::Fresh => {
                add_worktree(&request, &worktree_path, CommitId(&start_commit), false)?;
                leases.grant(grant)?;
            }
        }

        let head_commit = verify_creation_postconditions(CreationPostconditions {
            repo_root: request.repo_root,
            worktree_path: &worktree_path,
            expected_branch: branch,
            expected_commit: CommitId(&start_commit),
        })?;

        Ok(Worktree {
            path: worktree_path,
            branch: request.branch.to_owned(),
            repo_root: request.repo_root.to_path_buf(),
            start_commit: Some(start_commit),
            head_commit: Some(head_commit),
        })
    }

    /// List all hive worktrees under the base path.
    pub fn list(&self) -> Result<Vec<Worktree>> {
        let base = self.base_path()?;
        let mut worktrees = Vec::new();

        if !base.exists() {
            return Ok(worktrees);
        }

        // Walk the base directory: {base}/{owner}/{repo}/{job_id}
        for owner_entry in fs::read_dir(base).map_err(|e| Error::Io {
            context: "read worktree base directory",
            source: e,
        })? {
            let owner_entry = owner_entry.map_err(|e| Error::Io {
                context: "read owner entry",
                source: e,
            })?;
            if !owner_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }

            for repo_entry in fs::read_dir(owner_entry.path()).map_err(|e| Error::Io {
                context: "read repo directory",
                source: e,
            })? {
                let repo_entry = repo_entry.map_err(|e| Error::Io {
                    context: "read repo entry",
                    source: e,
                })?;
                if !repo_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }

                for job_entry in fs::read_dir(repo_entry.path()).map_err(|e| Error::Io {
                    context: "read job directory",
                    source: e,
                })? {
                    let job_entry = job_entry.map_err(|e| Error::Io {
                        context: "read job entry",
                        source: e,
                    })?;
                    if !job_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }

                    // Try to get the branch name from the worktree
                    let branch = get_worktree_branch(&job_entry.path())
                        .unwrap_or_else(|_| "unknown".to_string());

                    worktrees.push(Worktree {
                        path: job_entry.path(),
                        branch,
                        repo_root: PathBuf::new(), // Not tracked in list
                        start_commit: None,        // Not tracked in list
                        head_commit: None,         // Not tracked in list
                    });
                }
            }
        }

        Ok(worktrees)
    }

    /// Remove a worktree by its path.
    ///
    /// If `force` is true, the worktree is removed even if it has uncommitted changes.
    /// The associated branch is NOT deleted by default. The lease row is released
    /// without deleting resume identity so a later create can reclaim the branch.
    pub fn remove(&self, worktree_path: &Path, force: bool) -> Result<()> {
        let base = self.base_path()?;
        // Canonicalize up front so every downstream step -- the sandbox check,
        // the git `worktree remove` argument, parent cleanup, and the lease
        // release -- uses the same spelling that `grant` stored. A raw,
        // non-canonical, or relative path with redundant segments would
        // otherwise fail to match the stored lease row and leak ownership.
        let canonical_path = canonicalize_removal_target(worktree_path)?;

        // Enforce sandbox containment BEFORE any lease release, including the
        // fast path for an already-gone worktree. Releasing a lease for a path
        // outside the sandbox would let a foreign path drop another job's lock.
        // `canonical_path` is already canonical (even when the leaf is gone), so
        // compare against the canonical base directly instead of re-canonicalizing
        // a possibly non-existent path.
        if !canonical_path.starts_with(base) {
            return Err(Error::SandboxViolation {
                base: base.to_path_buf(),
                candidate: worktree_path.to_path_buf(),
                reason: "worktree path is outside configured base",
            });
        }

        if !canonical_path.exists() {
            // The directory is gone but git may still hold a stale registration
            // for it. Prune it before releasing the lease so a later reclaim's
            // `git worktree add` is not blocked by a stranded entry. This is
            // best-effort: if the repo root cannot be resolved (the whole tree
            // is gone), we keep the historical release-only behavior. We never
            // delete branches or user content.
            if let Ok(repo_root) = find_repo_root_for_worktree(&canonical_path) {
                prune_stale_registration(&repo_root);
            }
            self.leases()?.release_by_path(&canonical_path)?;
            return Ok(());
        }

        // Find the repo root for this worktree
        let repo_root = find_repo_root_for_worktree(&canonical_path)?;

        let mut args = vec!["worktree".into(), "remove".into()];
        if force {
            args.push("--force".into());
        }
        args.push("--".into());
        args.push(canonical_path.to_string_lossy().to_string());

        let output = Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .args(&args)
            .output()
            .map_err(|e| Error::Io {
                context: "spawn git worktree remove",
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::GitCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                stderr,
            });
        }

        // Also clean up empty parent directories
        cleanup_empty_parents(&canonical_path, base);
        self.leases()?.release_by_path(&canonical_path)?;

        Ok(())
    }

    /// Prune worktree administrative files (stale entries).
    pub fn prune(&self, repo_root: &Path) -> Result<()> {
        // No validation here, deliberately. `create` calls `validate_repo_root` and
        // `remove` checks sandbox containment, so the absence looks like an
        // oversight and has been flagged as one. It is not, and adding a check
        // would be theatre:
        //
        // - `validate_repo_root` only rejects "not a git repository", which git
        //   itself already rejects with the same `Error::GitCommand` shape. The
        //   tests below pass with or without such a call -- verified by removing
        //   it and re-running.
        // - It would *accept* any repository outside the sandbox, so it does not
        //   constrain which repository can be targeted. The perceived gap stays
        //   open either way.
        // - Sandbox containment is the wrong invariant: `prune` takes a repository
        //   root, not a worktree path, and the primary checkout legitimately lives
        //   outside the worktree base.
        //
        // `git worktree prune` removes administrative entries for worktrees whose
        // directories are already gone. It cannot delete a live worktree or any
        // user content, which is why this is an asymmetry rather than a hole.
        // If that ever stops being true, the guard belongs here.
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("worktree")
            .arg("prune")
            .output()
            .map_err(|e| Error::Io {
                context: "spawn git worktree prune",
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::GitCommand {
                args: vec!["worktree".into(), "prune".into()],
                stderr,
            });
        }

        Ok(())
    }
}

fn validate_worktree_branch(branch: BranchName<'_>) -> Result<()> {
    match branch.as_str().chars().next() {
        None | Some('-') => Err(Error::GitCommand {
            args: vec!["worktree".into(), "add".into()],
            stderr: format!(
                "invalid branch name (empty or option-looking): {:?}",
                branch.as_str()
            ),
        }),
        Some(_) => Ok(()),
    }
}

fn prepare_worktree_path(
    base: &Path,
    owner: Owner<'_>,
    repo: Repo<'_>,
    job_id: JobId<'_>,
) -> Result<PathBuf> {
    let worktree_path = derive_worktree_path(base, owner.as_str(), repo.as_str(), job_id.as_str())?;

    // Reject symlink segments beneath the canonical base before and after
    // creating parents so a planted link is never followed.
    reject_symlink_components_under(base, &worktree_path)?;
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::Io {
            context: "create worktree parent directories",
            source: e,
        })?;
        reject_symlink_components_under(base, parent)?;
    }
    Ok(worktree_path)
}

fn validate_repo_root(repo_root: &Path) -> Result<()> {
    if repo_root.join(".git").exists() {
        return Ok(());
    }
    if is_bare_repo(repo_root)? {
        return Ok(());
    }
    Err(Error::GitCommand {
        args: vec!["worktree".into(), "add".into()],
        stderr: format!("not a git repository: {}", repo_root.display()),
    })
}

fn decide_create_mode(
    request: &WorktreeCreateRequest<'_>,
    start_commit: CommitId<'_>,
    worktree_path: &Path,
    leases: &LeaseStore,
) -> Result<CreateMode> {
    if let Some(mode) = existing_worktree_mode(request, start_commit, worktree_path)? {
        prove_resume(request, start_commit, worktree_path, leases)?;
        return Ok(mode);
    }
    if !branch_exists_in_repo(request.repo_root, BranchName(request.branch))? {
        return Ok(CreateMode::Fresh);
    }
    prove_resume(request, start_commit, worktree_path, leases)?;
    Ok(CreateMode::Reclaim)
}

fn existing_worktree_mode(
    request: &WorktreeCreateRequest<'_>,
    start_commit: CommitId<'_>,
    worktree_path: &Path,
) -> Result<Option<CreateMode>> {
    if !worktree_path.exists() {
        return Ok(None);
    }
    let Some(listing) = optional_git_stdout(
        request.repo_root,
        GitArgList(&["worktree", "list", "--porcelain"]),
    ) else {
        return Ok(None);
    };
    let expected_branch = format!("refs/heads/{}", request.branch);
    if worktree_registration_matches(
        PorcelainListing(&listing),
        worktree_path,
        BranchRef(&expected_branch),
        start_commit,
    ) {
        return Ok(Some(CreateMode::AlreadyPresent));
    }
    if path_is_registered(PorcelainListing(&listing), worktree_path) {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            None,
            "existing worktree registration does not match branch identity and start commit",
        ));
    }
    Ok(None)
}

fn prove_resume(
    request: &WorktreeCreateRequest<'_>,
    start_commit: CommitId<'_>,
    worktree_path: &Path,
    leases: &LeaseStore,
) -> Result<()> {
    let Some(lease) = leases.find_resume(ResumeKey {
        owner: request.owner,
        repo_name: request.repo,
        job_id: request.job_id,
        branch: request.branch,
    })?
    else {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            None,
            "no durable lease identity for this owner/repo/job/branch",
        ));
    };
    let expected_ref = format!("refs/heads/{}", request.branch);
    let ownership = format!(
        "lease_mode={} lease_start_commit={} lease_branch_ref={} lease_path={}",
        lease.mode.as_str(),
        lease.start_commit,
        lease.branch_ref,
        lease.worktree_path
    );
    if lease.branch_ref != expected_ref {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            Some(&ownership),
            &format!(
                "lease branch_ref {} does not match expected {expected_ref}",
                lease.branch_ref
            ),
        ));
    }
    if !matches!(lease.mode, LeaseMode::WriterLocked | LeaseMode::Unassigned) {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            Some(&ownership),
            &format!(
                "lease mode {} is not a reclaimable writer or released identity",
                lease.mode.as_str()
            ),
        ));
    }
    if Path::new(&lease.worktree_path) != worktree_path {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            Some(&ownership),
            "lease worktree path does not match the derived canonical job path",
        ));
    }
    if lease.start_commit != start_commit.as_str() {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            Some(&ownership),
            &format!(
                "lease start_commit {} does not match requested commit {}",
                lease.start_commit,
                start_commit.as_str()
            ),
        ));
    }
    let branch_commit = git_stdout(
        request.repo_root,
        GitArgList(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("refs/heads/{}^{{commit}}", request.branch),
        ]),
        IoContext("verify resume branch commit"),
    )?;
    if branch_commit != start_commit.as_str() {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            Some(&ownership),
            &format!("branch moved: current commit {branch_commit}"),
        ));
    }
    if branch_checked_out_elsewhere(request.repo_root, request.branch, worktree_path)? {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            Some(&ownership),
            "branch is checked out in another worktree",
        ));
    }
    if let Some(reason) = resume_upstream_mismatch(request.repo_root, request.branch)? {
        return Err(resume_unproven(
            request,
            start_commit,
            worktree_path,
            Some(&ownership),
            &reason,
        ));
    }
    Ok(())
}

fn resume_upstream_mismatch(repo_root: &Path, branch: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            &format!("{branch}@{{upstream}}"),
        ])
        .output()
        .map_err(|e| Error::Io {
            context: "verify resume upstream",
            source: e,
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let upstream = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !tracking_ref_matches_branch(&upstream, branch) {
        return Ok(Some(format!(
            "upstream {upstream:?} is not the expected tracking ref"
        )));
    }
    // The upstream ref name is expected; now verify the COMMIT relationship
    // (AGENTS.md: a published branch may only be equal to or ahead of its
    // upstream by job-owned commits; stop on behind or divergent state).
    resume_upstream_commit_mismatch(repo_root, branch, &upstream)
}

/// Compare the local branch against its configured upstream by commit.
///
/// Reclaim is rejected when the branch is BEHIND (the upstream contains commits
/// the local branch does not) or DIVERGENT (neither commit is an ancestor of
/// the other). Equal or strictly-ahead is reclaimable.
///
/// We deliberately do NOT `git fetch` inside core: fetching is a networked,
/// side-effecting operation that belongs to the caller's remote-alignment step,
/// and tests run against local repositories. We compare against the local
/// remote-tracking state that a prior fetch already recorded, exactly as
/// `@{upstream}` resolves it.
fn resume_upstream_commit_mismatch(
    repo_root: &Path,
    branch: &str,
    upstream: &str,
) -> Result<Option<String>> {
    let Some(local) = optional_git_stdout(
        repo_root,
        GitArgList(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("refs/heads/{branch}^{{commit}}"),
        ]),
    ) else {
        return Ok(None);
    };
    let Some(remote) = optional_git_stdout(
        repo_root,
        GitArgList(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{upstream}^{{commit}}"),
        ]),
    ) else {
        // The tracking ref name is configured but no local remote-tracking
        // commit exists yet (nothing fetched). There is no recorded upstream
        // state to be behind or divergent from, so this is not a mismatch.
        return Ok(None);
    };
    if local == remote {
        return Ok(None);
    }
    // `--is-ancestor` exits 0 when the first commit is an ancestor of the
    // second. Local ahead: remote is an ancestor of local (reclaimable).
    // Behind: local is an ancestor of remote. Divergent: neither.
    let local_is_ancestor_of_remote = git_is_ancestor(repo_root, &local, &remote)?;
    let remote_is_ancestor_of_local = git_is_ancestor(repo_root, &remote, &local)?;
    if remote_is_ancestor_of_local {
        // Local strictly ahead of upstream by job-owned commits: reclaimable.
        return Ok(None);
    }
    if local_is_ancestor_of_remote {
        return Ok(Some(format!(
            "branch is behind its upstream {upstream:?}: \
             local {local} is an ancestor of remote {remote}"
        )));
    }
    Ok(Some(format!(
        "branch has diverged from its upstream {upstream:?}: \
         local {local} and remote {remote} share no ancestor relationship"
    )))
}

/// True when `ancestor` is an ancestor of `descendant` (or they are equal).
///
/// `git merge-base --is-ancestor` exits 0 for ancestor, 1 for not-ancestor,
/// and anything else is a real error we surface.
fn git_is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            ancestor,
            descendant,
        ])
        .output()
        .map_err(|e| Error::Io {
            context: "verify resume upstream ancestry",
            source: e,
        })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::GitCommand {
            args: vec![
                "merge-base".into(),
                "--is-ancestor".into(),
                ancestor.to_owned(),
                descendant.to_owned(),
            ],
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }),
    }
}

/// A configured upstream is reclaimable only when it is the branch itself or
/// `{remote}/{branch}` with a single remote-name segment. A suffix match would
/// admit `origin/evil/{branch}`.
fn tracking_ref_matches_branch(upstream: &str, branch: &str) -> bool {
    if upstream == branch {
        return true;
    }
    upstream
        .split_once('/')
        .is_some_and(|(remote, name)| !remote.is_empty() && name == branch)
}

fn branch_checked_out_elsewhere(
    repo_root: &Path,
    branch: &str,
    expected_path: &Path,
) -> Result<bool> {
    let listing = git_stdout(
        repo_root,
        GitArgList(&["worktree", "list", "--porcelain"]),
        IoContext("list worktrees for resume check"),
    )?;
    let expected_ref = format!("refs/heads/{branch}");
    Ok(listing.split("\n\n").any(|entry| {
        let mut path = None;
        let mut checked_out = None;
        for line in entry.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = Some(Path::new(value));
            } else if let Some(value) = line.strip_prefix("branch ") {
                checked_out = Some(value);
            }
        }
        checked_out == Some(expected_ref.as_str()) && path != Some(expected_path)
    }))
}

fn resume_unproven(
    request: &WorktreeCreateRequest<'_>,
    start_commit: CommitId<'_>,
    worktree_path: &Path,
    ownership: Option<&str>,
    reason: &str,
) -> Error {
    let residual =
        inspect_residual_state(request.repo_root, worktree_path, BranchName(request.branch));
    let branch_ref = format!("refs/heads/{}", request.branch);
    Error::PolicyViolation {
        code: PolicyCode::WorktreeResumeUnproven,
        message: format!(
            "refusing to reuse existing branch {:?} at requested commit {}: {reason}; \
             residual_state path={} path_exists={} registered={} branch_ref={} \
             branch_commit={} head_commit={} ownership_evidence={}; automatic cleanup skipped",
            request.branch,
            start_commit.as_str(),
            worktree_path.display(),
            residual.path_exists,
            residual.worktree_registered,
            branch_ref,
            residual.branch_commit.as_deref().unwrap_or("<absent>"),
            residual.head_commit.as_deref().unwrap_or("<absent>"),
            ownership.unwrap_or("lease=<absent>")
        ),
    }
}

fn path_is_registered(listing: PorcelainListing<'_>, expected_path: &Path) -> bool {
    listing.0.lines().any(|line| {
        line.strip_prefix("worktree ")
            .is_some_and(|path| Path::new(path) == expected_path)
    })
}

fn add_worktree(
    request: &WorktreeCreateRequest<'_>,
    worktree_path: &Path,
    start_commit: CommitId<'_>,
    reuse_existing_branch: bool,
) -> Result<()> {
    let branch = BranchName(request.branch);
    // Fresh create uses `worktree add -b` so a concurrent actor creating the
    // ref first makes Git fail rather than attaching to it. Reclaim attaches
    // to the already-proven branch without creating a new ref.
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(request.repo_root)
        .arg("worktree")
        .arg("add");
    if reuse_existing_branch {
        command.arg("--").arg(worktree_path).arg(branch.as_str());
    } else {
        command
            .arg("-b")
            .arg(branch.as_str())
            .arg("--")
            .arg(worktree_path)
            .arg(start_commit.as_str());
    }
    let output = command.output().map_err(|e| Error::Io {
        context: "spawn git worktree add",
        source: e,
    })?;

    if output.status.success() {
        return Ok(());
    }
    let residual = inspect_residual_state(request.repo_root, worktree_path, branch);
    Err(Error::WorktreeCreationFailed(Box::new(
        WorktreeCreationFailure {
            path: worktree_path.to_path_buf(),
            branch: branch.as_str().to_owned(),
            path_exists: residual.path_exists,
            branch_commit: residual.branch_commit,
            head_commit: residual.head_commit,
            worktree_registered: residual.worktree_registered,
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
    )))
}

/// Check if a path is a bare git repository.
fn is_bare_repo(path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--is-bare-repository")
        .output()
        .map_err(|e| Error::Io {
            context: "check bare repository",
            source: e,
        })?;

    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
}

/// Check if a branch exists in the repository.
fn branch_exists_in_repo(repo_root: &Path, branch: BranchName<'_>) -> Result<bool> {
    let branch_ref = format!("refs/heads/{}", branch.as_str());
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("show-ref")
        .arg("--verify")
        .arg("--quiet")
        .arg(&branch_ref)
        .output()
        .map_err(|e| Error::Io {
            context: "check branch existence",
            source: e,
        })?;

    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::GitCommand {
            args: vec![
                "show-ref".into(),
                "--verify".into(),
                "--quiet".into(),
                branch_ref,
            ],
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }),
    }
}

struct CreationPostconditions<'a> {
    repo_root: &'a Path,
    worktree_path: &'a Path,
    expected_branch: BranchName<'a>,
    expected_commit: CommitId<'a>,
}

impl CreationPostconditions<'_> {
    fn actual_branch_ref(&self) -> Result<String> {
        git_stdout(
            self.worktree_path,
            GitArgList(&["symbolic-ref", "--quiet", "HEAD"]),
            IoContext("verify created worktree branch"),
        )
        .map_err(|error| self.failure(None, PostconditionCause(&error.to_string())))
    }

    fn branch_commit(&self, actual_branch: BranchRef<'_>) -> Result<String> {
        git_stdout(
            self.repo_root,
            GitArgList(&[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("refs/heads/{}^{{commit}}", self.expected_branch.as_str()),
            ]),
            IoContext("verify created branch commit"),
        )
        .map_err(|error| self.failure(Some(actual_branch), PostconditionCause(&error.to_string())))
    }

    fn head_commit(&self, actual_branch: BranchRef<'_>) -> Result<String> {
        git_stdout(
            self.worktree_path,
            GitArgList(&["rev-parse", "--verify", "HEAD^{commit}"]),
            IoContext("verify created worktree HEAD"),
        )
        .map_err(|error| self.failure(Some(actual_branch), PostconditionCause(&error.to_string())))
    }

    fn verify_identity(
        &self,
        actual_branch_ref: BranchRef<'_>,
        branch_commit: CommitId<'_>,
        head_commit: CommitId<'_>,
    ) -> Result<()> {
        let expected_branch_ref = format!("refs/heads/{}", self.expected_branch.as_str());
        let actual = [
            actual_branch_ref.as_str(),
            branch_commit.as_str(),
            head_commit.as_str(),
        ];
        let expected = [
            expected_branch_ref.as_str(),
            self.expected_commit.as_str(),
            self.expected_commit.as_str(),
        ];
        if actual != expected {
            return Err(self.failure(
                Some(actual_branch_ref),
                PostconditionCause(&format!(
                    "identity mismatch: actual branch ref={:?} \
                     branch_commit={}, head_commit={}",
                    actual_branch_ref.as_str(),
                    branch_commit.as_str(),
                    head_commit.as_str()
                )),
            ));
        }
        Ok(())
    }

    fn verify_registration(
        &self,
        actual_branch_ref: BranchRef<'_>,
        head_commit: CommitId<'_>,
    ) -> Result<()> {
        let listing = git_stdout(
            self.repo_root,
            GitArgList(&["worktree", "list", "--porcelain"]),
            IoContext("verify created worktree registration"),
        )
        .map_err(|error| {
            self.failure(
                Some(actual_branch_ref),
                PostconditionCause(&error.to_string()),
            )
        })?;
        if !worktree_registration_matches(
            PorcelainListing(&listing),
            self.worktree_path,
            actual_branch_ref,
            head_commit,
        ) {
            return Err(self.failure(
                Some(actual_branch_ref),
                PostconditionCause(
                    "worktree registration does not match the expected path, branch, and HEAD",
                ),
            ));
        }
        Ok(())
    }

    fn failure(
        &self,
        actual_branch: Option<BranchRef<'_>>,
        cause: PostconditionCause<'_>,
    ) -> Error {
        let residual =
            inspect_residual_state(self.repo_root, self.worktree_path, self.expected_branch);
        Error::WorktreePostconditionFailed(Box::new(WorktreePostconditionFailure {
            path: self.worktree_path.to_path_buf(),
            branch: self.expected_branch.as_str().to_owned(),
            expected_commit: self.expected_commit.as_str().to_owned(),
            actual_branch: actual_branch.map(|branch| branch.as_str().to_owned()),
            path_exists: residual.path_exists,
            branch_commit: residual.branch_commit,
            head_commit: residual.head_commit,
            worktree_registered: residual.worktree_registered,
            reason: cause.0.to_owned(),
        }))
    }
}

fn verify_creation_postconditions(postconditions: CreationPostconditions<'_>) -> Result<String> {
    let actual_branch_ref = postconditions.actual_branch_ref()?;
    let actual_branch = BranchRef(&actual_branch_ref);
    let branch_commit = postconditions.branch_commit(actual_branch)?;
    let head_commit = postconditions.head_commit(actual_branch)?;
    postconditions.verify_identity(
        actual_branch,
        CommitId(&branch_commit),
        CommitId(&head_commit),
    )?;
    postconditions.verify_registration(actual_branch, CommitId(&head_commit))?;
    Ok(head_commit)
}

fn worktree_registration_matches(
    listing: PorcelainListing<'_>,
    expected_path: &Path,
    expected_branch_ref: BranchRef<'_>,
    expected_head: CommitId<'_>,
) -> bool {
    listing.0.split("\n\n").any(|entry| {
        let mut path = None;
        let mut branch = None;
        let mut head = None;
        for line in entry.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = Some(Path::new(value));
            } else if let Some(value) = line.strip_prefix("branch ") {
                branch = Some(value);
            } else if let Some(value) = line.strip_prefix("HEAD ") {
                head = Some(value);
            }
        }
        path == Some(expected_path)
            && branch == Some(expected_branch_ref.as_str())
            && head == Some(expected_head.as_str())
    })
}

fn git_stdout(repo: &Path, args: GitArgList<'_>, context: IoContext) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args.0)
        .output()
        .map_err(|e| Error::Io {
            context: context.0,
            source: e,
        })?;
    if !output.status.success() {
        return Err(Error::GitCommand {
            args: args.0.iter().map(|arg| (*arg).to_owned()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn optional_git_stdout(repo: &Path, args: GitArgList<'_>) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args.0)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

struct ResidualState {
    path_exists: bool,
    branch_commit: Option<String>,
    head_commit: Option<String>,
    worktree_registered: bool,
}

fn inspect_residual_state(
    repo_root: &Path,
    worktree_path: &Path,
    branch: BranchName<'_>,
) -> ResidualState {
    let branch_commit = optional_git_stdout(
        repo_root,
        GitArgList(&[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}^{{commit}}", branch.as_str()),
        ]),
    );
    let head_commit = optional_git_stdout(
        worktree_path,
        GitArgList(&["rev-parse", "--verify", "HEAD^{commit}"]),
    );
    let worktree_registered =
        optional_git_stdout(repo_root, GitArgList(&["worktree", "list", "--porcelain"]))
            .is_some_and(|listing| {
                listing.lines().any(|line| {
                    line.strip_prefix("worktree ")
                        .is_some_and(|path| Path::new(path) == worktree_path)
                })
            });
    ResidualState {
        path_exists: worktree_path.exists(),
        branch_commit,
        head_commit,
        worktree_registered,
    }
}

/// Get the branch name associated with a worktree.
fn get_worktree_branch(worktree_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .map_err(|e| Error::Io {
            context: "get worktree branch",
            source: e,
        })?;

    if !output.status.success() {
        return Err(Error::GitCommand {
            args: vec!["rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()],
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Find the repository root for a given worktree path.
fn find_repo_root_for_worktree(worktree_path: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .arg("rev-parse")
        .arg("--git-common-dir")
        .output()
        .map_err(|e| Error::Io {
            context: "find repo root for worktree",
            source: e,
        })?;

    if !output.status.success() {
        return Err(Error::GitCommand {
            args: vec!["rev-parse".into(), "--git-common-dir".into()],
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let git_dir = String::from_utf8_lossy(&output.stdout).into_owned();
    let git_dir_path = PathBuf::from(git_dir.trim());

    // For worktrees, the common dir is the main repo's .git directory.
    // The repo root is the parent of .git (never panic on malformed paths).
    if git_dir_path.ends_with(".git") {
        git_dir_path
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| Error::GitCommand {
                args: vec!["rev-parse".into(), "--git-common-dir".into()],
                stderr: format!("invalid git directory path: {}", git_dir_path.display()),
            })
    } else {
        // Bare repo case
        Ok(git_dir_path)
    }
}

/// Reject symlink components of `path` that lie *under* `base`.
///
/// Components of `base` itself (and ancestors) are not checked: on macOS the
/// system temp root lives under `/var` → `/private/var`, which is a legitimate
/// OS alias, not an escape. Escape risk is owner/repo/job segments that are
/// symlinks pointing outside the sandbox.
fn reject_symlink_components_under(base: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(base)
        .map_err(|_| Error::SandboxViolation {
            base: base.to_path_buf(),
            candidate: path.to_path_buf(),
            reason: "path is not under worktree base",
        })?;

    let mut cur = base.to_path_buf();
    for comp in relative.components() {
        cur.push(comp);
        if !cur.exists() {
            continue;
        }
        let meta = fs::symlink_metadata(&cur).map_err(|e| Error::Io {
            context: "stat path component for symlink check",
            source: e,
        })?;
        if meta.file_type().is_symlink() {
            return Err(Error::SandboxViolation {
                base: base.to_path_buf(),
                candidate: cur,
                reason: "symlink component under worktree base is not allowed",
            });
        }
    }
    Ok(())
}

/// Resolve a removal target to the canonical spelling `grant` stored.
///
/// When the leaf exists we canonicalize it directly. When it is already gone we
/// cannot canonicalize the leaf, so we canonicalize the nearest existing
/// ancestor and re-attach the remaining tail. This keeps the sandbox check and
/// the `release_by_path` lookup using the same canonical form that a live
/// worktree would have produced. If no ancestor exists (nothing to anchor to),
/// fall back to the raw path so the caller's sandbox check still runs and
/// rejects it when appropriate.
fn canonicalize_removal_target(worktree_path: &Path) -> Result<PathBuf> {
    if worktree_path.exists() {
        return canonicalize_for_tools(worktree_path).map_err(|e| Error::Io {
            context: "canonicalize worktree removal target",
            source: e,
        });
    }
    let mut tail = PathBuf::new();
    let mut ancestor = worktree_path;
    loop {
        match ancestor.parent() {
            Some(parent) => {
                let leaf = ancestor
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(ancestor));
                if parent.exists() {
                    let canonical_parent =
                        canonicalize_for_tools(parent).map_err(|e| Error::Io {
                            context: "canonicalize worktree removal parent",
                            source: e,
                        })?;
                    return Ok(canonical_parent.join(leaf).join(&tail));
                }
                tail = leaf.join(&tail);
                ancestor = parent;
            }
            // No existing ancestor to anchor to; keep the raw path so the
            // sandbox containment check still runs against it.
            None => return Ok(worktree_path.to_path_buf()),
        }
    }
}

/// Best-effort prune of stale worktree administrative entries in `repo_root`.
///
/// Only call this for paths already proven to be within the sandbox base. A
/// failure is intentionally ignored: pruning stale registrations is a courtesy
/// to a later reclaim, not a correctness precondition for releasing the lease.
fn prune_stale_registration(repo_root: &Path) {
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("worktree")
        .arg("prune")
        .output();
}

/// Clean up empty parent directories up to the base.
fn cleanup_empty_parents(path: &Path, base: &Path) {
    let mut current = path.parent();
    while let Some(parent) = current {
        if parent == base {
            break;
        }
        // Only remove if empty
        if fs::read_dir(parent)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false)
        {
            let _ = fs::remove_dir(parent);
            current = parent.parent();
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn init_test_repo_with_object_format(
        dir: &Path,
        object_format: Option<&str>,
    ) -> Result<PathBuf> {
        // Initialize a git repo
        let mut init = Command::new("git");
        init.arg("-C").arg(dir).arg("init");
        if let Some(format) = object_format {
            init.arg(format!("--object-format={format}"));
        }
        init.arg("-b").arg("main").output().map_err(|e| Error::Io {
            context: "git init",
            source: e,
        })?;

        // Configure git for testing
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .map_err(|e| Error::Io {
                context: "git config email",
                source: e,
            })?;

        Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .map_err(|e| Error::Io {
                context: "git config name",
                source: e,
            })?;

        // Create initial commit
        fs::write(dir.join("README.md"), "# Test Repo\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("add")
            .arg("README.md")
            .output()
            .map_err(|e| Error::Io {
                context: "git add",
                source: e,
            })?;

        Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("commit")
            .arg("-m")
            .arg("Initial commit")
            .output()
            .map_err(|e| Error::Io {
                context: "git commit",
                source: e,
            })?;

        Ok(dir.to_path_buf())
    }

    fn git_output(repo: &Path, args: &[&str]) -> String {
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
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    struct Harness {
        temp: tempfile::TempDir,
        repo_root: PathBuf,
        manager: WorktreeManager,
    }

    impl Harness {
        fn sha1() -> Self {
            Self::with_format(None)
        }

        fn sha256() -> Self {
            Self::with_format(Some("sha256"))
        }

        fn with_format(object_format: Option<&str>) -> Self {
            let temp = tempdir().unwrap();
            let repo = temp.path().join("repo");
            fs::create_dir(&repo).unwrap();
            let repo_root = init_test_repo_with_object_format(&repo, object_format).unwrap();
            let manager = WorktreeManager::with_base(temp.path().join("worktrees")).unwrap();
            Self {
                temp,
                repo_root,
                manager,
            }
        }

        fn temp_path(&self) -> &Path {
            self.temp.path()
        }

        fn head(&self) -> String {
            git_output(&self.repo_root, &["rev-parse", "HEAD"])
        }

        fn request<'a>(
            &'a self,
            job_id: &'a str,
            branch: &'a str,
            start_point: &'a str,
        ) -> WorktreeCreateRequest<'a> {
            WorktreeCreateRequest {
                repo_root: &self.repo_root,
                owner: "acme",
                repo: "test-repo",
                job_id,
                branch,
                start_point,
            }
        }

        fn create<'a>(
            &'a self,
            job_id: &'a str,
            branch: &'a str,
            start_point: &'a str,
        ) -> Result<Worktree> {
            self.manager
                .create_with_request(self.request(job_id, branch, start_point))
        }

        fn job_path(&self, job_id: &str) -> PathBuf {
            self.manager
                .base_path()
                .unwrap()
                .join("acme/test-repo")
                .join(job_id)
        }

        fn git(&self, args: &[&str]) -> String {
            git_output(&self.repo_root, args)
        }

        fn commit_file(&self, name: &str, contents: &str, message: &str) -> String {
            fs::write(self.repo_root.join(name), contents).unwrap();
            self.git(&["add", name]);
            self.git(&["commit", "-m", message]);
            self.head()
        }
    }

    fn assert_create_rejects_start_point_without_mutation(
        harness: &Harness,
        start_point: &str,
        job_id: &str,
        branch: &str,
    ) {
        let expected_path = harness.job_path(job_id);
        let result = harness.create(job_id, branch, start_point);
        assert!(
            matches!(result, Err(Error::GitCommand { .. })),
            "expected GitCommand reject for {start_point:?}, got {result:?}"
        );
        assert!(
            git_output(&harness.repo_root, &["branch", "--list", branch])
                .trim()
                .is_empty()
        );
        assert!(!expected_path.exists());
    }

    #[test]
    fn create_and_list_worktree() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .create("job-1", "feature/test", &start_commit)
            .unwrap();

        assert!(wt.path.exists());
        assert_eq!(wt.branch, "feature/test");
        assert_eq!(wt.start_commit.as_deref(), Some(start_commit.as_str()));

        let listed = harness.manager.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, wt.path);
    }

    #[test]
    fn positional_create_api_remains_compatible() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .manager
            .create(
                &harness.repo_root,
                "acme",
                "test-repo",
                "job-positional",
                "feature/positional",
                &start_commit,
            )
            .unwrap();

        assert_eq!(wt.head_commit.as_deref(), Some(start_commit.as_str()));
    }

    fn assert_unproven_resume(result: Result<Worktree>) {
        assert!(matches!(
            result,
            Err(Error::PolicyViolation {
                code: PolicyCode::WorktreeResumeUnproven,
                ..
            })
        ));
    }

    #[test]
    fn create_rejects_existing_branch_without_resume_identity() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        harness.git(&["branch", "existing-branch"]);
        let result = harness.create("job-2", "existing-branch", &start_commit);
        assert_unproven_resume(result);
        assert!(!harness.job_path("job-2").exists());
    }

    #[test]
    fn create_rejects_branch_checked_out_in_another_worktree() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let other = harness.temp_path().join("other-worktree");
        harness.git(&[
            "worktree",
            "add",
            "-b",
            "feature/elsewhere",
            "--",
            other.to_str().unwrap(),
            &start_commit,
        ]);
        let result = harness.create("job-elsewhere", "feature/elsewhere", &start_commit);
        assert_unproven_resume(result);
        assert_eq!(
            git_output(&other, &["rev-parse", "HEAD"]),
            start_commit,
            "the unrelated checked-out branch must remain untouched"
        );
    }

    #[test]
    fn create_uses_resolved_start_commit_not_ambient_head() {
        let harness = Harness::sha1();
        let requested_commit = harness.head();
        let ambient_head =
            harness.commit_file("later.txt", "ambient head only\n", "advance ambient HEAD");
        assert_ne!(requested_commit, ambient_head);
        let wt = harness
            .create("job-exact", "feature/exact", &requested_commit)
            .unwrap();
        assert_eq!(wt.start_commit.as_deref(), Some(requested_commit.as_str()));
        assert_eq!(wt.head_commit.as_deref(), Some(requested_commit.as_str()));
        assert_eq!(
            git_output(&wt.path, &["rev-parse", "HEAD"]),
            requested_commit
        );
        assert!(!wt.path.join("later.txt").exists());
    }

    #[test]
    fn create_verifies_full_branch_ref_when_same_named_tag_exists() {
        let harness = Harness::sha1();
        let requested_commit = harness.head();
        harness.git(&["tag", "feature/collision", &requested_commit]);
        let wt = harness
            .create("job-collision", "feature/collision", &requested_commit)
            .unwrap();
        assert_eq!(
            git_output(&wt.path, &["symbolic-ref", "--quiet", "HEAD"]),
            "refs/heads/feature/collision"
        );
        assert_eq!(wt.head_commit.as_deref(), Some(requested_commit.as_str()));
    }

    #[test]
    fn postconditions_reject_moved_branch_and_preserve_residual_state() {
        let harness = Harness::sha1();
        let requested_commit = harness.head();
        let wt = harness
            .create(
                "job-postcondition",
                "feature/postcondition",
                &requested_commit,
            )
            .unwrap();
        let moved_commit = harness.commit_file("moved.txt", "moved\n", "moved branch target");
        harness.git(&[
            "update-ref",
            "refs/heads/feature/postcondition",
            &moved_commit,
        ]);
        let result = verify_creation_postconditions(CreationPostconditions {
            repo_root: &harness.repo_root,
            worktree_path: &wt.path,
            expected_branch: BranchName("feature/postcondition"),
            expected_commit: CommitId(&requested_commit),
        });
        assert!(matches!(result, Err(Error::WorktreePostconditionFailed(_))));
        assert_eq!(
            harness.git(&["rev-parse", "refs/heads/feature/postcondition"]),
            moved_commit
        );
        assert!(wt.path.exists());
    }

    #[test]
    fn registration_identity_requires_matching_path_branch_and_head() {
        let path = Path::new("/tmp/hive/job");
        let expected_head = "a".repeat(40);
        let correct = format!(
            "worktree /tmp/hive/job\nHEAD {expected_head}\nbranch refs/heads/feature/job\n\n"
        );
        let wrong_branch = format!(
            "worktree /tmp/hive/job\nHEAD {expected_head}\nbranch refs/heads/feature/other\n\n"
        );

        assert!(worktree_registration_matches(
            PorcelainListing(&correct),
            path,
            BranchRef("refs/heads/feature/job"),
            CommitId(&expected_head),
        ));
        assert!(!worktree_registration_matches(
            PorcelainListing(&wrong_branch),
            path,
            BranchRef("refs/heads/feature/job"),
            CommitId(&expected_head),
        ));
    }

    #[test]
    fn create_rejects_invalid_start_point_without_creating_branch() {
        let harness = Harness::sha1();
        assert_create_rejects_start_point_without_mutation(
            &harness,
            "refs/heads/does-not-exist",
            "job-invalid",
            "feature/invalid",
        );
        let branch_check = Command::new("git")
            .arg("-C")
            .arg(&harness.repo_root)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/feature/invalid",
            ])
            .status()
            .unwrap();
        assert!(!branch_check.success());
    }

    #[test]
    fn create_rejects_abbreviated_all_hex_start_point_without_mutation() {
        let harness = Harness::sha1();
        let abbreviated = harness.head()[..12].to_owned();
        assert_create_rejects_start_point_without_mutation(
            &harness,
            &abbreviated,
            "job-abbreviated",
            "feature/abbreviated",
        );
    }

    #[test]
    fn create_accepts_uppercase_full_object_id_and_returns_canonical_lowercase() {
        let harness = Harness::sha1();
        let full_commit = harness.head();
        let uppercase_commit = full_commit.to_ascii_uppercase();
        let wt = harness
            .create("job-uppercase", "feature/uppercase", &uppercase_commit)
            .unwrap();

        assert_eq!(wt.start_commit.as_deref(), Some(full_commit.as_str()));
        assert_eq!(wt.head_commit.as_deref(), Some(full_commit.as_str()));
    }

    #[test]
    fn create_accepts_uppercase_full_sha256_and_returns_canonical_lowercase() {
        let harness = Harness::sha256();
        let full_commit = harness.head();
        assert_eq!(full_commit.len(), 64);
        let uppercase_commit = full_commit.to_ascii_uppercase();
        let wt = harness
            .create(
                "job-uppercase-sha256",
                "feature/uppercase-sha256",
                &uppercase_commit,
            )
            .unwrap();

        assert_eq!(wt.start_commit.as_deref(), Some(full_commit.as_str()));
        assert_eq!(wt.head_commit.as_deref(), Some(full_commit.as_str()));
    }

    #[test]
    fn create_rejects_sha256_prefix_with_sha1_width_without_mutation() {
        let harness = Harness::sha256();
        let full_commit = harness.head();
        assert_eq!(full_commit.len(), 64);
        let abbreviated = full_commit[..40].to_owned();
        let expected_path = harness.job_path("job-sha256-prefix");
        assert_create_rejects_start_point_without_mutation(
            &harness,
            &abbreviated,
            "job-sha256-prefix",
            "feature/sha256-prefix",
        );
        assert!(
            !git_output(&harness.repo_root, &["worktree", "list", "--porcelain"])
                .contains(&expected_path.to_string_lossy().to_string())
        );
    }

    #[test]
    fn create_rejects_decorated_abbreviated_hex_start_points_without_mutation() {
        let harness = Harness::sha1();
        let abbreviated = harness.head()[..12].to_owned();

        for (suffix, job_id, branch) in [
            ("~0", "job-abbrev-tilde", "feature/abbrev-tilde"),
            ("^0", "job-abbrev-caret", "feature/abbrev-caret"),
            ("^{commit}", "job-abbrev-peel", "feature/abbrev-peel"),
        ] {
            let start_point = format!("{abbreviated}{suffix}");
            assert_create_rejects_start_point_without_mutation(
                &harness,
                &start_point,
                job_id,
                branch,
            );
        }
    }

    #[test]
    fn create_rejects_sha256_prefix_with_tilde_zero_without_mutation() {
        let harness = Harness::sha256();
        let full_commit = harness.head();
        assert_eq!(full_commit.len(), 64);
        let decorated = format!("{}~0", &full_commit[..40]);
        assert_create_rejects_start_point_without_mutation(
            &harness,
            &decorated,
            "job-sha256-prefix-tilde",
            "feature/sha256-prefix-tilde",
        );
    }

    #[test]
    fn create_accepts_symbolic_ref_and_full_object_id() {
        let harness = Harness::sha1();
        let full_commit = harness.head();
        let from_ref = harness
            .create("job-symbolic", "feature/symbolic", "refs/heads/main")
            .unwrap();
        assert_eq!(from_ref.start_commit.as_deref(), Some(full_commit.as_str()));

        let from_oid = harness
            .create("job-full-oid", "feature/full-oid", &full_commit)
            .unwrap();
        assert_eq!(from_oid.start_commit.as_deref(), Some(full_commit.as_str()));
    }

    #[test]
    fn create_failure_reports_and_preserves_residual_branch() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let target = harness.job_path("job-collision");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("occupied"), "force worktree add failure\n").unwrap();

        let result = harness.create("job-collision", "feature/rollback", &start_commit);
        assert!(
            matches!(
                result,
                Err(Error::WorktreeCreationFailed(ref failure))
                    if failure.branch_commit.as_ref() == Some(&start_commit)
                        && !failure.worktree_registered
            ),
            "unexpected result: {result:?}"
        );
        assert_eq!(
            harness.git(&["rev-parse", "refs/heads/feature/rollback"]),
            start_commit
        );
        let adopted_commit = harness.commit_file(
            "adopted.txt",
            "adopted after failure\n",
            "adopt residual branch",
        );
        harness.git(&["update-ref", "refs/heads/feature/rollback", &adopted_commit]);
        assert_eq!(
            harness.git(&["rev-parse", "refs/heads/feature/rollback"]),
            adopted_commit,
            "no delayed cleanup may delete a residual branch adopted after failure"
        );
        assert_eq!(
            fs::read_to_string(target.join("occupied")).unwrap(),
            "force worktree add failure\n"
        );
    }

    #[test]
    fn remove_worktree() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .create("job-3", "feature/remove", &start_commit)
            .unwrap();
        assert!(wt.path.exists());
        harness.manager.remove(&wt.path, false).unwrap();
        assert!(!wt.path.exists());
    }

    #[test]
    fn remove_via_non_canonical_path_still_releases_lease() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .create("job-noncanon", "feature/noncanon", &start_commit)
            .unwrap();
        assert!(wt.path.exists());

        // Spell the same worktree path with a redundant `.` segment so it is
        // not byte-identical to the canonical path stored by `grant`.
        let noncanonical = wt.path.join(".");
        harness.manager.remove(&noncanonical, false).unwrap();
        assert!(!wt.path.exists());

        let lease = harness
            .manager
            .lease_store()
            .unwrap()
            .find_resume(ResumeKey {
                owner: "acme",
                repo_name: "test-repo",
                job_id: "job-noncanon",
                branch: "feature/noncanon",
            })
            .unwrap()
            .unwrap();
        assert_eq!(lease.mode, LeaseMode::Unassigned);
        assert!(lease.released_at.is_some());
    }

    #[test]
    fn remove_out_of_sandbox_path_is_rejected_and_releases_no_lease() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        // Grant a lease for an in-sandbox job so we can prove it stays held.
        let wt = harness
            .create("job-guarded", "feature/guarded", &start_commit)
            .unwrap();
        assert!(wt.path.exists());

        // A path outside the configured base must be rejected up front.
        let outside = harness.temp_path().join("outside-sandbox");
        fs::create_dir_all(&outside).unwrap();
        let result = harness.manager.remove(&outside, false);
        assert!(
            matches!(result, Err(Error::SandboxViolation { .. })),
            "expected SandboxViolation, got {result:?}"
        );

        // The in-sandbox lease must remain held (not released by the foreign path).
        let lease = harness
            .manager
            .lease_store()
            .unwrap()
            .find_resume(ResumeKey {
                owner: "acme",
                repo_name: "test-repo",
                job_id: "job-guarded",
                branch: "feature/guarded",
            })
            .unwrap()
            .unwrap();
        assert_eq!(lease.mode, LeaseMode::WriterLocked);
        assert!(lease.released_at.is_none());
    }

    #[test]
    fn remove_out_of_sandbox_nonexistent_path_is_rejected_before_release() {
        let harness = Harness::sha1();
        // A non-existent out-of-sandbox path must still be rejected (the
        // containment check runs before the release fast path).
        let outside = harness.temp_path().join("gone-outside/leaf");
        let result = harness.manager.remove(&outside, false);
        assert!(
            matches!(result, Err(Error::SandboxViolation { .. })),
            "expected SandboxViolation, got {result:?}"
        );
    }

    #[test]
    fn reject_path_outside_sandbox() {
        let harness = Harness::sha1();
        let result = harness.create("../escape", "branch", "HEAD");
        assert!(result.is_err());
    }

    #[test]
    fn prune_worktrees() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .create("job-4", "feature/prune", &start_commit)
            .unwrap();
        fs::remove_dir_all(&wt.path).unwrap();
        harness.manager.prune(&harness.repo_root).unwrap();
    }

    #[test]
    fn reject_symlink_owner_under_base() {
        let harness = Harness::sha1();
        let outside = harness.temp_path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let owner_link = harness.manager.base_path().unwrap().join("acme");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &owner_link).unwrap();
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(&outside, &owner_link).unwrap();
        }

        let result = harness.create("job-sym", "branch-sym", "HEAD");
        assert!(
            matches!(result, Err(Error::SandboxViolation { .. })),
            "expected SandboxViolation, got {result:?}"
        );
    }

    #[test]
    fn prune_on_a_non_repository_errors_rather_than_acting() {
        let harness = Harness::sha1();
        let not_a_repo = harness.temp.path().join("not-a-repo");
        fs::create_dir_all(&not_a_repo).unwrap();

        let err = harness.manager.prune(&not_a_repo).unwrap_err();
        match err {
            Error::GitCommand { args, stderr } => {
                assert_eq!(args, vec!["worktree".to_owned(), "prune".to_owned()]);
                assert!(stderr.contains("not a git repository"), "stderr: {stderr}");
            }
            other => panic!("expected GitCommand, got {other:?}"),
        }
    }

    #[test]
    fn prune_does_not_touch_a_directory_it_rejects() {
        let harness = Harness::sha1();
        let not_a_repo = harness.temp.path().join("bystander");
        fs::create_dir_all(&not_a_repo).unwrap();
        let canary = not_a_repo.join("keep-me");
        fs::write(&canary, b"untouched").unwrap();

        assert!(harness.manager.prune(&not_a_repo).is_err());

        // The point of this lock: a rejected prune leaves the directory alone.
        // It passes today without any guard in `prune`, because git refuses
        // first -- that is the evidence the guard would be redundant.
        assert!(canary.exists());
        assert_eq!(fs::read(&canary).unwrap(), b"untouched");
        assert!(!not_a_repo.join(".git").exists());
    }

    #[test]
    fn prune_succeeds_on_a_real_repository() {
        let harness = Harness::sha1();
        harness.manager.prune(&harness.repo_root).unwrap();
    }

    #[test]
    fn create_remove_reclaim_same_start_commit() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let first = harness
            .create("job-reclaim", "feature/reclaim", &start_commit)
            .unwrap();
        harness.manager.remove(&first.path, false).unwrap();
        assert!(!first.path.exists());
        assert_eq!(
            harness.git(&["rev-parse", "refs/heads/feature/reclaim"]),
            start_commit
        );

        let second = harness
            .create("job-reclaim", "feature/reclaim", &start_commit)
            .unwrap();
        assert_eq!(second.start_commit.as_deref(), Some(start_commit.as_str()));
        assert_eq!(second.head_commit.as_deref(), Some(start_commit.as_str()));
        assert_eq!(
            git_output(&second.path, &["symbolic-ref", "--quiet", "HEAD"]),
            "refs/heads/feature/reclaim"
        );
        let lease = harness
            .manager
            .lease_store()
            .unwrap()
            .find_resume(ResumeKey {
                owner: "acme",
                repo_name: "test-repo",
                job_id: "job-reclaim",
                branch: "feature/reclaim",
            })
            .unwrap()
            .unwrap();
        assert_eq!(lease.mode, crate::lease::LeaseMode::WriterLocked);
        assert!(lease.released_at.is_none());
    }

    #[test]
    fn worktree_create_reuse_without_remove_succeeds() {
        // Claude Code WorktreeCreate on an already-present verified worktree
        // must return success (exit 0) rather than aborting session setup.
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let first = harness
            .create("job-reuse", "feature/reuse", &start_commit)
            .unwrap();
        let second = harness
            .create("job-reuse", "feature/reuse", &start_commit)
            .unwrap();
        assert_eq!(second.path, first.path);
        assert_eq!(second.head_commit.as_deref(), Some(start_commit.as_str()));
        assert!(first.path.exists());
        // WorktreeCreate treats Ok as exit 0 and does not abort Claude Code.
        assert_eq!(
            harness
                .create("job-reuse", "feature/reuse", &start_commit)
                .map(|_| 0u8)
                .unwrap_or(2),
            0
        );
    }

    #[test]
    fn reclaim_rejects_branch_checked_out_elsewhere() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .create(
                "job-elsewhere-resume",
                "feature/elsewhere-resume",
                &start_commit,
            )
            .unwrap();
        harness.manager.remove(&wt.path, false).unwrap();
        let other = harness.temp_path().join("other-worktree");
        harness.git(&[
            "worktree",
            "add",
            "--",
            other.to_str().unwrap(),
            "feature/elsewhere-resume",
        ]);
        let result = harness.create(
            "job-elsewhere-resume",
            "feature/elsewhere-resume",
            &start_commit,
        );
        assert_unproven_resume(result);
        assert_eq!(git_output(&other, &["rev-parse", "HEAD"]), start_commit);
        assert!(!harness.job_path("job-elsewhere-resume").exists());
    }

    #[test]
    fn reclaim_rejects_moved_branch_after_remove() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .create("job-moved", "feature/moved", &start_commit)
            .unwrap();
        harness.manager.remove(&wt.path, false).unwrap();
        let moved = harness.commit_file("moved.txt", "moved\n", "move branch");
        harness.git(&["update-ref", "refs/heads/feature/moved", &moved]);
        let result = harness.create("job-moved", "feature/moved", &start_commit);
        match result {
            Err(Error::PolicyViolation {
                code: PolicyCode::WorktreeResumeUnproven,
                message,
            }) => {
                assert!(message.contains("branch moved"), "{message}");
                assert!(message.contains("residual_state"), "{message}");
                assert!(
                    message.contains("branch_ref=refs/heads/feature/moved"),
                    "{message}"
                );
                assert!(message.contains("ownership_evidence="), "{message}");
                assert!(message.contains("automatic cleanup skipped"), "{message}");
            }
            other => panic!("expected unproven resume, got {other:?}"),
        }
        assert_eq!(
            harness.git(&["rev-parse", "refs/heads/feature/moved"]),
            moved
        );
        assert!(!harness.job_path("job-moved").exists());
    }

    #[test]
    fn reclaim_rejects_foreign_job_adopting_the_same_branch() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .create("job-owned", "feature/shared", &start_commit)
            .unwrap();
        harness.manager.remove(&wt.path, false).unwrap();
        let result = harness.create("job-other", "feature/shared", &start_commit);
        assert_unproven_resume(result);
        assert_eq!(
            harness.git(&["rev-parse", "refs/heads/feature/shared"]),
            start_commit,
            "a foreign job must not delete or adopt the owned branch"
        );
        assert!(!harness.job_path("job-other").exists());
    }

    #[test]
    fn reclaim_rejects_unexpected_upstream() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let wt = harness
            .create("job-upstream", "feature/upstream", &start_commit)
            .unwrap();
        harness.manager.remove(&wt.path, false).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&harness.repo_root)
            .args(["config", "branch.feature/upstream.remote", "."])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&harness.repo_root)
            .args(["config", "branch.feature/upstream.merge", "refs/heads/main"])
            .status()
            .unwrap();
        let result = harness.create("job-upstream", "feature/upstream", &start_commit);
        match result {
            Err(Error::PolicyViolation {
                code: PolicyCode::WorktreeResumeUnproven,
                message,
            }) => {
                assert!(message.contains("upstream"), "{message}");
                assert!(message.contains("residual_state"), "{message}");
                assert!(
                    message.contains("branch_ref=refs/heads/feature/upstream"),
                    "{message}"
                );
                assert!(message.contains("ownership_evidence="), "{message}");
            }
            other => panic!("expected unproven resume, got {other:?}"),
        }
        assert_eq!(
            harness.git(&["rev-parse", "refs/heads/feature/upstream"]),
            start_commit
        );
    }

    /// Configure `branch` to track `refs/remotes/<remote>/<branch>` and seed a
    /// local remote-tracking ref at `remote_commit`, mirroring the local state
    /// a prior `git fetch` would have recorded (no network required).
    ///
    /// The remote is wired to the repo itself with a standard fetch refspec so
    /// `branch@{upstream}` resolves to the remote-tracking ref name
    /// (`<remote>/<branch>`), exactly as it would after a real fetch.
    fn set_local_tracking_upstream(
        harness: &Harness,
        remote: &str,
        branch: &str,
        remote_commit: &str,
    ) {
        harness.git(&["config", &format!("remote.{remote}.url"), "."]);
        harness.git(&[
            "config",
            &format!("remote.{remote}.fetch"),
            &format!("+refs/heads/*:refs/remotes/{remote}/*"),
        ]);
        harness.git(&[
            "update-ref",
            &format!("refs/remotes/{remote}/{branch}"),
            remote_commit,
        ]);
        harness.git(&["config", &format!("branch.{branch}.remote"), remote]);
        harness.git(&[
            "config",
            &format!("branch.{branch}.merge"),
            &format!("refs/heads/{branch}"),
        ]);
    }

    #[test]
    fn reclaim_rejects_branch_behind_its_upstream() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let branch = "feature/behind";
        let wt = harness.create("job-behind", branch, &start_commit).unwrap();
        harness.manager.remove(&wt.path, false).unwrap();
        // The recorded remote-tracking commit is ahead of the local branch, so
        // the local branch is behind its upstream.
        let ahead_commit =
            harness.commit_file("ahead.txt", "remote ahead\n", "advance remote-tracking ref");
        assert_ne!(ahead_commit, start_commit);
        set_local_tracking_upstream(&harness, "origin", branch, &ahead_commit);

        let result = harness.create("job-behind", branch, &start_commit);
        match result {
            Err(Error::PolicyViolation {
                code: PolicyCode::WorktreeResumeUnproven,
                message,
            }) => {
                assert!(message.contains("behind its upstream"), "{message}");
                assert!(message.contains("ownership_evidence="), "{message}");
            }
            other => panic!("expected unproven resume, got {other:?}"),
        }
        assert_eq!(
            harness.git(&["rev-parse", &format!("refs/heads/{branch}")]),
            start_commit
        );
    }

    #[test]
    fn reclaim_rejects_branch_divergent_from_its_upstream() {
        let harness = Harness::sha1();
        let base_commit = harness.head();
        let branch = "feature/divergent";
        // Build a remote-only commit on a sibling line off the shared base so
        // it is neither an ancestor nor a descendant of the local branch tip.
        harness.git(&["checkout", "-b", "tmp-remote", &base_commit]);
        let remote_commit =
            harness.commit_file("remote.txt", "remote-only line\n", "remote-only commit");
        harness.git(&["checkout", "main"]);
        harness.git(&["branch", "-D", "tmp-remote"]);
        // The local branch advances on its OWN line off the same base, so local
        // and remote each have commits the other lacks -> divergent.
        let local_commit =
            harness.commit_file("local.txt", "local-only line\n", "local-only commit");
        assert_ne!(local_commit, remote_commit);
        let wt = harness
            .create("job-divergent", branch, &local_commit)
            .unwrap();
        harness.manager.remove(&wt.path, false).unwrap();
        set_local_tracking_upstream(&harness, "origin", branch, &remote_commit);

        let result = harness.create("job-divergent", branch, &local_commit);
        match result {
            Err(Error::PolicyViolation {
                code: PolicyCode::WorktreeResumeUnproven,
                message,
            }) => {
                assert!(message.contains("diverged from its upstream"), "{message}");
            }
            other => panic!("expected unproven resume, got {other:?}"),
        }
    }

    #[test]
    fn reclaim_succeeds_when_branch_is_ahead_of_its_upstream() {
        let harness = Harness::sha1();
        // The remote-tracking ref stays at the original commit; the local
        // branch advances ahead of it by a job-owned commit. This is the
        // reclaimable published-branch case per AGENTS.md.
        let base_commit = harness.head();
        let branch = "feature/ahead";
        // Advance the working history, then create the branch at the newer tip.
        let ahead_commit = harness.commit_file("ahead.txt", "job work\n", "job-owned commit");
        assert_ne!(ahead_commit, base_commit);
        let wt = harness.create("job-ahead", branch, &ahead_commit).unwrap();
        harness.manager.remove(&wt.path, false).unwrap();
        // Upstream recorded at the older base commit -> local is strictly ahead.
        set_local_tracking_upstream(&harness, "origin", branch, &base_commit);

        let second = harness.create("job-ahead", branch, &ahead_commit).unwrap();
        assert_eq!(second.start_commit.as_deref(), Some(ahead_commit.as_str()));
        assert_eq!(second.head_commit.as_deref(), Some(ahead_commit.as_str()));
    }

    #[test]
    fn reclaim_succeeds_when_branch_equals_its_upstream() {
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let branch = "feature/equal";
        let wt = harness.create("job-equal", branch, &start_commit).unwrap();
        harness.manager.remove(&wt.path, false).unwrap();
        // Upstream equals the local branch commit -> reclaimable.
        set_local_tracking_upstream(&harness, "origin", branch, &start_commit);

        let second = harness.create("job-equal", branch, &start_commit).unwrap();
        assert_eq!(second.start_commit.as_deref(), Some(start_commit.as_str()));
    }

    #[test]
    fn fresh_create_persists_no_lease_for_a_job_that_never_won_add_b() {
        // A Fresh create must not leave an ownership-proving lease for a job
        // that did not win `git worktree add -b`. Simulate a loser: job B tries
        // to Fresh-create a branch that job A already created. `add -b` fails
        // for B (ref exists), and B must have no lease afterwards.
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let branch = "feature/race";
        // Job A wins the ref creation.
        harness.create("job-a", branch, &start_commit).unwrap();

        // Job B never removed/owned the branch, so it takes the Reclaim path
        // and is rejected for lack of a proven lease. Either way, B must own no
        // lease that could later prove ownership.
        let result = harness.create("job-b", branch, &start_commit);
        assert_unproven_resume(result);

        let lease_b = harness
            .manager
            .lease_store()
            .unwrap()
            .find_resume(ResumeKey {
                owner: "acme",
                repo_name: "test-repo",
                job_id: "job-b",
                branch,
            })
            .unwrap();
        assert!(
            lease_b.is_none(),
            "a job that never won `add -b` must hold no lease, got {lease_b:?}"
        );
    }

    #[test]
    fn fresh_create_failure_leaves_no_ownership_proving_lease() {
        // If `git worktree add -b` fails on the Fresh path, no lease row may be
        // left that could later prove ownership of the branch.
        let harness = Harness::sha1();
        let start_commit = harness.head();
        let branch = "feature/failed-fresh";
        // Occupy the target path so `git worktree add` fails.
        let target = harness.job_path("job-fail");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("occupied"), "force add failure\n").unwrap();

        let result = harness.create("job-fail", branch, &start_commit);
        assert!(
            matches!(result, Err(Error::WorktreeCreationFailed(_))),
            "expected creation failure, got {result:?}"
        );

        let lease = harness
            .manager
            .lease_store()
            .unwrap()
            .find_resume(ResumeKey {
                owner: "acme",
                repo_name: "test-repo",
                job_id: "job-fail",
                branch,
            })
            .unwrap();
        assert!(
            lease.is_none(),
            "a failed Fresh create must leave no lease, got {lease:?}"
        );
    }

    #[test]
    fn list_does_not_create_the_lease_store() {
        // `worktree list` is read-only and must not open (or create) the
        // writable lease store.
        let temp = tempdir().unwrap();
        let base = temp.path().join("worktrees");
        let manager = WorktreeManager::with_base(base.clone()).unwrap();
        let leases_db = manager.base_path().unwrap().join("leases.db");
        assert!(!leases_db.exists());

        let listed = manager.list().unwrap();
        assert!(listed.is_empty());
        assert!(
            !leases_db.exists(),
            "listing must not create the lease store at {}",
            leases_db.display()
        );
    }

    #[test]
    fn prune_does_not_create_the_lease_store() {
        // `worktree prune` is read-only over git state and must not open (or
        // create) the writable lease store.
        let harness = Harness::sha1();
        let leases_db = harness.manager.base_path().unwrap().join("leases.db");
        assert!(!leases_db.exists());

        harness.manager.prune(&harness.repo_root).unwrap();
        assert!(
            !leases_db.exists(),
            "pruning must not create the lease store at {}",
            leases_db.display()
        );
    }

    #[test]
    fn tracking_ref_matches_exact_remote_branch_not_nested_suffix() {
        assert!(tracking_ref_matches_branch(
            "origin/hive/gh-42",
            "hive/gh-42"
        ));
        assert!(tracking_ref_matches_branch("hive/gh-42", "hive/gh-42"));
        assert!(!tracking_ref_matches_branch(
            "origin/evil/hive/gh-42",
            "hive/gh-42"
        ));
        assert!(!tracking_ref_matches_branch("origin/main", "hive/gh-42"));
    }
}
