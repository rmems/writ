//! Claim an issue or pull request into one isolated worktree.
//!
//! This is the orchestration layer that used to live in Python `ClaimManager`.
//! Git mutations still go through [`crate::worktree::WorktreeManager`]: issues
//! create a new `hive/issue-<n>` branch from an exact start point; pull
//! requests attach to the caller-supplied head branch without renaming it.

use std::path::{Path, PathBuf};

use crate::error::{Error, PolicyCode, Result};
use crate::paths::derive_worktree_path;
use crate::worktree::{Worktree, WorktreeCreateRequest, WorktreeManager};

const ISSUE_JOB_PREFIX: &str = "gh-";
const PR_JOB_PREFIX: &str = "pr-";
const ISSUE_BRANCH_PREFIX: &str = "hive/issue-";
const WRIT_ALLOWED_OWNERS_ENV: &str = "WRIT_ALLOWED_OWNERS";
const LEGACY_ALLOWED_OWNERS_ENV: &str = "WH_ALLOWED_OWNERS";
const MAX_SLUG_CHARS: usize = 40;

/// GitHub issue versus pull-request claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimResource {
    /// A GitHub issue that needs a new hive branch.
    Issue,
    /// An existing pull request whose head branch is reused as-is.
    PullRequest,
}

/// Parsed GitHub issue or pull-request identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubClaimTarget {
    pub owner: String,
    pub repo: String,
    pub kind: ClaimResource,
    pub number: u64,
}

/// Issue versus PR inputs after owner/repo/number have been resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimKind<'a> {
    /// Create `hive/issue-<n>` (optionally slugged) from `start_point`.
    Issue { number: u64, slug: Option<&'a str> },
    /// Attach to `head_branch` at `start_point` without renaming it.
    PullRequest {
        number: u64,
        head_branch: &'a str,
        head_repo: Option<&'a str>,
    },
}

/// Inputs required to claim one issue or pull request.
#[derive(Debug, Clone, Copy)]
pub struct ClaimRequest<'a> {
    pub repo_root: &'a Path,
    pub owner: &'a str,
    pub repo: &'a str,
    pub start_point: &'a str,
    pub kind: ClaimKind<'a>,
    /// Empty means unrestricted for this single-repository operation.
    pub allowed_owners: &'a [String],
}

/// Successful claim metadata for CLI and orchestrator handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimResult {
    pub owner: String,
    pub repo: String,
    pub job_id: String,
    pub branch: String,
    pub worktree_path: PathBuf,
    pub issue_number: Option<u64>,
    pub pr_number: Option<u64>,
    pub owns_branch: bool,
    pub start_commit: String,
    pub head_commit: String,
    pub head_repo: Option<String>,
}

/// Load `WRIT_ALLOWED_OWNERS` (then `WH_ALLOWED_OWNERS`).
///
/// `None` means the variable is unset, empty, or `*`, so a single-repository
/// claim is unrestricted. `Some` is a deny-by-default list for `writ claim`.
#[must_use]
pub fn allowed_owners_from_env() -> Option<Vec<String>> {
    allowed_owners_from(
        std::env::var_os(WRIT_ALLOWED_OWNERS_ENV),
        std::env::var_os(LEGACY_ALLOWED_OWNERS_ENV),
    )
}

fn allowed_owners_from(
    writ: Option<std::ffi::OsString>,
    legacy: Option<std::ffi::OsString>,
) -> Option<Vec<String>> {
    let raw = writ
        .filter(|value| !value.is_empty())
        .or_else(|| legacy.filter(|value| !value.is_empty()))?;
    let text = raw.to_str()?.trim();
    if text.is_empty() || text == "*" {
        return None;
    }
    let owners: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if owners.is_empty() {
        None
    } else {
        Some(owners)
    }
}

