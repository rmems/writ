//! Watchlist mutations: add, remove, check, check-all, optional pr-babysit import.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::WatchlistError;
use super::schema::{
    StackGroup, WatchEntry, WatchKind, WatchStatus, Watchlist, owner_of_repo, repos_match,
};
use super::store::{
    load_allowed_owners, load_watchlist, mutate_watchlist, owner_is_allowed, save_watchlist,
    utc_now_rfc3339,
};
use std::time::Duration;

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

fn parse_pr_view(repo: &str, stdout: &str) -> Result<PrSnapshot, WatchlistError> {
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

/// Result of adding one or more pull requests.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AddReport {
    /// Newly inserted identities.
    pub added: Vec<(String, u64)>,
    /// Existing identities whose metadata was refreshed.
    pub refreshed: Vec<(String, u64)>,
    /// Skipped MERGED/CLOSED identities.
    pub skipped: Vec<(String, u64, String)>,
}

/// Result of one check cycle.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CheckReport {
    /// Entries still on the watchlist after the cycle, in visit order.
    pub checked: Vec<WatchEntry>,
    /// MERGED/CLOSED identities that were pruned.
    pub pruned: Vec<(String, u64, String)>,
}

/// Add `numbers` in `repo`, resolving metadata through `probe`.
///
/// `add` refreshes branch/base/title but preserves `fix_count` and history
/// unless `reset` is true. Stack-mates stay when a related PR is already
/// present; stack fields are inferred from `base` matching another watched
/// branch in the same repo.
pub fn add_prs(
    list: &mut Watchlist,
    probe: &impl PrProbe,
    repo: &str,
    numbers: &[u64],
    kind: WatchKind,
    reset: bool,
    allowed_owners: &[String],
) -> Result<AddReport, WatchlistError> {
    validate_repo(repo)?;
    if let Some(owner) = owner_of_repo(repo)
        && !allowed_owners.is_empty()
        && !owner_is_allowed(owner, allowed_owners)
    {
        return Err(WatchlistError::OwnerNotAllowed {
            owner: owner.to_owned(),
        });
    }
    let now = utc_now_rfc3339();
    let mut report = AddReport {
        added: Vec::new(),
        refreshed: Vec::new(),
        skipped: Vec::new(),
    };
    for &number in numbers {
        let snapshot = probe.view(repo, number)?;
        let github_state = snapshot.state.to_ascii_uppercase();
        if github_state == "MERGED" || github_state == "CLOSED" {
            report
                .skipped
                .push((repo.to_owned(), number, snapshot.state.clone()));
            continue;
        }
        if let Some(existing) = list.get_mut(repo, number) {
            existing.branch = snapshot.branch.clone();
            existing.base = Some(snapshot.base.clone());
            existing.title = Some(snapshot.title.clone());
            existing.url = Some(snapshot.url.clone());
            if reset {
                existing.fix_count = 0;
                existing.check_count = Some(0);
                existing.residual_blockers.clear();
                existing.status = WatchStatus::Pending;
            }
            report.refreshed.push((repo.to_owned(), number));
        } else {
            list.prs.push(WatchEntry {
                repo: repo.to_owned(),
                number,
                branch: snapshot.branch.clone(),
                status: WatchStatus::Pending,
                last_checked: now.clone(),
                fix_count: 0,
                residual_blockers: Vec::new(),
                stack_id: None,
                stack_type: None,
                stack_position: None,
                base: Some(snapshot.base.clone()),
                title: Some(snapshot.title.clone()),
                added_at: Some(now.clone()),
                check_count: Some(0),
                url: Some(snapshot.url.clone()),
                kind: Some(kind),
                extra: serde_json::Map::new(),
            });
            report.added.push((repo.to_owned(), number));
        }
    }
    detect_stacks(list);
    Ok(report)
}

/// Remove one entry. Stack-mates remain; empty groups are dropped.
pub fn remove_pr(
    list: &mut Watchlist,
    repo: &str,
    number: u64,
) -> Result<WatchEntry, WatchlistError> {
    validate_repo(repo)?;
    let Some(index) = list
        .prs
        .iter()
        .position(|entry| entry.is_identity(repo, number))
    else {
        return Err(WatchlistError::NotFound {
            repo: repo.to_owned(),
            number,
        });
    };
    let removed = list.prs.remove(index);
    rebuild_groups(list);
    Ok(removed)
}

/// Run a check cycle over matching entries: refresh from GitHub, update
/// `last_checked` / `status` / `residual_blockers` / `check_count`, prune
/// MERGED/CLOSED. Does not increment `fix_count` (babysit owns that budget).
///
/// Visit order is stack-bottom first, then remaining PRs by repo and number.
pub fn check_prs(
    list: &mut Watchlist,
    probe: &impl PrProbe,
    owner: Option<&str>,
    repo: Option<&str>,
    numbers: Option<&[u64]>,
    allowed_owners: &[String],
) -> Result<CheckReport, WatchlistError> {
    let targets = select_check_targets(list, owner, repo, numbers, allowed_owners)?;
    refresh_targets(list, probe, &targets)
}

