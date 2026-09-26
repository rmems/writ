//! Shared error definitions.

use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

/// Result alias for core operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Residual state captured after a failed atomic worktree-add transaction.
#[derive(Debug)]
pub struct WorktreeCreationFailure {
    pub path: PathBuf,
    pub branch: String,
    pub path_exists: bool,
    pub branch_commit: Option<String>,
    pub head_commit: Option<String>,
    pub worktree_registered: bool,
    pub stderr: String,
}

impl Display for WorktreeCreationFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "worktree creation failed for branch `{}` at `{}`: {}; {}",
            self.branch,
            self.path.display(),
            self.stderr.trim(),
            GitResidual::from(self),
        )
    }
}

/// Exact-identity postcondition failure and the residual state left in place.
#[derive(Debug)]
pub struct WorktreePostconditionFailure {
    pub path: PathBuf,
    pub branch: String,
    pub expected_commit: String,
    pub actual_branch: Option<String>,
    pub path_exists: bool,
    pub branch_commit: Option<String>,
    pub head_commit: Option<String>,
    pub worktree_registered: bool,
    pub reason: String,
}

/// Fail-closed interrupted registration that must not be cleaned up automatically.
#[derive(Debug)]
pub struct LeaseAttentionFailure {
    pub operation_id: String,
    pub allocation_state: String,
    pub classification: String,
    pub conflicts: Vec<String>,
    pub path: PathBuf,
    pub path_exists: bool,
    pub branch_commit: Option<String>,
    pub head_commit: Option<String>,
    pub worktree_registered: bool,
}

impl Display for LeaseAttentionFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let residual = GitResidual::from(self);
        write!(
            f,
            "lease allocation `{}` needs attention (state={} class={}): {}; {residual}",
            self.operation_id,
            self.allocation_state,
            self.classification,
            self.conflicts.join("; "),
        )
    }
}

/// Residual state after a failed exact-object PR-head import.
#[derive(Debug)]
pub struct PrImportFailure {
    pub expected_commit: String,
    pub source_remote: String,
    pub source_ref: String,
    pub import_ref: String,
    pub imported_commit: Option<String>,
    pub import_ref_exists: bool,
    pub cleanup_performed: bool,
    pub reason: String,
    pub stderr: String,
}

impl Display for PrImportFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PR head import failed for `{}` from `{}/{}`: {}; residual_state \
             import_ref_exists={} imported_commit={} cleanup_performed={}; automatic cleanup \
             skipped because concurrent adoption cannot be disproven",
            self.expected_commit,
            self.source_remote,
            self.source_ref,
            self.reason.trim(),
            self.import_ref_exists,
            self.imported_commit.as_deref().unwrap_or("<absent>"),
            self.cleanup_performed
        )
    }
}

impl Display for WorktreePostconditionFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "worktree postcondition failed for branch `{}` at `{}`: expected_commit={} \
             actual_branch={} reason={}; {}",
            self.branch,
            self.path.display(),
            self.expected_commit,
            self.actual_branch.as_deref().unwrap_or("<unavailable>"),
            self.reason,
            GitResidual::from(self),
        )
    }
}

struct GitResidual {
    path_exists: bool,
    registered: bool,
    branch_commit: String,
    head_commit: String,
}

impl GitResidual {
    fn new(
        path_exists: bool,
        registered: bool,
        branch_commit: Option<&str>,
        head_commit: Option<&str>,
    ) -> Self {
        Self {
            path_exists,
            registered,
            branch_commit: branch_commit.unwrap_or("<absent>").to_owned(),
            head_commit: head_commit.unwrap_or("<absent>").to_owned(),
        }
    }
}

impl From<&WorktreeCreationFailure> for GitResidual {
    fn from(failure: &WorktreeCreationFailure) -> Self {
        Self::new(
            failure.path_exists,
            failure.worktree_registered,
            failure.branch_commit.as_deref(),
            failure.head_commit.as_deref(),
        )
    }
}

