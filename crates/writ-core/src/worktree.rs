use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{
    Error, PolicyCode, Result, WorktreeCreationFailure, WorktreePostconditionFailure,
};
use crate::identity::{
    BranchName, BranchRef, CommitId, JobId, Owner, Repo, StartPoint, resolve_start_commit,
};
use crate::paths::{canonicalize_for_tools, derive_worktree_path, worktree_base_path};

#[derive(Clone, Copy)]
struct GitArgList<'a>(&'a [&'a str]);

#[derive(Clone, Copy)]
struct IoContext(&'static str);

#[derive(Clone, Copy)]
struct PorcelainListing<'a>(&'a str);

#[derive(Clone, Copy)]
struct PostconditionCause<'a>(&'a str);

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
#[derive(Debug, Default)]
pub struct WorktreeManager {
    base_path: Option<PathBuf>,
}

impl WorktreeManager {
    /// Create a new manager using the default base path.
    pub fn new() -> Result<Self> {
        let base = worktree_base_path()?;
        Self::with_base(base)
    }

    /// Create a new manager with an explicit base path (for testing or overrides).
    ///
    /// The base is created if missing and stored in canonical form so OS path
    /// aliases (e.g. macOS `/var` → `/private/var`) do not trip sandbox checks.
    pub fn with_base(base: PathBuf) -> Result<Self> {
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
        Ok(Self {
            base_path: Some(base),
        })
    }

    /// Get the base path this manager uses.
    pub fn base_path(&self) -> Result<&Path> {
        self.base_path.as_deref().ok_or_else(|| Error::Io {
            context: "worktree base path not initialized",
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "base path not set"),
        })
    }

    /// Create a new worktree for the given job.
    ///
    /// The worktree path will be: `{base}/{owner}/{repo}/{job_id}`.
    /// `start_point` is required and is resolved to a commit before any branch
    /// mutation. Existing branches are rejected because the exact-base contract
    /// does not define a durable resume identity for an existing ref.
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
        reject_unproven_resume(&request, CommitId(&start_commit))?;
        add_worktree(&request, &worktree_path, CommitId(&start_commit))?;

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
    /// The associated branch is NOT deleted by default.
    pub fn remove(&self, worktree_path: &Path, force: bool) -> Result<()> {
        // Verify the path is within our sandbox
        let base = self.base_path()?;
        if !is_within_base(worktree_path, base)? {
            return Err(Error::SandboxViolation {
                base: base.to_path_buf(),
                candidate: worktree_path.to_path_buf(),
                reason: "worktree path is outside configured base",
            });
        }

        // Find the repo root for this worktree
        let repo_root = find_repo_root_for_worktree(worktree_path)?;

        let mut args = vec!["worktree".into(), "remove".into()];
        if force {
            args.push("--force".into());
        }
        args.push("--".into());
        args.push(worktree_path.to_string_lossy().to_string());

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
        cleanup_empty_parents(worktree_path, base);

        Ok(())
    }

    /// Prune worktree administrative files (stale entries).
    pub fn prune(&self, repo_root: &Path) -> Result<()> {
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

fn reject_unproven_resume(
    request: &WorktreeCreateRequest<'_>,
    start_commit: CommitId<'_>,
) -> Result<()> {
    let branch = BranchName(request.branch);
    if branch_exists_in_repo(request.repo_root, branch)? {
        return Err(Error::PolicyViolation {
            code: PolicyCode::WorktreeResumeUnproven,
            message: format!(
                "refusing to reuse existing branch {:?} at requested commit \
                 {}: safe resume identity is not proven",
                branch.as_str(),
                start_commit.as_str()
            ),
        });
    }
    Ok(())
}

fn add_worktree(
    request: &WorktreeCreateRequest<'_>,
    worktree_path: &Path,
    start_commit: CommitId<'_>,
) -> Result<()> {
    let branch = BranchName(request.branch);
    // Create the branch and linked worktree in one operation. If a concurrent
    // actor creates the ref first, Git fails rather than attaching to it.
    let output = Command::new("git")
        .arg("-C")
        .arg(request.repo_root)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(branch.as_str())
        .arg("--")
        .arg(worktree_path)
        .arg(start_commit.as_str())
        .output()
        .map_err(|e| Error::Io {
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

/// Check if a path is within the base directory (sandbox check).
fn is_within_base(path: &Path, base: &Path) -> Result<bool> {
    let canonical_path = canonicalize_for_tools(path).map_err(|e| Error::Io {
        context: "canonicalize candidate path",
        source: e,
    })?;
    let canonical_base = canonicalize_for_tools(base).map_err(|e| Error::Io {
        context: "canonicalize base path",
        source: e,
    })?;

    Ok(canonical_path.starts_with(&canonical_base))
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
}