fn select_check_targets(
    list: &Watchlist,
    owner: Option<&str>,
    repo: Option<&str>,
    numbers: Option<&[u64]>,
    allowed_owners: &[String],
) -> Result<Vec<(String, u64)>, WatchlistError> {
    if let Some(repo) = repo {
        validate_repo(repo)?;
    }
    if repo.is_none() && owner.is_none() && numbers.is_none() {
        if allowed_owners.is_empty() {
            return Err(WatchlistError::AllowlistRequired);
        }
    } else if !allowed_owners.is_empty() {
        if let Some(repo) = repo
            && let Some(have) = owner_of_repo(repo)
            && !owner_is_allowed(have, allowed_owners)
        {
            return Err(WatchlistError::OwnerNotAllowed {
                owner: have.to_owned(),
            });
        }
        if let Some(owner) = owner
            && !owner_is_allowed(owner, allowed_owners)
        {
            return Err(WatchlistError::OwnerNotAllowed {
                owner: owner.to_owned(),
            });
        }
    }

    let targets: Vec<(String, u64)> = ordered_identities(list)
        .into_iter()
        .filter(|(target_repo, number)| {
            repo.is_none_or(|want| repos_match(target_repo, want))
                && owner.is_none_or(|want| {
                    owner_of_repo(target_repo).is_some_and(|have| have.eq_ignore_ascii_case(want))
                })
                && numbers.is_none_or(|want| want.contains(number))
        })
        .filter(|(target_repo, _)| {
            if repo.is_none() && owner.is_none() && numbers.is_none() {
                owner_of_repo(target_repo)
                    .is_some_and(|have| owner_is_allowed(have, allowed_owners))
            } else {
                true
            }
        })
        .collect();
    if let Some(want) = numbers {
        for &number in want {
            if !targets.iter().any(|(_, have)| *have == number) {
                return Err(WatchlistError::NotFound {
                    repo: repo.unwrap_or("<unknown>").to_owned(),
                    number,
                });
            }
        }
    }
    Ok(targets)
}

fn refresh_targets(
    list: &mut Watchlist,
    probe: &impl PrProbe,
    targets: &[(String, u64)],
) -> Result<CheckReport, WatchlistError> {
    let now = utc_now_rfc3339();
    let mut pruned = Vec::new();
    let mut checked = Vec::new();
    for (target_repo, number) in targets {
        match refresh_one(list, probe, target_repo, *number, &now)? {
            RefreshOutcome::Checked(entry) => checked.push(*entry),
            RefreshOutcome::Pruned {
                repo,
                number,
                state,
            } => pruned.push((repo, number, state)),
        }
    }
    detect_stacks(list);
    Ok(CheckReport { checked, pruned })
}

enum RefreshOutcome {
    Checked(Box<WatchEntry>),
    Pruned {
        repo: String,
        number: u64,
        state: String,
    },
}

fn refresh_one(
    list: &mut Watchlist,
    probe: &impl PrProbe,
    target_repo: &str,
    number: u64,
    now: &str,
) -> Result<RefreshOutcome, WatchlistError> {
    match probe.view(target_repo, number) {
        Ok(snapshot) => {
            let github_state = snapshot.state.to_ascii_uppercase();
            if github_state == "MERGED" || github_state == "CLOSED" {
                let _ = remove_pr(list, target_repo, number);
                return Ok(RefreshOutcome::Pruned {
                    repo: target_repo.to_owned(),
                    number,
                    state: snapshot.state,
                });
            }
            let (status, blockers) = classify_snapshot(&snapshot);
            let Some(entry) = list.get_mut(target_repo, number) else {
                return Err(WatchlistError::NotFound {
                    repo: target_repo.to_owned(),
                    number,
                });
            };
            entry.branch = snapshot.branch;
            entry.base = Some(snapshot.base);
            entry.title = Some(snapshot.title);
            entry.url = Some(snapshot.url);
            entry.status = status;
            entry.residual_blockers = blockers;
            entry.last_checked = now.to_owned();
            entry.check_count = Some(entry.check_count.unwrap_or(0).saturating_add(1));
            Ok(RefreshOutcome::Checked(Box::new(entry.clone())))
        }
        Err(WatchlistError::Timeout { .. }) => {
            let Some(entry) = list.get_mut(target_repo, number) else {
                return Err(WatchlistError::NotFound {
                    repo: target_repo.to_owned(),
                    number,
                });
            };
            entry.status = WatchStatus::Timeout;
            entry.residual_blockers = vec!["timeout:gh".to_owned()];
            entry.last_checked = now.to_owned();
            entry.check_count = Some(entry.check_count.unwrap_or(0).saturating_add(1));
            Ok(RefreshOutcome::Checked(Box::new(entry.clone())))
        }
        Err(err) => Err(err),
    }
}