impl From<&LeaseAttentionFailure> for GitResidual {
    fn from(failure: &LeaseAttentionFailure) -> Self {
        Self::new(
            failure.path_exists,
            failure.worktree_registered,
            failure.branch_commit.as_deref(),
            failure.head_commit.as_deref(),
        )
    }
}

impl From<&WorktreePostconditionFailure> for GitResidual {
    fn from(failure: &WorktreePostconditionFailure) -> Self {
        Self::new(
            failure.path_exists,
            failure.worktree_registered,
            failure.branch_commit.as_deref(),
            failure.head_commit.as_deref(),
        )
    }
}

impl Display for GitResidual {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "residual_state path_exists={} registered={} branch_commit={} \
             head_commit={}; automatic cleanup skipped because concurrent \
             adoption cannot be disproven",
            self.path_exists, self.registered, self.branch_commit, self.head_commit,
        )
    }
}

/// One fully qualified ref that collided with an unqualified start point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousRef {
    /// Fully qualified refname, such as `refs/heads/collision`.
    pub refname: String,
    /// Canonical commit that ref peels to.
    pub commit: String,
}

/// Errors returned by core primitives.
#[derive(Debug)]
pub enum Error {
    /// A path segment was invalid for sandbox derivation.
    InvalidSegment { field: &'static str, value: String },
    /// A candidate path escaped or violated sandbox rules.
    SandboxViolation {
        base: PathBuf,
        candidate: PathBuf,
        reason: &'static str,
    },
    /// A filesystem operation failed.
    Io {
        context: &'static str,
        source: io::Error,
    },
    /// A git subprocess command failed.
    GitCommand { args: Vec<String>, stderr: String },
    /// An unqualified start point named more than one commit-ish.
    AmbiguousStartPoint {
        start_point: String,
        refs: Vec<AmbiguousRef>,
        git_warning: Option<String>,
    },
    /// A worktree create transaction failed and may have left residual state.
    WorktreeCreationFailed(Box<WorktreeCreationFailure>),
    /// Creation completed but its exact branch/ref/HEAD identity was not preserved.
    WorktreePostconditionFailed(Box<WorktreePostconditionFailure>),
    /// A legacy worktree-create request must select the exact-base boundary.
    ContractUpgradeRequired { required_schema_version: u8 },
    /// The exact-base request boundary requires an explicit start point.
    StartPointRequired,
    /// head_repo or a source remote was supplied without an explicit PR number.
    PrImportIdentityRequired,
    /// Exact-object import of a fork PR head failed and left residual refs.
    PrImportFailed(Box<PrImportFailure>),
    /// A git or gh command was blocked by safety policy.
    PolicyViolation {
        /// Machine-readable policy error code.
        code: PolicyCode,
        /// Human-readable explanation.
        message: String,
    },
    /// The SQLite lease store could not complete an operation.
    LeaseStore {
        context: &'static str,
        message: String,
    },
    /// Interrupted registration evidence is partial or conflicting.
    LeaseAttention(Box<LeaseAttentionFailure>),
}

/// Machine-readable error codes for policy violations.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PolicyCode {
    /// The git subcommand is not on the allowlist.
    SubcommandNotAllowed,
    /// Bare `--force` or `-f` was used without `--force-with-lease`.
    BareForcePush,
    /// A merge subcommand or `gh pr merge` was attempted.
    MergeBlocked,
    /// The current branch does not match the expected job branch.
    BranchMismatch,
    /// The git directory could not be resolved.
    GitDirUnavailable,
    /// The gh subcommand is not on the allowlist.
    GhSubcommandNotAllowed,
    /// A gh subcommand flag is not permitted.
    GhFlagNotAllowed,
    /// A path is outside the allowed sandbox (e.g. supervised --repo).
    PathNotAllowed,
    /// An existing worktree branch lacks a durable identity proving safe resume ownership.
    WorktreeResumeUnproven,
    /// The owner is missing from the configured allowlist (including an empty list).
    OwnerNotAllowed,
    /// The import source is not the configured base-repository remote.
    UnauthorizedSource,
    /// A job's active lease is held by a different worktree path.
    LeaseConflict,
    /// Reconciliation found partial or conflicting git/lease evidence.
    LeaseNeedsAttention,
    /// A released lease cannot be resurrected by reconcile or prepare.
    LeaseReleased,
    /// A tombstoned lease cannot be resurrected by reconcile or prepare.
    LeaseTombstoned,
    /// No live lease exists for the requested coordination claim.
    CoordClaimMissing,
    /// Another agent already owns this job claim; pause is not a seize.
    CoordClaimHeld,
    /// A handoff ACK named a stale owner generation.
    CoordStaleGeneration,
}

