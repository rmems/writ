//! Status enums and JSON row types for the collaboration watchlist view.

use std::fmt::{self, Formatter};

use serde::Serialize;

/// Local collaboration lifecycle. Distinct from GitHub PR/check state.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollabStatus {
    /// Active writer lock, not paused or blocked.
    Running,
    /// Waiting on help, a handoff ACK, a dependency, or a blocked lease.
    Waiting,
    /// Claim is paused; WIP is preserved.
    Paused,
    /// Conflict ownership: git conflict, `NEEDS_HUMAN`, or conflicting PR.
    Conflicted,
    /// Locally ready to integrate on the assigned branch (not GitHub-merged).
    ReadyForIntegration,
}

impl fmt::Display for CollabStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Conflicted => "conflicted",
            Self::ReadyForIntegration => "ready_for_integration",
        })
    }
}

/// Checkout/lease recovery, independent of collaboration status.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    Live,
    Released,
    StaleHeartbeat,
    MissingCheckout,
}

impl fmt::Display for RecoveryStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Live => "live",
            Self::Released => "released",
            Self::StaleHeartbeat => "stale_heartbeat",
            Self::MissingCheckout => "missing_checkout",
        })
    }
}

/// External GitHub view. Never a merge gate.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GithubState {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub branch: String,
    pub base: String,
    /// GitHub `OPEN` / `MERGED` / `CLOSED`.
    pub state: String,
    /// Visibility classification of checks/review (not local collab status).
    pub check_status: String,
    pub mergeable: Option<String>,
    pub is_draft: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub residual_blockers: Vec<String>,
}

/// Optional same-host claim/message overlay. Empty when RM-825 tables are absent.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct CoordOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_generation: Option<i64>,
    pub paused: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub declared_paths: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub overlaps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<String>,
}

/// One watchlist row: lease ownership plus optional overlays.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct WatchEntry {
    pub job_id: String,
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub worktree_path: String,
    pub lease_mode: String,
    pub collab_status: CollabStatus,
    pub recovery_status: RecoveryStatus,
    pub coord: CoordOverlay,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub residual_blockers: Vec<String>,
}

/// Payload for `writ watchlist` JSON envelopes.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct WatchlistData {
    pub entries: Vec<WatchEntry>,
    /// True when `coord_claims` / `coord_messages` were readable.
    pub coord_available: bool,
    /// Live GitHub probes ran for this command.
    pub github_probed: bool,
}

impl WatchlistData {
    /// Construct an empty payload with the supplied probe and coordination availability flags.
    #[must_use]
    pub fn empty(github_probed: bool, coord_available: bool) -> Self {
        Self {
            entries: Vec::new(),
            coord_available,
            github_probed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collab_status_serializes_snake_case() {
        let json = serde_json::to_string(&CollabStatus::ReadyForIntegration).unwrap();
        assert_eq!(json, "\"ready_for_integration\"");
    }

    #[test]
    fn status_display_tokens() {
        assert_eq!(CollabStatus::Running.to_string(), "running");
        assert_eq!(CollabStatus::Waiting.to_string(), "waiting");
        assert_eq!(CollabStatus::Paused.to_string(), "paused");
        assert_eq!(CollabStatus::Conflicted.to_string(), "conflicted");
        assert_eq!(
            CollabStatus::ReadyForIntegration.to_string(),
            "ready_for_integration"
        );
        assert_eq!(RecoveryStatus::Live.to_string(), "live");
        assert_eq!(RecoveryStatus::Released.to_string(), "released");
        assert_eq!(
            RecoveryStatus::StaleHeartbeat.to_string(),
            "stale_heartbeat"
        );
        assert_eq!(
            RecoveryStatus::MissingCheckout.to_string(),
            "missing_checkout"
        );
    }
}
