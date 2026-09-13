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
            "worktree creation failed for branch `{}` at `{}`: {}; residual_state \
             path_exists={} registered={} branch_commit={} head_commit={}; automatic cleanup \
             skipped because concurrent adoption cannot be disproven",
            self.branch,
            self.path.display(),
            self.stderr.trim(),
            self.path_exists,
            self.worktree_registered,
            self.branch_commit.as_deref().unwrap_or("<absent>"),
            self.head_commit.as_deref().unwrap_or("<absent>")
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

impl Display for WorktreePostconditionFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "worktree postcondition failed for branch `{}` at `{}`: expected_commit={} \
             actual_branch={} reason={}; residual_state path_exists={} registered={} \
             branch_commit={} head_commit={}; automatic cleanup skipped because concurrent \
             adoption cannot be disproven",
            self.branch,
            self.path.display(),
            self.expected_commit,
            self.actual_branch.as_deref().unwrap_or("<unavailable>"),
            self.reason,
            self.path_exists,
            self.worktree_registered,
            self.branch_commit.as_deref().unwrap_or("<absent>"),
            self.head_commit.as_deref().unwrap_or("<absent>")
        )
    }
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
    /// A worktree create transaction failed and may have left residual state.
    WorktreeCreationFailed(Box<WorktreeCreationFailure>),
    /// Creation completed but its exact branch/ref/HEAD identity was not preserved.
    WorktreePostconditionFailed(Box<WorktreePostconditionFailure>),
    /// A legacy worktree-create request must select the exact-base boundary.
    ContractUpgradeRequired { required_schema_version: u8 },
    /// The exact-base request boundary requires an explicit start point.
    StartPointRequired,
    /// A git or gh command was blocked by safety policy.
    PolicyViolation {
        /// Machine-readable policy error code.
        code: PolicyCode,
        /// Human-readable explanation.
        message: String,
    },
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
            Self::PolicyViolation { code, message } => {
                write!(f, "policy violation [{code}]: {message}")
            }
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
            Self::WorktreeCreationFailed(_) => "WORKTREE_CREATE_FAILED",
            Self::WorktreePostconditionFailed(_) => "WORKTREE_POSTCONDITION_FAILED",
            Self::ContractUpgradeRequired { .. } => "CONTRACT_UPGRADE_REQUIRED",
            Self::StartPointRequired => "START_POINT_REQUIRED",
            Self::PolicyViolation { code, .. } => code.as_str(),
        }
    }

    /// Process exit code used by the CLI boundary.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::PolicyViolation { .. } => 2,
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
    }
}
