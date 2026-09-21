//! Assemble watchlist rows from leases + optional coord + live GitHub.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::lease::{Lease, LeaseMode, LeaseStore};

use super::classify::classify_snapshot;
use super::coord_read::{CoordSnapshot, JobId, load_coord_snapshot};
use super::github::{GithubProbe, PrSnapshot};
use super::types::{
    CollabStatus, CoordOverlay, GithubState, RecoveryStatus, WatchEntry, WatchlistData,
};

/// Filters and probe flags for one watchlist command.
#[derive(Debug, Clone, Default)]
pub struct WatchQuery {
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub job_id: Option<String>,
    pub include_released: bool,
    pub probe_github: bool,
}

/// Load the collaboration view. Never writes leases, coord tables, or JSON state.
pub fn load_view(
    store: &LeaseStore,
    query: &WatchQuery,
    probe: Option<&dyn GithubProbe>,
) -> Result<WatchlistData> {
    let leases = if query.include_released {
        store.list_all()?
    } else {
        store.list_active()?
    };
    let coord = load_coord_snapshot(store.path());
    let mut entries = Vec::new();
    for lease in leases {
        if !matches_filter(&lease, query) {
            continue;
        }
        entries.push(entry_from_lease(&lease, &coord, query, probe));
    }
    Ok(WatchlistData {
        entries,
        coord_available: coord.available,
        github_probed: query.probe_github,
    })
}

fn matches_filter(lease: &Lease, query: &WatchQuery) -> bool {
    if let Some(owner) = query.owner.as_deref()
        && !lease.owner.eq_ignore_ascii_case(owner)
    {
        return false;
    }
    if let Some(repo) = query.repo.as_deref()
        && !repo_matches(lease, repo)
    {
        return false;
    }
    if let Some(job_id) = query.job_id.as_deref()
        && lease.job_id != job_id
    {
        return false;
    }
    true
}

fn repo_matches(lease: &Lease, repo: &str) -> bool {
    if repo.contains('/') {
        let expected = format!("{}/{}", lease.owner, lease.repo_name);
        expected.eq_ignore_ascii_case(repo) || lease.repo.eq_ignore_ascii_case(repo)
    } else {
        lease.repo_name.eq_ignore_ascii_case(repo)
    }
}

fn entry_from_lease(
    lease: &Lease,
    coord: &CoordSnapshot,
    query: &WatchQuery,
    probe: Option<&dyn GithubProbe>,
) -> WatchEntry {
    let overlay = coord.overlay_for(&JobId::new(&lease.owner, &lease.repo_name, &lease.job_id));
    let github = query
        .probe_github
        .then(|| probe.and_then(|p| probe_lease(lease, p)))
        .flatten();
    let recovery_status = recovery_of(lease);
    let collab_status = collab_of(lease, &overlay, github.as_ref());
    let residual_blockers = collect_blockers(&overlay, github.as_ref(), recovery_status);
    WatchEntry {
        job_id: lease.job_id.clone(),
        owner: lease.owner.clone(),
        repo: format!("{}/{}", lease.owner, lease.repo_name),
        branch: lease.branch.clone(),
        worktree_path: lease.worktree_path.clone(),
        lease_mode: lease.mode.as_str().to_owned(),
        collab_status,
        recovery_status,
        coord: overlay,
        github,
        residual_blockers,
    }
}

fn probe_lease(lease: &Lease, probe: &dyn GithubProbe) -> Option<GithubState> {
    let repo = format!("{}/{}", lease.owner, lease.repo_name);
    match probe.find_by_branch(&repo, &lease.branch) {
        Ok(Some(snapshot)) => Some(github_state(snapshot)),
        Ok(None) => None,
        Err(err) => Some(GithubState {
            number: 0,
            title: String::new(),
            url: String::new(),
            branch: lease.branch.clone(),
            base: String::new(),
            state: "UNKNOWN".to_owned(),
            check_status: "unknown".to_owned(),
            mergeable: None,
            is_draft: false,
            residual_blockers: vec![err.residual()],
        }),
    }
}

fn github_state(snapshot: PrSnapshot) -> GithubState {
    let (check_status, residual_blockers) = classify_snapshot(&snapshot);
    GithubState {
        number: snapshot.number,
        title: snapshot.title,
        url: snapshot.url,
        branch: snapshot.branch,
        base: snapshot.base,
        state: snapshot.state,
        check_status,
        mergeable: snapshot.mergeable,
        is_draft: snapshot.is_draft,
        residual_blockers,
    }
}

fn recovery_of(lease: &Lease) -> RecoveryStatus {
    if lease.released_at.is_some() {
        return RecoveryStatus::Released;
    }
    if !Path::new(&lease.worktree_path).exists() {
        return RecoveryStatus::MissingCheckout;
    }
    if heartbeat_stale(lease) {
        return RecoveryStatus::StaleHeartbeat;
    }
    RecoveryStatus::Live
}

fn heartbeat_stale(lease: &Lease) -> bool {
    let (Some(ttl), Some(heartbeat)) = (lease.ttl, lease.heartbeat) else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    now.saturating_sub(heartbeat) > ttl
}

fn collab_of(lease: &Lease, overlay: &CoordOverlay, github: Option<&GithubState>) -> CollabStatus {
    if is_conflicted(lease, github) {
        return CollabStatus::Conflicted;
    }
    if overlay.paused {
        return CollabStatus::Paused;
    }
    if is_waiting(lease, overlay) {
        return CollabStatus::Waiting;
    }
    if lease.mode == LeaseMode::MergeReady {
        return CollabStatus::ReadyForIntegration;
    }
    CollabStatus::Running
}

fn is_conflicted(lease: &Lease, github: Option<&GithubState>) -> bool {
    lease.mode == LeaseMode::NeedsHuman || github.is_some_and(github_conflicted)
}

fn is_waiting(lease: &Lease, overlay: &CoordOverlay) -> bool {
    overlay.waiting_on.is_some()
        || lease.mode == LeaseMode::Blocked
        || lease.mode == LeaseMode::ReviewOnly
}

fn github_conflicted(github: &GithubState) -> bool {
    github.check_status == "conflict"
        || github
            .mergeable
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("CONFLICTING"))
}

fn collect_blockers(
    overlay: &CoordOverlay,
    github: Option<&GithubState>,
    recovery: RecoveryStatus,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if let Some(waiting) = &overlay.waiting_on {
        blockers.push(waiting.clone());
    }
    blockers.extend(overlay.overlaps.iter().cloned());
    if let Some(github) = github {
        blockers.extend(github.residual_blockers.iter().cloned());
    }
    match recovery {
        RecoveryStatus::Live | RecoveryStatus::Released => {}
        RecoveryStatus::StaleHeartbeat => blockers.push("recovery:stale_heartbeat".to_owned()),
        RecoveryStatus::MissingCheckout => blockers.push("recovery:missing_checkout".to_owned()),
    }
    blockers
}
