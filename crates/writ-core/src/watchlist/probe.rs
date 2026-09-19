//! GitHub PR probe and snapshot types for the watchlist.

use std::time::Duration;

use serde::Deserialize;

use super::WatchlistError;
use crate::git_safe::{GhRun, SafeGhCommand};

/// Default wall-clock deadline for a single `gh pr view` probe. A hung `gh`
/// (network stall, credential prompt) is killed after this and mapped to
/// [`WatchlistError::Timeout`] rather than blocking the check cycle forever.
const GH_PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// Snapshot of a pull request used to insert or refresh a watchlist entry.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PrSnapshot {
    /// `owner/name`.
    pub repo: String,
    /// Pull-request number.
    pub number: u64,
    /// Head branch.
    pub branch: String,
    /// Base branch.
    pub base: String,
    /// Title.
    pub title: String,
    /// HTML URL.
    pub url: String,
    /// GitHub `state`: `OPEN`, `MERGED`, or `CLOSED`.
    pub state: String,
    /// GitHub `mergeable` (`MERGEABLE`, `CONFLICTING`, `UNKNOWN`).
    pub mergeable: Option<String>,
    /// GitHub `reviewDecision`.
    pub review_decision: Option<String>,
    /// GitHub `isDraft`: a draft PR is not ready to merge.
    pub is_draft: bool,
    /// Rollup checks.
    pub checks: Vec<CheckSnapshot>,
}

/// One CI/status rollup row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CheckSnapshot {
    /// Check name or context.
    pub name: String,
    /// Combined state/conclusion (`SUCCESS`, `FAILURE`, `PENDING`, …).
    pub state: String,
}

/// Source of PR metadata. Production uses `gh pr view` via [`GhPrProbe`].
pub trait PrProbe {
    /// Load current GitHub metadata for one pull request.
    fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError>;
}

/// Probe that runs allowlisted `gh pr view --json …`.
#[derive(Debug, Default, Clone, Copy)]
pub struct GhPrProbe;

impl PrProbe for GhPrProbe {
    fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
        let args = [
            "pr".to_owned(),
            "view".to_owned(),
            number.to_string(),
            "--repo".to_owned(),
            repo.to_owned(),
            "--json".to_owned(),
            "number,title,url,state,headRefName,baseRefName,mergeable,reviewDecision,isDraft,statusCheckRollup"
                .to_owned(),
        ];
        let cmd = SafeGhCommand::new(&args)?;
        // Use a bounded wall-clock deadline: a truly hung `gh` never returns
        // from blocking `output()`, so the stderr-based `looks_like_timeout`
        // heuristic below can never fire. The deadline turns a stall into an
        // explicit WatchlistError::Timeout; the stderr mapping stays as a
        // fallback for a `gh` that exits non-zero with a timeout message.
        let output = match cmd.run_with_timeout(GH_PROBE_TIMEOUT)? {
            GhRun::Completed(output) => output,
            GhRun::TimedOut { timeout } => {
                return Err(WatchlistError::Timeout {
                    repo: repo.to_owned(),
                    number,
                    message: format!("gh pr view exceeded {}s deadline", timeout.as_secs()),
                });
            }
        };
        if output.exit_code != 0 {
            let stderr = output.stderr.trim();
            if looks_like_timeout(stderr) {
                return Err(WatchlistError::Timeout {
                    repo: repo.to_owned(),
                    number,
                    message: stderr.to_owned(),
                });
            }
            return Err(WatchlistError::Gh {
                repo: repo.to_owned(),
                number,
                message: if stderr.is_empty() {
                    format!("gh pr view exited {}", output.exit_code)
                } else {
                    stderr.to_owned()
                },
            });
        }
        parse_pr_view(repo, &output.stdout)
    }
}

fn looks_like_timeout(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timeout") || lower.contains("timed out")
}

#[derive(Debug, Deserialize)]
struct GhPrView {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    mergeable: Option<String>,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(rename = "isDraft", default)]
    is_draft: Option<bool>,
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Vec<GhCheck>>,
}

#[derive(Debug, Deserialize)]
struct GhCheck {
    name: Option<String>,
    context: Option<String>,
    state: Option<String>,
    conclusion: Option<String>,
    status: Option<String>,
}

pub(crate) fn parse_pr_view(repo: &str, stdout: &str) -> Result<PrSnapshot, WatchlistError> {
    let view: GhPrView = serde_json::from_str(stdout).map_err(|err| WatchlistError::Gh {
        repo: repo.to_owned(),
        number: 0,
        message: format!("failed to parse gh pr view JSON: {err}"),
    })?;
    let checks = view
        .status_check_rollup
        .unwrap_or_default()
        .into_iter()
        .map(|check| {
            let name = check
                .name
                .or(check.context)
                .unwrap_or_else(|| "unnamed".to_owned());
            // Treat empty strings as absent: an in-progress CheckRun reports
            // conclusion="" with status="IN_PROGRESS", and an empty conclusion
            // must not win over a real status (which would look healthy).
            let state = non_empty(check.conclusion)
                .or_else(|| non_empty(check.state))
                .or_else(|| non_empty(check.status))
                .unwrap_or_else(|| "UNKNOWN".to_owned());
            CheckSnapshot { name, state }
        })
        .collect();
    Ok(PrSnapshot {
        repo: repo.to_owned(),
        number: view.number,
        branch: view.head_ref_name,
        base: view.base_ref_name,
        title: view.title,
        url: view.url,
        state: view.state,
        mergeable: view.mergeable,
        review_decision: view.review_decision,
        is_draft: view.is_draft.unwrap_or(false),
        checks,
    })
}

/// Map `Some("")` to `None` so empty JSON strings are treated as absent.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uses_context_when_name_missing() {
        let json = r#"{
          "number": 1,
          "title": "t",
          "url": "https://example.test",
          "state": "OPEN",
          "headRefName": "head",
          "baseRefName": "main",
          "mergeable": "MERGEABLE",
          "statusCheckRollup": [{
            "context": "ci/build",
            "conclusion": "",
            "status": "IN_PROGRESS"
          }]
        }"#;
        let snap = parse_pr_view("acme/widgets", json).unwrap();
        assert_eq!(snap.checks[0].name, "ci/build");
        assert_eq!(snap.checks[0].state, "IN_PROGRESS");
    }

    #[test]
    fn parse_invalid_json_is_gh_error() {
        let err = parse_pr_view("acme/widgets", "{").unwrap_err();
        assert!(matches!(err, WatchlistError::Gh { .. }));
    }
}