/// Persist `add_prs` against `path`.
pub fn add_prs_at(
    path: &Path,
    probe: &impl PrProbe,
    repo: &str,
    numbers: &[u64],
    kind: WatchKind,
    reset: bool,
    allowed_owners: Option<&[String]>,
) -> Result<AddReport, WatchlistError> {
    let owners = allowed_owners
        .map(ToOwned::to_owned)
        .unwrap_or_else(load_allowed_owners);
    mutate_watchlist(path, |list| {
        add_prs(list, probe, repo, numbers, kind, reset, &owners)
    })
}

/// Persist `remove_pr` against `path`.
pub fn remove_pr_at(path: &Path, repo: &str, number: u64) -> Result<WatchEntry, WatchlistError> {
    mutate_watchlist(path, |list| remove_pr(list, repo, number))
}

/// Load and filter without writing.
pub fn list_prs_at(
    path: &Path,
    owner: Option<&str>,
    repo: Option<&str>,
) -> Result<Vec<WatchEntry>, WatchlistError> {
    let list = load_watchlist(path)?;
    Ok(list.filtered(owner, repo).into_iter().cloned().collect())
}

/// Persist `check_prs` against `path`.
pub fn check_prs_at(
    path: &Path,
    probe: &impl PrProbe,
    owner: Option<&str>,
    repo: Option<&str>,
    numbers: Option<&[u64]>,
    allowed_owners: Option<&[String]>,
) -> Result<CheckReport, WatchlistError> {
    let owners = allowed_owners
        .map(ToOwned::to_owned)
        .unwrap_or_else(load_allowed_owners);
    let mut list = load_watchlist(path)?;
    let targets = select_check_targets(&list, owner, repo, numbers, &owners)?;
    let now = utc_now_rfc3339();
    let mut pruned = Vec::new();
    let mut checked = Vec::new();
    for (target_repo, number) in targets {
        match refresh_one(&mut list, probe, &target_repo, number, &now) {
            Ok(RefreshOutcome::Checked(entry)) => checked.push(*entry),
            Ok(RefreshOutcome::Pruned {
                repo,
                number,
                state,
            }) => pruned.push((repo, number, state)),
            Err(err) => {
                detect_stacks(&mut list);
                save_watchlist(path, &list)?;
                return Err(err);
            }
        }
        detect_stacks(&mut list);
        save_watchlist(path, &list)?;
    }
    Ok(CheckReport { checked, pruned })
}

/// Read-only import from a pr-babysit `watched-prs.json`. The source file is
/// never written. Identity is still `(repo, number)`; existing hive history
/// (`fix_count`) is preserved.
pub fn import_pr_babysit(
    list: &mut Watchlist,
    source_path: &Path,
) -> Result<AddReport, WatchlistError> {
    let data = fs::read_to_string(source_path).map_err(|err| WatchlistError::Io {
        context: "read pr-babysit state",
        path: source_path.to_path_buf(),
        source: err,
    })?;
    let parsed: PrBabysitFile = serde_json::from_str(&data).map_err(|err| WatchlistError::Gh {
        repo: source_path.display().to_string(),
        number: 0,
        message: format!("failed to parse pr-babysit JSON: {err}"),
    })?;
    let now = utc_now_rfc3339();
    let mut report = AddReport {
        added: Vec::new(),
        refreshed: Vec::new(),
        skipped: Vec::new(),
    };
    for incoming in parsed.prs {
        let Some(repo) = incoming.repo.clone() else {
            continue;
        };
        // Reject malformed identities: a repo that is not `owner/name` would
        // break owner filtering and later refreshes, so never persist it.
        if owner_of_repo(&repo).is_none() {
            continue;
        }
        let status = map_imported_status(incoming.last_status.as_deref());
        if let Some(existing) = list.get_mut(&repo, incoming.number) {
            // Refresh source fields from the incoming record but preserve the
            // hive budget (`fix_count`) already accrued locally.
            if let Some(branch) = incoming.branch.clone() {
                existing.branch = branch;
            }
            existing.base = incoming.base.clone().or_else(|| existing.base.clone());
            existing.title = incoming.title.clone().or_else(|| existing.title.clone());
            existing.url = incoming.url.clone().or_else(|| existing.url.clone());
            existing.stack_id = incoming
                .stack_id
                .clone()
                .or_else(|| existing.stack_id.clone());
            existing.stack_type = incoming
                .stack_type
                .clone()
                .or_else(|| existing.stack_type.clone());
            existing.stack_position = incoming.stack_position.or(existing.stack_position);
            if let Some(added_at) = incoming.added_at.clone() {
                existing.added_at = Some(added_at);
            }
            existing.check_count = incoming.check_count.or(existing.check_count);
            existing.status = status;
            report.refreshed.push((repo, incoming.number));
            continue;
        }
        list.prs.push(WatchEntry {
            repo: repo.clone(),
            number: incoming.number,
            branch: incoming.branch.unwrap_or_default(),
            status,
            last_checked: incoming.last_checked.clone().unwrap_or_else(|| now.clone()),
            fix_count: incoming.fix_count.unwrap_or(0),
            residual_blockers: Vec::new(),
            stack_id: incoming.stack_id.clone(),
            stack_type: incoming.stack_type.clone(),
            stack_position: incoming.stack_position,
            base: incoming.base.clone(),
            title: incoming.title.clone(),
            added_at: incoming.added_at.clone().or_else(|| Some(now.clone())),
            check_count: incoming.check_count,
            url: incoming.url.clone(),
            kind: Some(WatchKind::PrBabysit),
            extra: serde_json::Map::new(),
        });
        report.added.push((repo, incoming.number));
    }
    detect_stacks(list);
    Ok(report)
}

