//! Live GitHub PR probes. Results are not written to disk.

use serde::Deserialize;

use crate::git_safe::SafeGhCommand;
use crate::owners::OwnerAllowlist;

/// Snapshot of a pull request used only for the watchlist view.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PrSnapshot {
    pub repo: String,
    pub number: u64,
    pub branch: String,
    pub base: String,
    pub title: String,
    pub url: String,
    pub state: String,
    pub mergeable: Option<String>,
    pub review_decision: Option<String>,
    pub is_draft: bool,
    pub checks: Vec<CheckSnapshot>,
}

/// One CI/status rollup row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CheckSnapshot {
    pub name: String,
    pub state: String,
}

/// Probe failure. Callers map this into residuals rather than crashing the view.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ProbeError {
    OwnerNotAllowed { repo: String },
    Gh { repo: String, message: String },
}

impl ProbeError {
    #[must_use]
    pub fn residual(&self) -> String {
        match self {
            Self::OwnerNotAllowed { repo } => format!("github:owner_not_allowed:{repo}"),
            Self::Gh { repo, message } => format!("github:probe:{repo}:{message}"),
        }
    }
}

/// Source of PR metadata. Production uses allowlisted `gh`.
pub trait GithubProbe {
    fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, ProbeError>;
    fn find_by_branch(&self, repo: &str, branch: &str) -> Result<Option<PrSnapshot>, ProbeError>;
}

/// Probe that runs `gh pr view` / `gh pr list` after argv policy.
#[derive(Debug, Clone)]
pub struct GhPrProbe {
    allowlist: OwnerAllowlist,
}

impl GhPrProbe {
    #[must_use]
    pub fn new(allowlist: OwnerAllowlist) -> Self {
        Self { allowlist }
    }

    fn run_json(&self, args: &[String], repo: &str) -> Result<String, ProbeError> {
        let cmd = SafeGhCommand::with_allowlist(args, &self.allowlist).map_err(|err| {
            if err.code() == "OWNER_NOT_ALLOWED" {
                ProbeError::OwnerNotAllowed {
                    repo: repo.to_owned(),
                }
            } else {
                ProbeError::Gh {
                    repo: repo.to_owned(),
                    message: err.to_string(),
                }
            }
        })?;
        let output = cmd.run().map_err(|err| ProbeError::Gh {
            repo: repo.to_owned(),
            message: err.to_string(),
        })?;
        if output.exit_code != 0 {
            let stderr = output.stderr.trim();
            return Err(ProbeError::Gh {
                repo: repo.to_owned(),
                message: if stderr.is_empty() {
                    format!("gh exited {}", output.exit_code)
                } else {
                    stderr.to_owned()
                },
            });
        }
        Ok(output.stdout)
    }
}

impl GithubProbe for GhPrProbe {
    fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, ProbeError> {
        let args = pr_view_args(repo, number);
        let stdout = self.run_json(&args, repo)?;
        parse_pr_view(repo, &stdout).map_err(|message| ProbeError::Gh {
            repo: repo.to_owned(),
            message,
        })
    }

    fn find_by_branch(&self, repo: &str, branch: &str) -> Result<Option<PrSnapshot>, ProbeError> {
        let args = pr_list_args(repo, branch);
        let stdout = self.run_json(&args, repo)?;
        parse_pr_list(repo, &stdout).map_err(|message| ProbeError::Gh {
            repo: repo.to_owned(),
            message,
        })
    }
}

const PR_JSON_FIELDS: &str = "number,title,url,state,headRefName,baseRefName,mergeable,reviewDecision,isDraft,statusCheckRollup";

fn pr_view_args(repo: &str, number: u64) -> Vec<String> {
    vec![
        "pr".to_owned(),
        "view".to_owned(),
        number.to_string(),
        "--repo".to_owned(),
        repo.to_owned(),
        "--json".to_owned(),
        PR_JSON_FIELDS.to_owned(),
    ]
}

fn pr_list_args(repo: &str, branch: &str) -> Vec<String> {
    vec![
        "pr".to_owned(),
        "list".to_owned(),
        "--repo".to_owned(),
        repo.to_owned(),
        "--head".to_owned(),
        branch.to_owned(),
        "--state".to_owned(),
        "all".to_owned(),
        "--limit".to_owned(),
        "1".to_owned(),
        "--json".to_owned(),
        PR_JSON_FIELDS.to_owned(),
    ]
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

/// Parse `gh pr view --json` stdout.
pub fn parse_pr_view(repo: &str, stdout: &str) -> Result<PrSnapshot, String> {
    let view: GhPrView = serde_json::from_str(stdout)
        .map_err(|err| format!("failed to parse gh pr view JSON: {err}"))?;
    Ok(snapshot_from_view(repo, view))
}

fn parse_pr_list(repo: &str, stdout: &str) -> Result<Option<PrSnapshot>, String> {
    let views: Vec<GhPrView> = serde_json::from_str(stdout)
        .map_err(|err| format!("failed to parse gh pr list JSON: {err}"))?;
    Ok(views
        .into_iter()
        .next()
        .map(|view| snapshot_from_view(repo, view)))
}

fn snapshot_from_view(repo: &str, view: GhPrView) -> PrSnapshot {
    let checks = view
        .status_check_rollup
        .unwrap_or_default()
        .into_iter()
        .map(check_from_gh)
        .collect();
    PrSnapshot {
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
    }
}

fn check_from_gh(check: GhCheck) -> CheckSnapshot {
    let name = check
        .name
        .or(check.context)
        .unwrap_or_else(|| "unnamed".to_owned());
    let state = non_empty(check.conclusion)
        .or_else(|| non_empty(check.state))
        .or_else(|| non_empty(check.status))
        .unwrap_or_else(|| "UNKNOWN".to_owned());
    CheckSnapshot { name, state }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uses_context_and_status_when_name_missing() {
        let json = r#"{
          "number": 1,
          "title": "t",
          "url": "https://example.test",
          "state": "OPEN",
          "headRefName": "feat",
          "baseRefName": "main",
          "mergeable": "MERGEABLE",
          "statusCheckRollup": [{"context": "ci", "status": "IN_PROGRESS", "conclusion": ""}]
        }"#;
        let snap = parse_pr_view("acme/sample", json).unwrap();
        assert_eq!(snap.checks[0].name, "ci");
        assert_eq!(snap.checks[0].state, "IN_PROGRESS");
    }

    #[test]
    fn parse_list_empty_is_none() {
        assert!(parse_pr_list("acme/sample", "[]").unwrap().is_none());
    }
}
