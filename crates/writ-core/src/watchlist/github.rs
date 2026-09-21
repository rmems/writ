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
    /// Owner of the head repository (differs from `repo` owner for fork PRs).
    pub head_owner: Option<String>,
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

/// One pull request in a repository, used as a probe key.
#[derive(Debug, Clone, Copy)]
pub struct PrRef<'a> {
    pub repo: &'a str,
    pub number: u64,
}

/// Head branch in a repository, used to match a listed PR to a lease.
#[derive(Debug, Clone, Copy)]
pub struct BranchRef<'a> {
    pub repo: &'a str,
    pub branch: &'a str,
}

impl BranchRef<'_> {
    /// Whether `pr` belongs to this head: same branch and same head-repo owner
    /// (fork PRs from identically named branches do not match).
    #[must_use]
    pub fn matches(&self, pr: &PrSnapshot) -> bool {
        if !pr.branch.eq_ignore_ascii_case(self.branch) {
            return false;
        }
        let expected = self.repo.rsplit('/').nth(1);
        match (pr.head_owner.as_deref(), expected) {
            (Some(actual), Some(expected)) => actual.eq_ignore_ascii_case(expected),
            _ => true,
        }
    }
}

/// Probe failure. Callers map this into residuals rather than crashing the view.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ProbeError {
    OwnerNotAllowed { repo: String },
    Gh { repo: String, message: String },
}

impl ProbeError {
    fn from_policy(target: PrRef<'_>, err: crate::error::Error) -> Self {
        if err.code() == "OWNER_NOT_ALLOWED" {
            Self::OwnerNotAllowed {
                repo: target.repo.to_owned(),
            }
        } else {
            Self::Gh {
                repo: target.repo.to_owned(),
                message: err.to_string(),
            }
        }
    }

    fn from_output(target: PrRef<'_>, err: crate::error::Error) -> Self {
        Self::Gh {
            repo: target.repo.to_owned(),
            message: err.to_string(),
        }
    }

    fn from_message(target: PrRef<'_>, message: String) -> Self {
        Self::Gh {
            repo: target.repo.to_owned(),
            message,
        }
    }

    /// Encode the probe failure as a watchlist residual blocker.
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
    /// Fetch one pull request by repository and number.
    fn view(&self, target: PrRef<'_>) -> Result<PrSnapshot, ProbeError>;

    /// List recent pull requests (all states) for `repo`, so callers can match
    /// branches locally instead of running one subprocess per lease.
    fn list_prs(&self, repo: &str) -> Result<Vec<PrSnapshot>, ProbeError>;
}

/// Probe that runs `gh pr view` / `gh pr list` after argv policy.
#[derive(Debug, Clone)]
pub struct GhPrProbe {
    allowlist: OwnerAllowlist,
}

impl GhPrProbe {
    /// Construct a probe that permits GitHub access only for `allowlist` owners.
    #[must_use]
    pub fn new(allowlist: OwnerAllowlist) -> Self {
        Self { allowlist }
    }

    fn run_json(&self, args: &[String], target: PrRef<'_>) -> Result<String, ProbeError> {
        let cmd = SafeGhCommand::with_allowlist(args, &self.allowlist)
            .map_err(|err| ProbeError::from_policy(target, err))?;
        let output = cmd
            .run()
            .map_err(|err| ProbeError::from_output(target, err))?;
        if output.exit_code != 0 {
            let stderr = output.stderr.trim();
            let message = if stderr.is_empty() {
                format!("gh exited {}", output.exit_code)
            } else {
                stderr.to_owned()
            };
            return Err(ProbeError::from_message(target, message));
        }
        Ok(output.stdout)
    }

    fn decode_one(&self, target: PrRef<'_>, args: &[String]) -> Result<PrSnapshot, ProbeError> {
        let stdout = self.run_json(args, target)?;
        parse_pr_view(target, &stdout).map_err(|message| ProbeError::from_message(target, message))
    }

    fn decode_list(
        &self,
        target: PrRef<'_>,
        args: &[String],
    ) -> Result<Vec<PrSnapshot>, ProbeError> {
        let stdout = self.run_json(args, target)?;
        parse_pr_list(target, &stdout).map_err(|message| ProbeError::from_message(target, message))
    }
}

impl GithubProbe for GhPrProbe {
    fn view(&self, target: PrRef<'_>) -> Result<PrSnapshot, ProbeError> {
        self.decode_one(target, &pr_view_args(target))
    }