/// Persist [`import_pr_babysit`] against `path`.
pub fn import_pr_babysit_at(path: &Path, source_path: &Path) -> Result<AddReport, WatchlistError> {
    mutate_watchlist(path, |list| import_pr_babysit(list, source_path))
}

/// Default pr-babysit path under the user data directory (read-only).
#[must_use]
pub fn default_pr_babysit_path() -> std::path::PathBuf {
    crate::paths::user_data_dir()
        .join("pr-babysit")
        .join("watched-prs.json")
}

#[derive(Debug, Deserialize)]
struct PrBabysitFile {
    #[serde(default)]
    prs: Vec<PrBabysitEntry>,
}

#[derive(Debug, Deserialize)]
struct PrBabysitEntry {
    number: u64,
    repo: Option<String>,
    branch: Option<String>,
    stack_id: Option<String>,
    stack_type: Option<String>,
    stack_position: Option<u32>,
    base: Option<String>,
    title: Option<String>,
    added_at: Option<String>,
    last_checked: Option<String>,
    last_status: Option<String>,
    check_count: Option<u32>,
    fix_count: Option<u32>,
    url: Option<String>,
}

fn map_imported_status(last_status: Option<&str>) -> WatchStatus {
    match last_status.map(str::to_ascii_lowercase).as_deref() {
        Some("healthy" | "success" | "pass") => WatchStatus::Healthy,
        Some("failed" | "fail" | "failure") => WatchStatus::Failed,
        Some("residual") => WatchStatus::Residual,
        Some("conflict") => WatchStatus::Conflict,
        Some("timeout") => WatchStatus::Timeout,
        _ => WatchStatus::Pending,
    }
}

pub(crate) fn classify_snapshot(snapshot: &PrSnapshot) -> (WatchStatus, Vec<String>) {
    let mut blockers = Vec::new();
    let mergeable = snapshot
        .mergeable
        .as_deref()
        .unwrap_or("")
        .to_ascii_uppercase();
    if mergeable == "CONFLICTING" {
        blockers.push("conflict:mergeable".to_owned());
        return (WatchStatus::Conflict, blockers);
    }

    let mut failed = false;
    let mut pending = false;

    // A draft PR is not ready to merge regardless of CI, so it cannot be
    // Healthy; surface it as pending with an explicit blocker.
    if snapshot.is_draft {
        pending = true;
        blockers.push("draft:true".to_owned());
    }

    // GitHub reports `mergeable == UNKNOWN` (or empty/missing) while the merge
    // is still being computed. Do not declare an unresolved merge Healthy;
    // treat it as pending until it resolves to MERGEABLE or CONFLICTING.
    if mergeable != "MERGEABLE" {
        pending = true;
        blockers.push("pending:mergeable_unknown".to_owned());
    }
    for check in &snapshot.checks {
        let state = check.state.to_ascii_uppercase();
        if matches!(
            state.as_str(),
            "FAILURE" | "FAIL" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "STARTUP_FAILURE" | "STALE"
        ) {
            failed = true;
            blockers.push(format!("class_a:{}", sanitize_token(&check.name)));
        } else if state == "ACTION_REQUIRED" {
            blockers.push(format!("class_b:{}", sanitize_token(&check.name)));
        } else if matches!(
            state.as_str(),
            "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | "UNKNOWN"
        ) {
            pending = true;
            blockers.push(format!("class_c:{}", sanitize_token(&check.name)));
        }
    }

    if let Some(decision) = snapshot.review_decision.as_deref() {
        let decision = decision.to_ascii_uppercase();
        if decision == "REVIEW_REQUIRED" || decision == "CHANGES_REQUESTED" {
            blockers.push(format!("review:{}", sanitize_token(&decision)));
        }
    }

    if failed {
        (WatchStatus::Failed, blockers)
    } else if pending || (snapshot.checks.is_empty() && blockers.is_empty()) {
        (WatchStatus::Pending, blockers)
    } else if !blockers.is_empty() {
        (WatchStatus::Residual, blockers)
    } else {
        (WatchStatus::Healthy, blockers)
    }
}

fn sanitize_token(name: &str) -> String {
    let token: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if token.is_empty() {
        "unnamed".to_owned()
    } else {
        token
    }
}

fn validate_repo(repo: &str) -> Result<(), WatchlistError> {
    if owner_of_repo(repo).is_none() {
        return Err(WatchlistError::InvalidInput(format!(
            "repo must be owner/name, got `{repo}`"
        )));
    }
    Ok(())
}

