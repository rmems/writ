//! Watchlist mutations: add, remove, check, and check-all.

use std::path::Path;

use super::WatchlistError;
use super::classify::classify_snapshot;
use super::probe::{PrProbe, PrSnapshot};
use super::schema::{WatchEntry, WatchKind, WatchStatus, Watchlist, owner_of_repo, repos_match};
use super::stack::{detect_stacks, ordered_identities, rebuild_groups};
use super::store::{
    load_allowed_owners, load_watchlist, mutate_watchlist, owner_is_allowed, save_watchlist,
    utc_now_rfc3339,
};

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
    let ctx = AddContext {
        repo,
        kind,
        reset,
        now: &now,
    };
    for &number in numbers {
        let snapshot = probe.view(repo, number)?;
        insert_or_refresh_entry(list, &ctx, &snapshot, &mut report);
    }
    detect_stacks(list);
    Ok(report)
}

/// Invariant context for one `add` pass. `repo` is the caller-supplied slug
/// (its case is preserved in the report, unlike the probe's canonical value).
struct AddContext<'a> {
    repo: &'a str,
    kind: WatchKind,
    reset: bool,
    now: &'a str,
}

/// Insert a new entry or refresh an existing one from `snapshot`, recording the
/// outcome in `report`. MERGED/CLOSED snapshots are skipped, not persisted.
fn insert_or_refresh_entry(
    list: &mut Watchlist,
    ctx: &AddContext,
    snapshot: &PrSnapshot,
    report: &mut AddReport,
) {
    let AddContext {
        repo,
        kind,
        reset,
        now,
    } = *ctx;
    let number = snapshot.number;
    let github_state = snapshot.state.to_ascii_uppercase();
    if github_state == "MERGED" || github_state == "CLOSED" {
        report
            .skipped
            .push((repo.to_owned(), number, snapshot.state.clone()));
        return;
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
            last_checked: now.to_owned(),
            fix_count: 0,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: Some(snapshot.base.clone()),
            title: Some(snapshot.title.clone()),
            added_at: Some(now.to_owned()),
            check_count: Some(0),
            url: Some(snapshot.url.clone()),
            kind: Some(kind),
            extra: serde_json::Map::new(),
        });
        report.added.push((repo.to_owned(), number));
    }
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
    let scope = CheckScope {
        owner,
        repo,
        numbers,
    };
    let targets = select_check_targets(list, &scope, allowed_owners)?;
    refresh_targets(list, probe, &targets)
}

/// Scope of a check cycle: optional owner/repo/number filters. `unscoped`
/// means a bare `check-all` that must walk every allowlisted owner.
struct CheckScope<'a> {
    owner: Option<&'a str>,
    repo: Option<&'a str>,
    numbers: Option<&'a [u64]>,
}

impl CheckScope<'_> {
    /// True when no explicit filter was given (bare multi-owner `check-all`).
    fn unscoped(&self) -> bool {
        self.repo.is_none() && self.owner.is_none() && self.numbers.is_none()
    }

    /// True when `(target_repo, number)` passes the owner/repo/number filters.
    fn matches_filter(&self, target_repo: &str, number: u64) -> bool {
        self.repo.is_none_or(|want| repos_match(target_repo, want))
            && self.owner.is_none_or(|want| {
                owner_of_repo(target_repo).is_some_and(|have| have.eq_ignore_ascii_case(want))
            })
            && self.numbers.is_none_or(|want| want.contains(&number))
    }
}

fn select_check_targets(
    list: &Watchlist,
    scope: &CheckScope,
    allowed_owners: &[String],
) -> Result<Vec<(String, u64)>, WatchlistError> {
    validate_check_scope(scope, allowed_owners)?;

    let targets: Vec<(String, u64)> = ordered_identities(list)
        .into_iter()
        .filter(|(target_repo, number)| scope.matches_filter(target_repo, *number))
        .filter(|(target_repo, _)| {
            // A bare `check-all` walks only allowlisted owners; a scoped call
            // already validated its explicit owner/repo above.
            !scope.unscoped()
                || owner_of_repo(target_repo)
                    .is_some_and(|have| owner_is_allowed(have, allowed_owners))
        })
        .collect();
    if let Some(want) = scope.numbers {
        for &number in want {
            if !targets.iter().any(|(_, have)| *have == number) {
                return Err(WatchlistError::NotFound {
                    repo: scope.repo.unwrap_or("<unknown>").to_owned(),
                    number,
                });
            }
        }
    }
    Ok(targets)
}

/// Enforce repo validity and the owner allowlist for a check scope.
fn validate_check_scope(
    scope: &CheckScope,
    allowed_owners: &[String],
) -> Result<(), WatchlistError> {
    if let Some(repo) = scope.repo {
        validate_repo(repo)?;
    }
    if scope.unscoped() {
        if allowed_owners.is_empty() {
            return Err(WatchlistError::AllowlistRequired);
        }
        return Ok(());
    }
    if allowed_owners.is_empty() {
        return Ok(());
    }
    if let Some(repo) = scope.repo
        && let Some(have) = owner_of_repo(repo)
        && !owner_is_allowed(have, allowed_owners)
    {
        return Err(WatchlistError::OwnerNotAllowed {
            owner: have.to_owned(),
        });
    }
    if let Some(owner) = scope.owner
        && !owner_is_allowed(owner, allowed_owners)
    {
        return Err(WatchlistError::OwnerNotAllowed {
            owner: owner.to_owned(),
        });
    }
    Ok(())
}

fn refresh_targets(
    list: &mut Watchlist,
    probe: &impl PrProbe,
    targets: &[(String, u64)],
) -> Result<CheckReport, WatchlistError> {
    let now = utc_now_rfc3339();
    let mut pruned = Vec::new();
    let mut checked = Vec::new();
    for target in targets {
        match refresh_one(list, probe, target, &now)? {
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
    target: &(String, u64),
    now: &str,
) -> Result<RefreshOutcome, WatchlistError> {
    let (target_repo, number) = (target.0.as_str(), target.1);
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
    let scope = CheckScope {
        owner,
        repo,
        numbers,
    };
    let targets = select_check_targets(&list, &scope, &owners)?;
    let now = utc_now_rfc3339();
    let mut pruned = Vec::new();
    let mut checked = Vec::new();
    for target in targets {
        match refresh_one(&mut list, probe, &target, &now) {
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

fn validate_repo(repo: &str) -> Result<(), WatchlistError> {
    if owner_of_repo(repo).is_none() {
        return Err(WatchlistError::InvalidInput(format!(
            "repo must be owner/name, got `{repo}`"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
