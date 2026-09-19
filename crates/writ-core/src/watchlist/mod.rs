//! Multi-owner PR watchlist persistence and command surface.
//!
//! This store is **not** [`crate::state`]'s `watched.json` (a JSON array of
//! job-status objects, read-only today). The watchlist lives in
//! `watchlist.json` under the same state root, or `WRIT_WATCHLIST_PATH`.
//!
//! Single-writer: callers must not run two `check-all` processes against the
//! same file. Writes are temp-file + rename; corrupt files are quarantined
//! rather than silently overwritten.

mod classify;
mod import;
mod ops;
mod probe;
mod schema;
mod stack;
mod store;

pub use import::{default_pr_babysit_path, import_pr_babysit, import_pr_babysit_at};
pub use ops::{
    AddReport, CheckReport, add_prs, add_prs_at, check_prs, check_prs_at, list_prs_at, remove_pr,
    remove_pr_at,
};
pub use probe::{CheckSnapshot, GhPrProbe, PrProbe, PrSnapshot};
pub use schema::{
    StackGroup, WATCHLIST_VERSION, WatchEntry, WatchKind, WatchStatus, Watchlist, owner_of_repo,
};
pub use store::{
    load_allowed_owners, load_watchlist, mutate_watchlist, owner_is_allowed, save_watchlist,
    utc_now_rfc3339,
};

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::error::PolicyCode;

/// Failures from watchlist load/save and GitHub refresh.
#[derive(Debug)]
pub enum WatchlistError {
    /// Filesystem failure while reading or writing the store.
    Io {
        /// Short operation label.
        context: &'static str,
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The file existed but was not valid JSON; it was moved aside.
    Corrupt {
        /// Original path.
        path: PathBuf,
        /// Quarantine path the bytes were moved to.
        quarantine: PathBuf,
        /// Parser message.
        message: String,
    },
    /// `version` is newer than this build supports.
    UnsupportedVersion {
        /// File path.
        path: PathBuf,
        /// Version found on disk.
        version: u32,
    },
    /// JSON serialization failed.
    Serialize {
        /// Target path.
        path: PathBuf,
        /// serde error text.
        message: String,
    },
    /// Requested `(repo, number)` is not on the watchlist.
    NotFound {
        /// Repository `owner/name`.
        repo: String,
        /// Pull-request number.
        number: u64,
    },
    /// Owner is outside `WRIT_ALLOWED_OWNERS`.
    OwnerNotAllowed {
        /// Rejected owner.
        owner: String,
    },
    /// Multi-owner `check-all` ran with an empty allowlist.
    AllowlistRequired,
    /// Caller supplied a malformed argument.
    InvalidInput(String),
    /// GitHub CLI timed out while viewing a PR.
    Timeout {
        /// Repository `owner/name`.
        repo: String,
        /// Pull-request number.
        number: u64,
        /// GitHub CLI stderr.
        message: String,
    },
    /// `gh pr view` failed or returned unusable JSON.
    Gh {
        /// Repository `owner/name`.
        repo: String,
        /// Pull-request number (0 if unknown).
        number: u64,
        /// Error text.
        message: String,
    },
    /// `gh` was blocked by the git-safe allowlist.
    Policy {
        /// Stable policy code.
        code: PolicyCode,
        /// Explanation.
        message: String,
    },
}

impl fmt::Display for WatchlistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                context,
                path,
                source,
            } => write!(f, "{context} {}: {source}", path.display()),
            Self::Corrupt {
                path,
                quarantine,
                message,
            } => write!(
                f,
                "corrupt watchlist at {}; quarantined to {}: {message}",
                path.display(),
                quarantine.display()
            ),
            Self::UnsupportedVersion { path, version } => write!(
                f,
                "unsupported watchlist version {version} in {} (this build supports {WATCHLIST_VERSION})",
                path.display()
            ),
            Self::Serialize { path, message } => {
                write!(f, "failed to serialize {}: {message}", path.display())
            }
            Self::NotFound { repo, number } => {
                write!(f, "{repo}#{number} is not on the watchlist")
            }
            Self::OwnerNotAllowed { owner } => {
                write!(f, "owner `{owner}` is not in WRIT_ALLOWED_OWNERS")
            }
            Self::AllowlistRequired => write!(
                f,
                "check-all requires WRIT_ALLOWED_OWNERS (empty allowlist denies multi-owner walks)"
            ),
            Self::InvalidInput(message) => write!(f, "{message}"),
            Self::Timeout {
                repo,
                number,
                message,
            } => write!(f, "timed out viewing {repo}#{number}: {message}"),
            Self::Gh {
                repo,
                number,
                message,
            } => write!(f, "gh pr view {repo}#{number}: {message}"),
            Self::Policy { code, message } => {
                write!(f, "policy violation [{code}]: {message}")
            }
        }
    }
}

impl std::error::Error for WatchlistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<crate::error::Error> for WatchlistError {
    fn from(err: crate::error::Error) -> Self {
        match err {
            crate::error::Error::PolicyViolation { code, message } => {
                Self::Policy { code, message }
            }
            other => Self::Gh {
                repo: String::new(),
                number: 0,
                message: other.to_string(),
            },
        }
    }
}

impl WatchlistError {
    /// Stable JSON error code for CLI envelopes.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "IO_ERROR",
            Self::Corrupt { .. } => "CORRUPT_STATE",
            Self::UnsupportedVersion { .. } => "UNSUPPORTED_VERSION",
            Self::Serialize { .. } => "SERIALIZE_FAILED",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::OwnerNotAllowed { .. } => "OWNER_NOT_ALLOWED",
            Self::AllowlistRequired => "OWNER_ALLOWLIST_REQUIRED",
            Self::InvalidInput(_) => "INVALID_INPUT",
            Self::Timeout { .. } => "TIMEOUT",
            Self::Gh { .. } => "GH_VIEW_FAILED",
            Self::Policy { code, .. } => code.as_str(),
        }
    }

    /// Process exit code: policy violations are 2, everything else 1.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Policy { .. } | Self::OwnerNotAllowed { .. } | Self::AllowlistRequired => 2,
            _ => 1,
        }
    }
}