/// Parse a `github.com` issue or pull-request URL into owner/repo/number.
pub fn parse_github_claim_url(input: &str) -> Result<GitHubClaimTarget> {
    let trimmed = input.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest
        .strip_prefix("github.com/")
        .ok_or_else(|| invalid_claim(format!("not a github.com issue or pull URL: {input:?}")))?;
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .trim_end_matches('/');
    let mut parts = path.split('/');
    let owner = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| invalid_claim(format!("GitHub URL missing owner: {input:?}")))?;
    let repo = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| invalid_claim(format!("GitHub URL missing repository: {input:?}")))?;
    let kind = match parts.next() {
        Some("issues") => ClaimResource::Issue,
        Some("pull") => ClaimResource::PullRequest,
        other => {
            return Err(invalid_claim(format!(
                "GitHub URL must contain /issues/ or /pull/, got {other:?} in {input:?}"
            )));
        }
    };
    let number_text = parts.next().ok_or_else(|| {
        invalid_claim(format!(
            "GitHub URL missing issue or pull number: {input:?}"
        ))
    })?;
    if parts.next().is_some() {
        return Err(invalid_claim(format!(
            "GitHub URL has extra path segments: {input:?}"
        )));
    }
    let number = parse_claim_number(number_text)?;
    validate_ref_segment("owner", owner)?;
    validate_ref_segment("repo", repo)?;
    Ok(GitHubClaimTarget {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        kind,
        number,
    })
}

/// Derive the issue branch name `hive/issue-<n>` or `hive/issue-<n>-<slug>`.
pub fn issue_branch_name(number: u64, slug: Option<&str>) -> Result<String> {
    require_positive("issue_number", number)?;
    match slug {
        None => Ok(format!("{ISSUE_BRANCH_PREFIX}{number}")),
        Some(raw) => {
            let slug = sanitize_slug(raw)?;
            Ok(format!("{ISSUE_BRANCH_PREFIX}{number}-{slug}"))
        }
    }
}

/// Job id `gh-<n>` used as the worktree path segment for an issue.
#[must_use]
pub fn issue_job_id(number: u64) -> String {
    format!("{ISSUE_JOB_PREFIX}{number}")
}

/// Job id `pr-<n>` used as the worktree path segment for a pull request.
#[must_use]
pub fn pr_job_id(number: u64) -> String {
    format!("{PR_JOB_PREFIX}{number}")
}

/// Claim one issue or pull request into an isolated worktree.
pub fn claim(manager: &WorktreeManager, request: ClaimRequest<'_>) -> Result<ClaimResult> {
    validate_ref_segment("owner", request.owner)?;
    validate_ref_segment("repo", request.repo)?;
    if request.start_point.is_empty() {
        return Err(invalid_claim("start_point must not be empty"));
    }
    assert_owner_allowed(request.owner, request.allowed_owners)?;

    match request.kind {
        ClaimKind::Issue { number, slug } => claim_issue(manager, request, number, slug),
        ClaimKind::PullRequest {
            number,
            head_branch,
            head_repo,
        } => claim_pr(manager, request, number, head_branch, head_repo),
    }
}

fn claim_issue(
    manager: &WorktreeManager,
    request: ClaimRequest<'_>,
    number: u64,
    slug: Option<&str>,
) -> Result<ClaimResult> {
    require_positive("issue_number", number)?;
    let branch = issue_branch_name(number, slug)?;
    let job_id = issue_job_id(number);
    reject_existing_claim(manager, request.owner, request.repo, &job_id)?;
    let worktree = manager.create_with_request(WorktreeCreateRequest {
        repo_root: request.repo_root,
        owner: request.owner,
        repo: request.repo,
        job_id: &job_id,
        branch: &branch,
        start_point: request.start_point,
    })?;
    finish_claim(request, worktree, &job_id, Some(number), None, true, None)
}

fn claim_pr(
    manager: &WorktreeManager,
    request: ClaimRequest<'_>,
    number: u64,
    head_branch: &str,
    head_repo: Option<&str>,
) -> Result<ClaimResult> {
    require_positive("pr_number", number)?;
    validate_branch_name("head_branch", head_branch)?;
    let head_repo = validate_head_repo(head_repo)?;
    let job_id = pr_job_id(number);
    reject_existing_claim(manager, request.owner, request.repo, &job_id)?;

    let create_request = WorktreeCreateRequest {
        repo_root: request.repo_root,
        owner: request.owner,
        repo: request.repo,
        job_id: &job_id,
        branch: head_branch,
        start_point: request.start_point,
    };
    let worktree = if branch_exists(request.repo_root, head_branch)? {
        manager.attach_with_request(create_request)?
    } else {
        manager.create_with_request(create_request)?
    };
    finish_claim(
        request,
        worktree,
        &job_id,
        None,
        Some(number),
        false,
        head_repo,
    )
}

