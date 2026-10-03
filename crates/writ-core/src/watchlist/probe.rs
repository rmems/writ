//! GitHub PR probe and snapshot types for the watchlist.

use std::time::Duration;

use serde::Deserialize;

use super::WatchlistError;
use crate::owners::OwnerAllowlist;
use crate::supervisor::{RunOptions, SupervisedOutput, Supervisor};

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
        let output = run_gh_supervised(args)?;
        let stdout = parse_probe_output(repo, number, output)?;
        parse_pr_view(repo, &stdout)
    }
}

fn run_gh_supervised(args: [String; 7]) -> Result<SupervisedOutput, WatchlistError> {
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|source| WatchlistError::Io {
                context: "create gh probe runtime",
                path: std::path::PathBuf::from("gh"),
                source,
            })?;
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let options = RunOptions {
            allowlist: Some(OwnerAllowlist::from_env()),
            ..RunOptions::default()
        };
        runtime
            .block_on(Supervisor::new(1).run("gh", &refs, Some(GH_PROBE_TIMEOUT), &options))
            .map_err(WatchlistError::from)
    });
    worker.join().map_err(|_| WatchlistError::Gh {
        repo: String::new(),
        number: 0,
        message: "gh probe supervisor thread panicked".to_owned(),
    })?
}

fn parse_probe_output(
    repo: &str,
    number: u64,
    output: SupervisedOutput,
) -> Result<String, WatchlistError> {
    if output.timed_out {
        return Err(WatchlistError::Timeout {
            repo: repo.to_owned(),
            number,
            message: format!(
                "gh pr view exceeded {}s deadline",
                GH_PROBE_TIMEOUT.as_secs()
            ),
        });
    }
    if output.exit_code != Some(0) {
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
                match output.exit_code {
                    Some(code) => format!("gh pr view exited {code}"),
                    None => "gh pr view was killed".to_owned(),
                }
            } else {
                stderr.to_owned()
            },
        });
    }
    Ok(output.stdout)
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

    #[test]
    fn supervised_timeout_maps_to_watchlist_timeout() {
        let output = crate::supervisor::SupervisedOutput {
            timed_out: true,
            ..crate::supervisor::SupervisedOutput::default()
        };

        let err = parse_probe_output("acme/widgets", 7, output).unwrap_err();

        assert!(matches!(
            err,
            WatchlistError::Timeout {
                repo,
                number: 7,
                ..
            } if repo == "acme/widgets"
        ));
    }
}