fn ordered_identities(list: &Watchlist) -> Vec<(String, u64)> {
    let mut stacked: Vec<(u32, String, u64)> = list
        .prs
        .iter()
        .filter(|entry| entry.stack_id.is_some())
        .map(|entry| {
            (
                entry.stack_position.unwrap_or(0),
                entry.repo.clone(),
                entry.number,
            )
        })
        .collect();
    stacked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    let mut rest: Vec<(String, u64)> = list
        .prs
        .iter()
        .filter(|entry| entry.stack_id.is_none())
        .map(|entry| (entry.repo.clone(), entry.number))
        .collect();
    rest.sort();

    let mut out: Vec<(String, u64)> = stacked
        .into_iter()
        .map(|(_, repo, number)| (repo, number))
        .collect();
    out.extend(rest);
    out
}

fn detect_stacks(list: &mut Watchlist) {
    let n = list.prs.len();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    for (i, entry) in list.prs.iter().enumerate() {
        let Some(base) = entry.base.as_deref() else {
            continue;
        };
        parent[i] = list.prs.iter().enumerate().find_map(|(j, other)| {
            (j != i && repos_match(&other.repo, &entry.repo) && other.branch == base).then_some(j)
        });
    }
    let mut has_child = vec![false; n];
    for parent_idx in parent.iter().flatten() {
        has_child[*parent_idx] = true;
    }
    let preserved: Vec<Option<String>> = list
        .prs
        .iter()
        .map(|entry| entry.stack_id.clone().filter(|id| !id.is_empty()))
        .collect();

    for i in 0..n {
        if parent[i].is_none() && !has_child[i] {
            list.prs[i].stack_id = None;
            list.prs[i].stack_position = None;
            continue;
        }
        let mut root = i;
        let mut pos = 0_u32;
        let mut seen = vec![false; n];
        while let Some(next) = parent[root] {
            if seen[root] {
                break;
            }
            seen[root] = true;
            root = next;
            pos = pos.saturating_add(1);
        }
        let stack_id = preserved[root]
            .clone()
            .unwrap_or_else(|| format!("{}#{}", list.prs[root].repo, list.prs[root].number));
        list.prs[i].stack_id = Some(stack_id);
        list.prs[i].stack_position = Some(pos);
    }
    rebuild_groups(list);
}