impl PolicyCode {
    /// Stable string representation for JSON error envelopes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubcommandNotAllowed => "SUBCOMMAND_NOT_ALLOWED",
            Self::BareForcePush => "BARE_FORCE_PUSH",
            Self::MergeBlocked => "MERGE_BLOCKED",
            Self::BranchMismatch => "BRANCH_MISMATCH",
            Self::GitDirUnavailable => "GIT_DIR_UNAVAILABLE",
            Self::GhSubcommandNotAllowed => "GH_SUBCOMMAND_NOT_ALLOWED",
            Self::GhFlagNotAllowed => "GH_FLAG_NOT_ALLOWED",
            Self::PathNotAllowed => "PATH_NOT_ALLOWED",
            Self::WorktreeResumeUnproven => "WORKTREE_RESUME_UNPROVEN",
            Self::OwnerNotAllowed => "OWNER_NOT_ALLOWED",
            Self::UnauthorizedSource => "UNAUTHORIZED_SOURCE",
            Self::LeaseConflict => "LEASE_CONFLICT",
            Self::LeaseNeedsAttention => "LEASE_NEEDS_ATTENTION",
            Self::LeaseReleased => "LEASE_RELEASED",
            Self::LeaseTombstoned => "LEASE_TOMBSTONED",
            Self::CoordClaimMissing => "COORD_CLAIM_MISSING",
            Self::CoordClaimHeld => "COORD_CLAIM_HELD",
            Self::CoordStaleGeneration => "COORD_STALE_GENERATION",
        }
    }
}