fn finish_claim(
    request: ClaimRequest<'_>,
    worktree: Worktree,
    job_id: &str,
    issue_number: Option<u64>,
    pr_number: Option<u64>,
    owns_branch: bool,
    head_repo: Option<String>,
) -> Result<ClaimResult> {
    let start_commit = worktree
        .start_commit
        .clone()
        .ok_or_else(|| invalid_claim("worktree create/attach returned no start_commit"))?;
    let head_commit = worktree
        .head_commit
        .clone()
        .ok_or_else(|| invalid_claim("worktree create/attach returned no head_commit"))?;
    Ok(ClaimResult {
        owner: request.owner.to_owned(),
        repo: request.repo.to_owned(),
        job_id: job_id.to_owned(),
        branch: worktree.branch,
        worktree_path: worktree.path,
        issue_number,
        pr_number,
        owns_branch,
        start_commit,
        head_commit,
        head_repo,
    })
}

fn reject_existing_claim(
    manager: &WorktreeManager,
    owner: &str,
    repo: &str,
    job_id: &str,
) -> Result<()> {
    let path = derive_worktree_path(manager.base_path()?, owner, repo, job_id)?;
    if path.exists() {
        return Err(Error::PolicyViolation {
            code: PolicyCode::WorktreeAlreadyClaimed,
            message: format!("worktree already exists for this job: {}", path.display()),
        });
    }
    Ok(())
}

fn assert_owner_allowed(owner: &str, allowed: &[String]) -> Result<()> {
    if allowed.is_empty() {
        return Ok(());
    }
    let allowed_match = allowed
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(owner));
    if allowed_match {
        return Ok(());
    }
    Err(Error::PolicyViolation {
        code: PolicyCode::OwnerNotAllowed,
        message: format!(
            "owner {owner:?} is not in the configured allowlist ({allowed:?}); \
             set WRIT_ALLOWED_OWNERS or pass an explicit allowed-owners list"
        ),
    })
}

fn validate_head_repo(head_repo: Option<&str>) -> Result<Option<String>> {
    let Some(slug) = head_repo else {
        return Ok(None);
    };
    let (owner, repo) = slug
        .split_once('/')
        .ok_or_else(|| invalid_claim(format!("head_repo must be owner/repo, got {slug:?}")))?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err(invalid_claim(format!(
            "head_repo must be owner/repo, got {slug:?}"
        )));
    }
    validate_ref_segment("head_repo.owner", owner)?;
    validate_ref_segment("head_repo.repo", repo)?;
    Ok(Some(slug.to_owned()))
}

fn branch_exists(repo_root: &Path, branch: &str) -> Result<bool> {
    let branch_ref = format!("refs/heads/{branch}");
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("show-ref")
        .arg("--verify")
        .arg("--quiet")
        .arg(&branch_ref)
        .output()
        .map_err(|e| Error::Io {
            context: "check claim branch existence",
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

fn parse_claim_number(text: &str) -> Result<u64> {
    let number: u64 = text
        .parse()
        .map_err(|_| invalid_claim(format!("invalid issue or pull number: {text:?}")))?;
    require_positive("number", number)?;
    Ok(number)
}

fn require_positive(field: &str, number: u64) -> Result<()> {
    if number == 0 {
        return Err(invalid_claim(format!(
            "{field} must be positive, got {number}"
        )));
    }
    Ok(())
}

fn sanitize_slug(raw: &str) -> Result<String> {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in raw.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' {
            if previous_dash || slug.is_empty() {
                continue;
            }
            previous_dash = true;
            slug.push('-');
        } else {
            previous_dash = false;
            slug.push(mapped);
        }
        if slug.len() >= MAX_SLUG_CHARS {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        return Err(invalid_claim(format!(
            "slug {raw:?} sanitizes to an empty branch suffix"
        )));
    }
    Ok(slug)
}

fn validate_ref_segment(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value == "." || value == ".." || value.starts_with('-') {
        return Err(invalid_claim(format!(
            "invalid {field} segment {value:?}: must be a plain name without separators or a leading dash"
        )));
    }
    if value
        .chars()
        .any(|ch| ch == '/' || ch == '\\' || ch == ':' || std::path::is_separator(ch))
    {
        return Err(invalid_claim(format!(
            "invalid {field} segment {value:?}: contains a separator"
        )));
    }
    Ok(())
}

fn validate_branch_name(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') || value.contains('\\') {
        return Err(invalid_claim(format!(
            "invalid {field} {value:?}: must be a plain git ref, not empty or option-looking"
        )));
    }
    if value
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid_claim(format!(
            "invalid {field} {value:?}: empty or dot path component"
        )));
    }
    Ok(())
}

