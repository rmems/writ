//! Versioned PR watchlist schema (Linear RM-127 / GitHub #12).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Current on-disk schema version for `watchlist.json`.
pub const WATCHLIST_VERSION: u32 = 1;

/// Lifecycle status of a watched pull request.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchStatus {
    /// Required checks passed and no residual blockers.
    Healthy,
    /// Checks or review still in progress.
    Pending,
    /// At least one required check failed.
    Failed,
    /// Open leftovers that are not a hard CI fail (review, class B/C).
    Residual,
    /// GitHub reports a merge conflict.
    Conflict,
    /// A check cycle timed out talking to GitHub.
    Timeout,
}

impl fmt::Display for WatchStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Healthy => "healthy",
            Self::Pending => "pending",
            Self::Failed => "failed",
            Self::Residual => "residual",
            Self::Conflict => "conflict",
            Self::Timeout => "timeout",
        })
    }
}

/// Why a watchlist entry exists.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchKind {
    /// Tracked for babysit / check-all cycles.
    PrBabysit,
    /// Added after issue-to-PR handoff.
    IssueToPr,
}

impl fmt::Display for WatchKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PrBabysit => "pr_babysit",
            Self::IssueToPr => "issue_to_pr",
        })
    }
}

/// One stacked group: a repo plus PR numbers in bottom-up order.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct StackGroup {
    /// Repository (`owner/name`) the stacked numbers belong to.
    pub repo: String,
    /// Pull-request numbers, bottom of the stack first.
    pub numbers: Vec<u64>,
}

/// One watched pull request. Identity is `(repo, number)`.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct WatchEntry {
    /// Repository in `owner/name` form.
    pub repo: String,
    /// Pull-request number.
    pub number: u64,
    /// Head branch.
    pub branch: String,
    /// Current check-cycle status.
    pub status: WatchStatus,
    /// RFC3339 UTC timestamp of the last check (or add, if never checked).
    pub last_checked: String,
    /// Fix commits already used in the current babysit budget.
    pub fix_count: u32,
    /// Structured leftover codes (`class_a:…`, `class_b:…`, `review:…`).
    #[serde(default)]
    pub residual_blockers: Vec<String>,
    /// Stack identifier, if this PR belongs to a stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_id: Option<String>,
    /// Optional stack type label from stack detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_type: Option<String>,
    /// Position in the stack; 0 is the bottom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_position: Option<u32>,
    /// Base branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// PR title (local only; file mode 600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// RFC3339 UTC timestamp when the entry was first added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    /// How many check cycles have run against this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_count: Option<u32>,
    /// HTML URL for the pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Job kind for issue-to-PR vs babysit handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<WatchKind>,
    /// Unknown additive v1 fields preserved on round-trip.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl WatchEntry {
    /// Owner segment of `repo`, if the value is `owner/name`.
    #[must_use]
    pub fn owner(&self) -> Option<&str> {
        owner_of_repo(&self.repo)
    }

    /// True when this entry is the same pull request as `(repo, number)`.
    #[must_use]
    pub fn is_identity(&self, repo: &str, number: u64) -> bool {
        self.number == number && self.repo == repo
    }
}

/// On-disk watchlist document.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Watchlist {
    /// Schema version. Readers must reject versions greater than they support.
    pub version: u32,
    /// Watched pull requests. Primary key is `(repo, number)`.
    #[serde(default)]
    pub prs: Vec<WatchEntry>,
    /// Stack groups keyed by `stack_id`.
    #[serde(default)]
    pub groups: BTreeMap<String, StackGroup>,
    /// Unknown top-level keys preserved on round-trip.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Watchlist {
    fn default() -> Self {
        Self {
            version: WATCHLIST_VERSION,
            prs: Vec::new(),
            groups: BTreeMap::new(),
            extra: Map::new(),
        }
    }
}

impl Watchlist {
    /// Look up an entry by `(repo, number)`.
    #[must_use]
    pub fn get(&self, repo: &str, number: u64) -> Option<&WatchEntry> {
        self.prs
            .iter()
            .find(|entry| entry.is_identity(repo, number))
    }

    /// Mutable lookup by `(repo, number)`.
    pub fn get_mut(&mut self, repo: &str, number: u64) -> Option<&mut WatchEntry> {
        self.prs
            .iter_mut()
            .find(|entry| entry.is_identity(repo, number))
    }

    /// Entries matching optional owner and repo filters. `list` shows every
    /// owner by default.
    #[must_use]
    pub fn filtered<'a>(
        &'a self,
        owner: Option<&'a str>,
        repo: Option<&'a str>,
    ) -> Vec<&'a WatchEntry> {
        self.prs
            .iter()
            .filter(|entry| owner.is_none_or(|want| owner_matches(entry.owner(), want)))
            .filter(|entry| repo.is_none_or(|want| entry.repo == want))
            .collect()
    }
}

/// Owner segment of `owner/name`.
#[must_use]
pub fn owner_of_repo(repo: &str) -> Option<&str> {
    let (owner, name) = repo.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(owner)
}

fn owner_matches(actual: Option<&str>, want: &str) -> bool {
    actual.is_some_and(|owner| owner.eq_ignore_ascii_case(want))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_status_round_trips() {
        for status in [
            WatchStatus::Healthy,
            WatchStatus::Pending,
            WatchStatus::Failed,
            WatchStatus::Residual,
            WatchStatus::Conflict,
            WatchStatus::Timeout,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: WatchStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn extra_fields_round_trip() {
        let json = r#"{
            "version": 1,
            "prs": [{
                "repo": "acme/widgets",
                "number": 7,
                "branch": "fix/x",
                "status": "pending",
                "last_checked": "2026-01-01T00:00:00Z",
                "fix_count": 0,
                "residual_blockers": [],
                "kind": "pr_babysit",
                "custom_note": "keep-me"
            }],
            "groups": {},
            "operator": "hive"
        }"#;
        let list: Watchlist = serde_json::from_str(json).unwrap();
        assert_eq!(list.prs[0].extra.get("custom_note").unwrap(), "keep-me");
        assert_eq!(list.extra.get("operator").unwrap(), "hive");
        let out = serde_json::to_value(&list).unwrap();
        assert_eq!(out["prs"][0]["custom_note"], "keep-me");
        assert_eq!(out["operator"], "hive");
    }

    #[test]
    fn owner_of_repo_rejects_bad_slugs() {
        assert_eq!(owner_of_repo("acme/widgets"), Some("acme"));
        assert_eq!(owner_of_repo("acme/widgets/extra"), None);
        assert_eq!(owner_of_repo("acme"), None);
        assert_eq!(owner_of_repo("/widgets"), None);
    }
}