impl Display for PolicyCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSegment { field, value } => {
                write!(f, "invalid {field} segment: `{value}`")
            }
            Self::SandboxViolation {
                base,
                candidate,
                reason,
            } => write!(
                f,
                "sandbox violation for `{}` under `{}`: {reason}",
                candidate.display(),
                base.display()
            ),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::GitCommand { args, stderr } => {
                write!(
                    f,
                    "git command failed (`git {}`): {}",
                    args.join(" "),
                    stderr.trim()
                )
            }
            Self::AmbiguousStartPoint {
                start_point,
                refs,
                git_warning,
            } => {
                write!(f, "ambiguous start point `{start_point}`")?;
                for colliding in refs {
                    write!(f, " {}={}", colliding.refname, colliding.commit)?;
                }
                if let Some(warning) = git_warning {
                    write!(f, "; {warning}")?;
                }
                write!(f, "; qualify as refs/heads/<name> or refs/tags/<name>")
            }
            Self::WorktreeCreationFailed(failure) => Display::fmt(failure.as_ref(), f),
            Self::WorktreePostconditionFailed(failure) => Display::fmt(failure.as_ref(), f),
            Self::ContractUpgradeRequired {
                required_schema_version,
            } => write!(
                f,
                "worktree.create schema v1 is a non-mutating migration stub; retry with \
                 --schema-version {required_schema_version} and an explicit --start-point"
            ),
            Self::StartPointRequired => write!(
                f,
                "worktree.create schema v2 requires an explicit --start-point"
            ),
            Self::PrImportIdentityRequired => write!(
                f,
                "fork PR import requires --pr-number and a full head object id; \
                 head_repo is not fetch or checkout authority"
            ),
            Self::PrImportFailed(failure) => Display::fmt(failure.as_ref(), f),
            Self::PolicyViolation { code, message } => {
                write!(f, "policy violation [{code}]: {message}")
            }
            Self::LeaseStore { context, message } => {
                write!(f, "{context}: {message}")
            }
            Self::LeaseAttention(failure) => Display::fmt(failure.as_ref(), f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Error {
    /// Stable error code for JSON command envelopes.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSegment { .. } => "INVALID_SEGMENT",
            Self::SandboxViolation { .. } => "SANDBOX_VIOLATION",
            Self::Io { .. } => "IO_ERROR",
            Self::GitCommand { .. } => "GIT_COMMAND_FAILED",
            Self::AmbiguousStartPoint { .. } => "AMBIGUOUS_START_POINT",
            Self::WorktreeCreationFailed(_) => "WORKTREE_CREATE_FAILED",
            Self::WorktreePostconditionFailed(_) => "WORKTREE_POSTCONDITION_FAILED",
            Self::ContractUpgradeRequired { .. } => "CONTRACT_UPGRADE_REQUIRED",
            Self::StartPointRequired => "START_POINT_REQUIRED",
            Self::PrImportIdentityRequired => "PR_IMPORT_IDENTITY_REQUIRED",
            Self::PrImportFailed(_) => "PR_IMPORT_FAILED",
            Self::PolicyViolation { code, .. } => code.as_str(),
            Self::LeaseStore { .. } => "LEASE_STORE_FAILED",
            Self::LeaseAttention(_) => "LEASE_NEEDS_ATTENTION",
        }
    }

    /// Process exit code used by the CLI boundary.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::PolicyViolation { .. } | Self::LeaseAttention(_) => 2,
            _ => 1,
        }
    }
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io {
            context: "io operation",
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, PolicyCode, WorktreePostconditionFailure};
    use std::path::PathBuf;

    #[test]
    fn only_policy_violations_use_policy_exit_code() {
        let policy = Error::PolicyViolation {
            code: PolicyCode::WorktreeResumeUnproven,
            message: "resume identity is unproven".to_owned(),
        };
        let postcondition =
            Error::WorktreePostconditionFailed(Box::new(WorktreePostconditionFailure {
                path: PathBuf::from("worktree"),
                branch: "feature/test".to_owned(),
                expected_commit: "0".repeat(40),
                actual_branch: Some("refs/heads/feature/test".to_owned()),
                path_exists: true,
                branch_commit: Some("1".repeat(40)),
                head_commit: Some("1".repeat(40)),
                worktree_registered: true,
                reason: "identity mismatch".to_owned(),
            }));

        assert_eq!(policy.exit_code(), 2);
        assert_eq!(postcondition.exit_code(), 1);

        let attention = Error::LeaseAttention(Box::new(super::LeaseAttentionFailure {
            operation_id: "op-1".to_owned(),
            allocation_state: "NEEDS_ATTENTION".to_owned(),
            classification: "needs_attention".to_owned(),
            conflicts: vec!["conflict".to_owned()],
            path: PathBuf::from("/wt"),
            path_exists: true,
            branch_commit: None,
            head_commit: None,
            worktree_registered: true,
        }));
        assert_eq!(attention.exit_code(), 2);

        let ambiguous = Error::AmbiguousStartPoint {
            start_point: "collision".to_owned(),
            refs: vec![super::AmbiguousRef {
                refname: "refs/heads/collision".to_owned(),
                commit: "0".repeat(40),
            }],
            git_warning: None,
        };
        assert_eq!(ambiguous.exit_code(), 1);
        assert_eq!(ambiguous.code(), "AMBIGUOUS_START_POINT");
        let displayed = ambiguous.to_string();
        assert!(displayed.contains("collision"), "{displayed}");
        assert!(displayed.contains("refs/heads/collision"), "{displayed}");
    }
}