fn invalid_claim(message: impl Into<String>) -> Error {
    Error::InvalidClaim {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    struct Harness {
        _temp: tempfile::TempDir,
        repo_root: PathBuf,
        manager: WorktreeManager,
    }

    impl Harness {
        fn new() -> Self {
            let temp = tempdir().unwrap();
            let repo = temp.path().join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            git(&repo, &["config", "user.name", "Test User"]);
            git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
            let manager = WorktreeManager::with_base(temp.path().join("worktrees")).unwrap();
            Self {
                _temp: temp,
                repo_root: repo,
                manager,
            }
        }

        fn head(&self) -> String {
            git(&self.repo_root, &["rev-parse", "HEAD"])
        }

        fn claim(&self, kind: ClaimKind<'_>, allowed: &[String]) -> Result<ClaimResult> {
            let start = self.head();
            claim(
                &self.manager,
                ClaimRequest {
                    repo_root: &self.repo_root,
                    owner: "acme",
                    repo: "sample",
                    start_point: &start,
                    kind,
                    allowed_owners: allowed,
                },
            )
        }
    }

    fn git(repo: &Path, args: &[&str]) -> String {
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
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn parse_issue_and_pull_urls() {
        let issue =
            parse_github_claim_url("https://github.com/acme/sample/issues/42#issuecomment-1")
                .unwrap();
        assert_eq!(
            issue,
            GitHubClaimTarget {
                owner: "acme".into(),
                repo: "sample".into(),
                kind: ClaimResource::Issue,
                number: 42,
            }
        );
        let pr = parse_github_claim_url("github.com/acme/sample/pull/9/").unwrap();
        assert_eq!(pr.kind, ClaimResource::PullRequest);
        assert_eq!(pr.number, 9);
    }

    #[test]
    fn parse_url_rejects_non_github_and_zero() {
        assert!(parse_github_claim_url("https://example.com/acme/sample/issues/1").is_err());
        assert!(parse_github_claim_url("https://github.com/acme/sample/issues/0").is_err());
        assert!(parse_github_claim_url("https://github.com/acme/sample").is_err());
    }

    #[test]
    fn issue_branch_uses_stable_prefix_and_optional_slug() {
        assert_eq!(issue_branch_name(8, None).unwrap(), "hive/issue-8");
        assert_eq!(
            issue_branch_name(8, Some("Fix CI!")).unwrap(),
            "hive/issue-8-fix-ci"
        );
        assert!(issue_branch_name(0, None).is_err());
        assert!(issue_branch_name(1, Some("---")).is_err());
    }

    #[test]
    fn allowed_owners_treats_star_and_empty_as_unrestricted() {
        assert_eq!(allowed_owners_from(Some("*".into()), None), None);
        assert_eq!(
            allowed_owners_from(Some("acme, example-org".into()), None).unwrap(),
            vec!["acme".to_owned(), "example-org".to_owned()]
        );
        assert_eq!(
            allowed_owners_from(None, Some("legacy-org".into())).unwrap(),
            vec!["legacy-org".to_owned()]
        );
    }

    #[test]
    fn claim_issue_creates_named_branch_from_start_point() {
        let harness = Harness::new();
        let start = harness.head();
        let result = harness
            .claim(
                ClaimKind::Issue {
                    number: 8,
                    slug: Some("short"),
                },
                &[],
            )
            .unwrap();
        assert_eq!(result.job_id, "gh-8");
        assert_eq!(result.branch, "hive/issue-8-short");
        assert_eq!(result.issue_number, Some(8));
        assert!(result.owns_branch);
        assert_eq!(result.start_commit, start);
        assert_eq!(
            git(
                &result.worktree_path,
                &["rev-parse", "--abbrev-ref", "HEAD"]
            ),
            "hive/issue-8-short"
        );
    }

    #[test]
    fn claim_issue_rejects_second_job_for_the_same_id() {
        let harness = Harness::new();
        harness
            .claim(
                ClaimKind::Issue {
                    number: 1,
                    slug: None,
                },
                &[],
            )
            .unwrap();
        let err = harness
            .claim(
                ClaimKind::Issue {
                    number: 1,
                    slug: None,
                },
                &[],
            )
            .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::WorktreeAlreadyClaimed,
                ..
            }
        ));
    }

    #[test]
    fn claim_rejects_owner_outside_allowlist_without_mutation() {
        let harness = Harness::new();
        let allowed = vec!["example-org".to_owned()];
        let err = harness
            .claim(
                ClaimKind::Issue {
                    number: 3,
                    slug: None,
                },
                &allowed,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::OwnerNotAllowed,
                ..
            }
        ));
        assert!(
            git(&harness.repo_root, &["branch", "--list", "hive/issue-3"])
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn claim_pr_attaches_existing_head_without_renaming() {
        let harness = Harness::new();
        let start = harness.head();
        git(&harness.repo_root, &["branch", "feature/pr-head", &start]);
        let result = harness
            .claim(
                ClaimKind::PullRequest {
                    number: 9,
                    head_branch: "feature/pr-head",
                    head_repo: Some("acme/sample"),
                },
                &[],
            )
            .unwrap();
        assert_eq!(result.job_id, "pr-9");
        assert_eq!(result.branch, "feature/pr-head");
        assert!(!result.owns_branch);
        assert_eq!(result.pr_number, Some(9));
        assert_eq!(result.head_repo.as_deref(), Some("acme/sample"));
        assert_eq!(
            git(&result.worktree_path, &["symbolic-ref", "--quiet", "HEAD"]),
            "refs/heads/feature/pr-head"
        );
    }

    #[test]
    fn claim_pr_creates_local_branch_when_head_is_absent() {
        let harness = Harness::new();
        let start = harness.head();
        let result = harness
            .claim(
                ClaimKind::PullRequest {
                    number: 11,
                    head_branch: "feature/from-sha",
                    head_repo: None,
                },
                &[],
            )
            .unwrap();
        assert_eq!(result.branch, "feature/from-sha");
        assert_eq!(result.start_commit, start);
        assert_eq!(
            git(
                &harness.repo_root,
                &["rev-parse", "refs/heads/feature/from-sha"]
            ),
            start
        );
    }

    #[test]
    fn two_claims_on_one_repo_stay_isolated_when_one_is_dirty() {
        let harness = Harness::new();
        let first = harness
            .claim(
                ClaimKind::Issue {
                    number: 1,
                    slug: None,
                },
                &[],
            )
            .unwrap();
        let second = harness
            .claim(
                ClaimKind::Issue {
                    number: 2,
                    slug: None,
                },
                &[],
            )
            .unwrap();
        fs::write(first.worktree_path.join("dirty.txt"), "only first\n").unwrap();
        assert!(!second.worktree_path.join("dirty.txt").exists());
        assert_eq!(git(&second.worktree_path, &["status", "--porcelain"]), "");
        harness.manager.remove(&first.worktree_path, true).unwrap();
        harness.manager.remove(&second.worktree_path, true).unwrap();
        assert!(!first.worktree_path.exists());
        assert!(!second.worktree_path.exists());
    }
}