fn rebuild_groups(list: &mut Watchlist) {
    let mut groups: BTreeMap<String, StackGroup> = BTreeMap::new();
    for entry in &list.prs {
        let Some(stack_id) = entry.stack_id.as_ref() else {
            continue;
        };
        let group = groups
            .entry(stack_id.clone())
            .or_insert_with(|| StackGroup {
                repo: entry.repo.clone(),
                numbers: Vec::new(),
            });
        if !group.numbers.contains(&entry.number) {
            group.numbers.push(entry.number);
        }
    }
    for group in groups.values_mut() {
        group.numbers.sort_by_key(|number| {
            list.prs
                .iter()
                .find(|entry| entry.number == *number && repos_match(&entry.repo, &group.repo))
                .and_then(|entry| entry.stack_position)
                .unwrap_or(0)
        });
    }
    list.groups = groups;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watchlist::schema::Watchlist;

    struct MapProbe {
        snaps: BTreeMap<(String, u64), PrSnapshot>,
    }

    impl PrProbe for MapProbe {
        fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
            self.snaps
                .iter()
                .find_map(|((have_repo, have_number), snap)| {
                    (*have_number == number && repos_match(have_repo, repo)).then(|| snap.clone())
                })
                .ok_or_else(|| WatchlistError::NotFound {
                    repo: repo.to_owned(),
                    number,
                })
        }
    }

    fn open_snap(repo: &str, number: u64, branch: &str, base: &str) -> PrSnapshot {
        PrSnapshot {
            repo: repo.to_owned(),
            number,
            branch: branch.to_owned(),
            base: base.to_owned(),
            title: format!("PR {number}"),
            url: format!("https://example.test/{repo}/pull/{number}"),
            state: "OPEN".to_owned(),
            mergeable: Some("MERGEABLE".to_owned()),
            review_decision: None,
            is_draft: false,
            checks: vec![CheckSnapshot {
                name: "ci".to_owned(),
                state: "SUCCESS".to_owned(),
            }],
        }
    }

    #[test]
    fn add_skips_merged_and_dedupes() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/a", "main"),
        );
        probe.snaps.insert(("acme/widgets".to_owned(), 2), {
            let mut snap = open_snap("acme/widgets", 2, "feat/b", "main");
            snap.state = "MERGED".to_owned();
            snap
        });
        let mut list = Watchlist::default();
        let first = add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1, 2],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(first.added, vec![("acme/widgets".to_owned(), 1)]);
        assert_eq!(
            first.skipped,
            vec![("acme/widgets".to_owned(), 2, "MERGED".to_owned())]
        );
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/a-renamed", "main"),
        );
        list.get_mut("acme/widgets", 1).unwrap().fix_count = 2;
        let second = add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(second.refreshed, vec![("acme/widgets".to_owned(), 1)]);
        assert_eq!(
            list.get("acme/widgets", 1).unwrap().branch,
            "feat/a-renamed"
        );
        assert_eq!(list.get("acme/widgets", 1).unwrap().fix_count, 2);
    }

    #[test]
    fn add_rejects_disallowed_owner_when_allowlist_set() {
        let probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        let mut list = Watchlist::default();
        let err = add_prs(
            &mut list,
            &probe,
            "evil/repo",
            &[1],
            WatchKind::PrBabysit,
            false,
            &["acme".to_owned()],
        )
        .unwrap_err();
        assert!(matches!(err, WatchlistError::OwnerNotAllowed { .. }));
    }

    #[test]
    fn remove_keeps_stack_mates() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/base", "main"),
        );
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 2),
            open_snap("acme/widgets", 2, "feat/child", "feat/base"),
        );
        let mut list = Watchlist::default();
        add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1, 2],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        assert!(list.get("acme/widgets", 2).unwrap().stack_id.is_some());
        remove_pr(&mut list, "acme/widgets", 2).unwrap();
        assert!(list.get("acme/widgets", 1).is_some());
        assert!(list.get("acme/widgets", 2).is_none());
    }

    #[test]
    fn check_all_requires_allowlist_and_prunes_merged() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/a", "main"),
        );
        probe.snaps.insert(("acme/widgets".to_owned(), 2), {
            let mut snap = open_snap("acme/widgets", 2, "feat/b", "main");
            snap.state = "CLOSED".to_owned();
            snap
        });
        probe.snaps.insert(
            ("other/repo".to_owned(), 9),
            open_snap("other/repo", 9, "feat/c", "main"),
        );
        let mut list = Watchlist::default();
        add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        list.prs.push(WatchEntry {
            repo: "acme/widgets".to_owned(),
            number: 2,
            branch: "feat/b".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 1,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: None,
            title: None,
            added_at: None,
            check_count: Some(0),
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        });
        list.prs.push(WatchEntry {
            repo: "other/repo".to_owned(),
            number: 9,
            branch: "feat/c".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 0,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: None,
            title: None,
            added_at: None,
            check_count: Some(0),
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        });

        let err = check_prs(&mut list, &probe, None, None, None, &[]).unwrap_err();
        assert!(matches!(err, WatchlistError::AllowlistRequired));

        let report = check_prs(&mut list, &probe, None, None, None, &["acme".to_owned()]).unwrap();
        assert_eq!(
            report.pruned,
            vec![("acme/widgets".to_owned(), 2, "CLOSED".to_owned())]
        );
        assert_eq!(report.checked.len(), 1);
        assert_eq!(report.checked[0].status, WatchStatus::Healthy);
        assert!(list.get("other/repo", 9).is_some());
        assert!(list.get("acme/widgets", 2).is_none());
        assert_eq!(list.get("acme/widgets", 1).unwrap().fix_count, 0);
        assert_eq!(list.get("acme/widgets", 1).unwrap().check_count, Some(1));
    }

    #[test]
    fn classify_maps_fail_conflict_review() {
        let mut snap = open_snap("acme/widgets", 1, "feat/a", "main");
        snap.mergeable = Some("CONFLICTING".to_owned());
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Conflict);
        assert_eq!(blockers, vec!["conflict:mergeable".to_owned()]);

        snap.mergeable = Some("MERGEABLE".to_owned());
        snap.checks.push(CheckSnapshot {
            name: "ci / test".to_owned(),
            state: "FAILURE".to_owned(),
        });
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Failed);
        assert_eq!(blockers, vec!["class_a:ci___test".to_owned()]);

        snap.checks = vec![CheckSnapshot {
            name: "codacy".to_owned(),
            state: "ACTION_REQUIRED".to_owned(),
        }];
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Residual);
        assert_eq!(blockers, vec!["class_b:codacy".to_owned()]);

        snap.checks = vec![CheckSnapshot {
            name: "setup".to_owned(),
            state: "STARTUP_FAILURE".to_owned(),
        }];
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Failed);
        assert_eq!(blockers, vec!["class_a:setup".to_owned()]);

        snap.checks = vec![CheckSnapshot {
            name: "ci".to_owned(),
            state: "STALE".to_owned(),
        }];
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Failed);
        assert_eq!(blockers, vec!["class_a:ci".to_owned()]);
    }

    #[test]
    fn classify_empty_rollup_is_pending() {
        let mut snap = open_snap("acme/widgets", 1, "feat/a", "main");
        snap.checks.clear();
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Pending);
        assert!(blockers.is_empty());

        snap.review_decision = Some("REVIEW_REQUIRED".to_owned());
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Residual);
        assert_eq!(blockers, vec!["review:review_required".to_owned()]);
    }

    #[test]
    fn parse_pr_view_empty_conclusion_is_pending() {
        let json = r#"{
            "number": 7,
            "title": "wip",
            "url": "https://example.test/acme/widgets/pull/7",
            "state": "OPEN",
            "headRefName": "feat/a",
            "baseRefName": "main",
            "mergeable": "MERGEABLE",
            "reviewDecision": null,
            "isDraft": false,
            "statusCheckRollup": [
                {"name": "ci", "conclusion": "", "status": "IN_PROGRESS", "state": ""}
            ]
        }"#;
        let snap = parse_pr_view("acme/widgets", json).unwrap();
        assert_eq!(snap.checks[0].state, "IN_PROGRESS");
        let (status, _) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Pending);
    }

    #[test]
    fn classify_mergeable_unknown_is_pending() {
        let mut snap = open_snap("acme/widgets", 1, "feat/a", "main");
        snap.mergeable = Some("UNKNOWN".to_owned());
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Pending);
        assert!(blockers.contains(&"pending:mergeable_unknown".to_owned()));
    }

    #[test]
    fn classify_draft_is_pending() {
        let mut snap = open_snap("acme/widgets", 1, "feat/a", "main");
        snap.is_draft = true;
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Pending);
        assert!(blockers.contains(&"draft:true".to_owned()));
    }

    #[test]
    fn probe_timeout_maps_to_watchlist_timeout() {
        // A stalled child must surface as WatchlistError::Timeout. `sleep 5`
        // is not gh, so we exercise the mapping via a tiny probe that reuses
        // the same GhRun::TimedOut -> Timeout logic shape.
        struct SlowProbe;
        impl PrProbe for SlowProbe {
            fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
                use crate::git_safe::{GhRun, SafeGhCommand};
                // `version` is allowlisted and returns fast; force the timeout
                // path with a zero deadline to assert the mapping.
                let cmd = SafeGhCommand::new(&[
                    "pr".to_owned(),
                    "view".to_owned(),
                    number.to_string(),
                    "--repo".to_owned(),
                    repo.to_owned(),
                ])?;
                match cmd.run_with_timeout(Duration::from_millis(0)) {
                    Ok(GhRun::TimedOut { timeout }) => Err(WatchlistError::Timeout {
                        repo: repo.to_owned(),
                        number,
                        message: format!("gh exceeded {}s deadline", timeout.as_secs()),
                    }),
                    Ok(GhRun::Completed(_)) => Ok(open_snap(repo, number, "feat/a", "main")),
                    Err(err) => Err(err.into()),
                }
            }
        }
        let err = SlowProbe.view("acme/widgets", 1).unwrap_err();
        assert!(
            matches!(err, WatchlistError::Timeout { number: 1, .. }),
            "expected timeout, got {err:?}"
        );
    }

    #[test]
    fn import_skips_malformed_repo() {
        let dir = std::env::temp_dir().join(format!(
            "watchlist-import-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("watched-prs.json");
        fs::write(
            &source,
            r#"{"prs": [{"number": 1, "repo": "not-a-slug", "branch": "x"}]}"#,
        )
        .unwrap();
        let mut list = Watchlist::default();
        let report = import_pr_babysit(&mut list, &source).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert!(report.added.is_empty());
        assert!(list.prs.is_empty());
    }

    #[test]
    fn import_refreshes_existing_and_preserves_fix_count() {
        let dir = std::env::temp_dir().join(format!(
            "watchlist-import-refresh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("watched-prs.json");
        fs::write(
            &source,
            r#"{"prs": [{"number": 5, "repo": "acme/widgets", "branch": "feat/new",
                "title": "Fresh title", "url": "https://example.test/x",
                "check_count": 9, "last_status": "healthy"}]}"#,
        )
        .unwrap();
        let mut list = Watchlist::default();
        list.prs.push(WatchEntry {
            repo: "acme/widgets".to_owned(),
            number: 5,
            branch: "feat/old".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 3,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: Some("main".to_owned()),
            title: Some("Old title".to_owned()),
            added_at: Some("2026-01-01T00:00:00Z".to_owned()),
            check_count: Some(1),
            url: None,
            kind: Some(WatchKind::PrBabysit),
            extra: serde_json::Map::new(),
        });
        let report = import_pr_babysit(&mut list, &source).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(report.refreshed, vec![("acme/widgets".to_owned(), 5)]);
        let entry = list.get("acme/widgets", 5).unwrap();
        assert_eq!(entry.branch, "feat/new");
        assert_eq!(entry.title.as_deref(), Some("Fresh title"));
        assert_eq!(entry.url.as_deref(), Some("https://example.test/x"));
        assert_eq!(entry.check_count, Some(9));
        assert_eq!(entry.status, WatchStatus::Healthy);
        // Hive budget preserved.
        assert_eq!(entry.fix_count, 3);
    }

    #[test]
    fn three_deep_stack_is_stable_regardless_of_add_order() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 3),
            open_snap("acme/widgets", 3, "feat/top", "feat/mid"),
        );
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 2),
            open_snap("acme/widgets", 2, "feat/mid", "feat/base"),
        );
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/base", "main"),
        );
        let mut list = Watchlist::default();
        add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[3, 2, 1],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        let bottom = list.get("acme/widgets", 1).unwrap();
        let mid = list.get("acme/widgets", 2).unwrap();
        let top = list.get("acme/widgets", 3).unwrap();
        assert_eq!(bottom.stack_id, mid.stack_id);
        assert_eq!(mid.stack_id, top.stack_id);
        assert_eq!(bottom.stack_position, Some(0));
        assert_eq!(mid.stack_position, Some(1));
        assert_eq!(top.stack_position, Some(2));
        let group = list.groups.get(bottom.stack_id.as_ref().unwrap()).unwrap();
        assert_eq!(group.numbers, vec![1, 2, 3]);
    }

    #[test]
    fn filtered_check_rejects_disallowed_owner() {
        let probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        let mut list = Watchlist::default();
        list.prs.push(WatchEntry {
            repo: "evil/repo".to_owned(),
            number: 1,
            branch: "feat/x".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 0,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: None,
            title: None,
            added_at: None,
            check_count: Some(0),
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        });
        let err = check_prs(
            &mut list,
            &probe,
            None,
            Some("evil/repo"),
            None,
            &["acme".to_owned()],
        )
        .unwrap_err();
        assert!(matches!(err, WatchlistError::OwnerNotAllowed { .. }));
    }

    #[test]
    fn check_missing_identity_is_not_found() {
        let probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        let mut list = Watchlist::default();
        let err = check_prs(
            &mut list,
            &probe,
            None,
            Some("acme/widgets"),
            Some(&[41]),
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, WatchlistError::NotFound { number: 41, .. }));

        let err = check_prs(
            &mut list,
            &probe,
            None,
            Some("not-a-slug"),
            Some(&[41]),
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, WatchlistError::InvalidInput(_)));
    }

    #[test]
    fn add_and_check_treat_repo_slug_case_as_identity() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 41),
            open_snap("acme/widgets", 41, "feat/a", "main"),
        );
        let mut list = Watchlist::default();
        add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[41],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        let second = add_prs(
            &mut list,
            &probe,
            "ACME/widgets",
            &[41],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(second.refreshed, vec![("ACME/widgets".to_owned(), 41)]);
        assert!(second.added.is_empty());
        assert_eq!(list.prs.len(), 1);

        let report = check_prs(
            &mut list,
            &probe,
            None,
            Some("ACME/Widgets"),
            Some(&[41]),
            &[],
        )
        .unwrap();
        assert_eq!(report.checked.len(), 1);
        assert_eq!(report.checked[0].repo, "acme/widgets");
        remove_pr(&mut list, "Acme/widgets", 41).unwrap();
        assert!(list.prs.is_empty());
    }

    #[test]
    fn multi_owner_entries_coexist() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "a", "main"),
        );
        probe.snaps.insert(
            ("example-org/core".to_owned(), 1),
            open_snap("example-org/core", 1, "b", "main"),
        );
        let mut list = Watchlist::default();
        add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        add_prs(
            &mut list,
            &probe,
            "example-org/core",
            &[1],
            WatchKind::IssueToPr,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(list.prs.len(), 2);
        assert_eq!(list.filtered(Some("acme"), None).len(), 1);
        assert_eq!(list.filtered(None, Some("example-org/core")).len(), 1);
    }

    struct FailSecondProbe {
        first: PrSnapshot,
    }

    impl PrProbe for FailSecondProbe {
        fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
            if number == 1 {
                Ok(self.first.clone())
            } else {
                Err(WatchlistError::Gh {
                    repo: repo.to_owned(),
                    number,
                    message: "boom".to_owned(),
                })
            }
        }
    }

    #[test]
    fn check_persists_completed_entries_before_gh_failure() {
        let dir = std::env::temp_dir().join(format!(
            "watchlist-partial-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watchlist.json");
        let mut list = Watchlist::default();
        list.prs.push(WatchEntry {
            repo: "acme/widgets".to_owned(),
            number: 1,
            branch: "feat/a".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 0,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: None,
            title: None,
            added_at: None,
            check_count: Some(0),
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        });
        list.prs.push(WatchEntry {
            repo: "acme/widgets".to_owned(),
            number: 2,
            branch: "feat/b".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 0,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: None,
            title: None,
            added_at: None,
            check_count: Some(0),
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        });
        crate::watchlist::save_watchlist(&path, &list).unwrap();
        let probe = FailSecondProbe {
            first: open_snap("acme/widgets", 1, "feat/a", "main"),
        };
        let err =
            check_prs_at(&path, &probe, None, Some("acme/widgets"), None, Some(&[])).unwrap_err();
        assert!(matches!(err, WatchlistError::Gh { number: 2, .. }));
        let loaded = crate::watchlist::load_watchlist(&path).unwrap();
        let first = loaded.get("acme/widgets", 1).unwrap();
        assert_eq!(first.status, WatchStatus::Healthy);
        assert_eq!(first.check_count, Some(1));
        assert_eq!(
            loaded.get("acme/widgets", 2).unwrap().status,
            WatchStatus::Pending
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