    fn list_prs(&self, repo: &str) -> Result<Vec<PrSnapshot>, ProbeError> {
        self.decode_list(PrRef { repo, number: 0 }, &pr_list_args(repo))
    }
}

const PR_JSON_FIELDS: &str = "number,title,url,state,headRefName,headRepositoryOwner,baseRefName,mergeable,reviewDecision,isDraft,statusCheckRollup";

fn pr_view_args(target: PrRef<'_>) -> Vec<String> {
    vec![
        "pr".to_owned(),
        "view".to_owned(),
        target.number.to_string(),
        "--repo".to_owned(),
        target.repo.to_owned(),
        "--json".to_owned(),
        PR_JSON_FIELDS.to_owned(),
    ]
}

fn pr_list_args(repo: &str) -> Vec<String> {
    vec![
        "pr".to_owned(),
        "list".to_owned(),
        "--repo".to_owned(),
        repo.to_owned(),
        "--state".to_owned(),
        "all".to_owned(),
        "--limit".to_owned(),
        "50".to_owned(),
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
    #[serde(rename = "headRepositoryOwner")]
    head_repository_owner: Option<GhRepoOwner>,
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
struct GhRepoOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhCheck {
    name: Option<String>,
    context: Option<String>,
    state: Option<String>,
    conclusion: Option<String>,
    status: Option<String>,
}

/// Parse `gh pr view --json` output and associate it with `target.repo`.
///
/// Returns an error message when `stdout` is not a valid pull-request payload.
pub fn parse_pr_view(target: PrRef<'_>, stdout: &str) -> Result<PrSnapshot, String> {
    let view: GhPrView = serde_json::from_str(stdout)
        .map_err(|err| format!("failed to parse gh pr view JSON: {err}"))?;
    Ok(snapshot_from_view(target, view))
}

fn parse_pr_list(target: PrRef<'_>, stdout: &str) -> Result<Vec<PrSnapshot>, String> {
    let views: Vec<GhPrView> = serde_json::from_str(stdout)
        .map_err(|err| format!("failed to parse gh pr list JSON: {err}"))?;
    Ok(views
        .into_iter()
        .map(|view| snapshot_from_view(target, view))
        .collect())
}

fn snapshot_from_view(target: PrRef<'_>, view: GhPrView) -> PrSnapshot {
    let checks = view
        .status_check_rollup
        .unwrap_or_default()
        .into_iter()
        .map(check_from_gh)
        .collect();
    PrSnapshot {
        repo: target.repo.to_owned(),
        number: view.number,
        branch: view.head_ref_name,
        head_owner: view.head_repository_owner.map(|owner| owner.login),
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
    use crate::owners::OwnerAllowlist;

    fn acme(number: u64) -> PrRef<'static> {
        PrRef {
            repo: "acme/sample",
            number,
        }
    }

    fn view_json(rollup: &str) -> String {
        format!(
            r#"{{"number":1,"title":"t","url":"https://example.test","state":"OPEN","headRefName":"feat","baseRefName":"main","statusCheckRollup":{rollup}}}"#
        )
    }

    fn parsed(rollup: &str) -> PrSnapshot {
        parse_pr_view(acme(1), &view_json(rollup)).unwrap()
    }

    #[test]
    fn parse_uses_context_and_status_when_name_missing() {
        let snap = parsed(r#"[{"context":"ci","status":"IN_PROGRESS","conclusion":""}]"#);
        assert_eq!(snap.checks[0].name, "ci");
        assert_eq!(snap.checks[0].state, "IN_PROGRESS");
    }

    #[test]
    fn parse_list_empty_is_empty() {
        assert!(parse_pr_list(acme(0), "[]").unwrap().is_empty());
    }

    #[test]
    fn parse_list_returns_all_with_head_owner() {
        let mut view = serde_json::from_str::<serde_json::Value>(&view_json("[]")).unwrap();
        view["headRepositoryOwner"] = serde_json::json!({"login": "acme"});
        let json = serde_json::to_string(&vec![view]).unwrap();
        let list = parse_pr_list(acme(0), &json).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].number, 1);
        assert!(!list[0].is_draft);
        assert_eq!(list[0].head_owner.as_deref(), Some("acme"));
    }

    #[test]
    fn parse_rejects_invalid_json() {
        let err = parse_pr_view(acme(1), "not-json").unwrap_err();
        assert!(err.contains("failed to parse gh pr view JSON"));
    }

    #[test]
    fn unnamed_check_uses_fallback_state() {
        let snap = parsed("[{}]");
        assert_eq!(snap.checks[0].name, "unnamed");
        assert_eq!(snap.checks[0].state, "UNKNOWN");
    }

    #[test]
    fn probe_error_residuals() {
        let denied = ProbeError::OwnerNotAllowed {
            repo: "acme/sample".to_owned(),
        };
        assert_eq!(denied.residual(), "github:owner_not_allowed:acme/sample");
        let gh = ProbeError::Gh {
            repo: "acme/sample".to_owned(),
            message: "boom".to_owned(),
        };
        assert_eq!(gh.residual(), "github:probe:acme/sample:boom");
    }

    #[test]
    fn argv_builders_use_refs() {
        let view = pr_view_args(acme(12));
        assert!(view.contains(&"--repo".to_owned()));
        assert!(view.contains(&"12".to_owned()));
        let list = pr_list_args("acme/sample");
        assert!(list.contains(&"acme/sample".to_owned()));
        assert!(list.contains(&"all".to_owned()));
    }

    #[test]
    fn branch_ref_matches_only_same_owner_head() {
        let head = BranchRef {
            repo: "acme/sample",
            branch: "feat",
        };
        let snap = |head_owner: Option<&str>| PrSnapshot {
            repo: "acme/sample".to_owned(),
            number: 1,
            branch: "feat".to_owned(),
            head_owner: head_owner.map(str::to_owned),
            base: "main".to_owned(),
            title: String::new(),
            url: String::new(),
            state: "OPEN".to_owned(),
            mergeable: None,
            review_decision: None,
            is_draft: false,
            checks: Vec::new(),
        };
        assert!(head.matches(&snap(Some("acme"))));
        assert!(!head.matches(&snap(Some("fork-owner"))));
        assert!(head.matches(&snap(None)));
    }

    #[test]
    fn gh_probe_rejects_disallowed_owner_before_gh() {
        let probe = GhPrProbe::new(OwnerAllowlist::parse("acme"));
        let denied = PrRef {
            repo: "other/denied",
            number: 1,
        };
        assert_eq!(
            probe.view(denied).unwrap_err(),
            ProbeError::OwnerNotAllowed {
                repo: "other/denied".to_owned()
            }
        );
        let err = probe.list_prs(denied.repo).unwrap_err();
        assert!(matches!(err, ProbeError::OwnerNotAllowed { .. }));
    }
}
