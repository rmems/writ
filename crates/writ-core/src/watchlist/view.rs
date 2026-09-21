//! Assemble watchlist rows from leases + optional coord + live GitHub.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::lease::{Lease, LeaseMode, LeaseStore};

use super::classify::classify_snapshot;
use super::coord_read::{CoordSnapshot, load_coord_snapshot};
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
    let overlay = coord.overlay_for(&lease.owner, &lease.repo_name, &lease.job_id);
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
    if github.is_some_and(github_conflicted) || lease.mode == LeaseMode::NeedsHuman {
        return CollabStatus::Conflicted;
    }
    if overlay.paused {
        return CollabStatus::Paused;
    }
    if overlay.waiting_on.is_some()
        || lease.mode == LeaseMode::Blocked
        || lease.mode == LeaseMode::ReviewOnly
    {
        return CollabStatus::Waiting;
    }
    if lease.mode == LeaseMode::MergeReady {
        return CollabStatus::ReadyForIntegration;
    }
    CollabStatus::Running
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lease::LeaseGrant;
    use crate::watchlist::github::{CheckSnapshot, ProbeError};
    use rusqlite::Connection;
    use tempfile::tempdir;

    struct NoGithub;

    impl GithubProbe for NoGithub {
        fn view(&self, repo: &str, _number: u64) -> std::result::Result<PrSnapshot, ProbeError> {
            Err(ProbeError::Gh {
                repo: repo.to_owned(),
                message: "unused".to_owned(),
            })
        }

        fn find_by_branch(
            &self,
            repo: &str,
            _branch: &str,
        ) -> std::result::Result<Option<PrSnapshot>, ProbeError> {
            Err(ProbeError::OwnerNotAllowed {
                repo: repo.to_owned(),
            })
        }
    }

    struct FakePr;

    impl GithubProbe for FakePr {
        fn view(&self, _repo: &str, _number: u64) -> std::result::Result<PrSnapshot, ProbeError> {
            unimplemented!()
        }

        fn find_by_branch(
            &self,
            repo: &str,
            branch: &str,
        ) -> std::result::Result<Option<PrSnapshot>, ProbeError> {
            Ok(Some(PrSnapshot {
                repo: repo.to_owned(),
                number: 41,
                branch: branch.to_owned(),
                base: "main".to_owned(),
                title: "fix".to_owned(),
                url: "https://example.test/41".to_owned(),
                state: "OPEN".to_owned(),
                mergeable: Some("CONFLICTING".to_owned()),
                review_decision: None,
                is_draft: false,
                checks: vec![CheckSnapshot {
                    name: "ci".to_owned(),
                    state: "SUCCESS".to_owned(),
                }],
            }))
        }
    }

    fn grant_job<'a>(
        repo: &'a Path,
        wt: &'a Path,
        job: &'a str,
        branch: &'a str,
    ) -> LeaseGrant<'a> {
        LeaseGrant {
            repo,
            owner: "acme",
            repo_name: "sample",
            job_id: job,
            branch,
            worktree_path: wt,
            start_commit: "abc123",
        }
    }

    #[test]
    fn list_reads_leases_without_coord_or_github() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("leases.db");
        let store = LeaseStore::open(&db).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkout");
        std::fs::create_dir_all(&wt).unwrap();
        store
            .grant(grant_job(&repo, &wt, "job-1", "hive/job-1"))
            .unwrap();

        let data = load_view(&store, &WatchQuery::default(), None).unwrap();
        assert!(!data.coord_available);
        assert!(!data.github_probed);
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].collab_status, CollabStatus::Running);
        assert_eq!(data.entries[0].recovery_status, RecoveryStatus::Live);
        assert!(data.entries[0].github.is_none());
    }

    #[test]
    fn paused_claim_and_help_message_shape_waiting_and_paused() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("leases.db");
        let store = LeaseStore::open(&db).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkout");
        std::fs::create_dir_all(&wt).unwrap();
        store
            .grant(grant_job(&repo, &wt, "job-1", "hive/job-1"))
            .unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE coord_claims (
                owner TEXT, repo_name TEXT, job_id TEXT, agent_id TEXT,
                session_id TEXT, intent TEXT, declared_paths TEXT,
                owner_generation INTEGER, paused_at INTEGER
            );
            CREATE TABLE coord_messages (
                kind TEXT, from_agent_id TEXT, from_job_id TEXT,
                to_owner TEXT, to_repo_name TEXT, to_job_id TEXT,
                body TEXT, acked_at INTEGER
            );
            INSERT INTO coord_claims VALUES (
                'acme','sample','job-1','agent-a',NULL,NULL,'[]',1,10
            );
            ",
        )
        .unwrap();
        drop(conn);

        let data = load_view(&store, &WatchQuery::default(), None).unwrap();
        assert!(data.coord_available);
        assert_eq!(data.entries[0].collab_status, CollabStatus::Paused);
        assert!(data.entries[0].coord.paused);
    }

    #[test]
    fn github_conflict_marks_conflicted_without_merge_gate() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("leases.db");
        let store = LeaseStore::open(&db).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkout");
        std::fs::create_dir_all(&wt).unwrap();
        store
            .grant(grant_job(&repo, &wt, "job-1", "hive/job-1"))
            .unwrap();

        let query = WatchQuery {
            probe_github: true,
            ..WatchQuery::default()
        };
        let data = load_view(&store, &query, Some(&FakePr)).unwrap();
        assert!(data.github_probed);
        assert_eq!(data.entries[0].collab_status, CollabStatus::Conflicted);
        let github = data.entries[0].github.as_ref().unwrap();
        assert_eq!(github.check_status, "conflict");
        assert_eq!(github.number, 41);
    }

    #[test]
    fn github_probe_failure_stays_on_lease_view() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("leases.db");
        let store = LeaseStore::open(&db).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkout");
        std::fs::create_dir_all(&wt).unwrap();
        store
            .grant(grant_job(&repo, &wt, "job-1", "hive/job-1"))
            .unwrap();
        let query = WatchQuery {
            probe_github: true,
            ..WatchQuery::default()
        };
        let data = load_view(&store, &query, Some(&NoGithub)).unwrap();
        assert_eq!(data.entries[0].collab_status, CollabStatus::Running);
        assert!(
            data.entries[0]
                .residual_blockers
                .iter()
                .any(|b| b.contains("owner_not_allowed"))
        );
    }

    #[test]
    fn unacked_help_without_pause_is_waiting() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("leases.db");
        let store = LeaseStore::open(&db).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkout");
        std::fs::create_dir_all(&wt).unwrap();
        store
            .grant(grant_job(&repo, &wt, "job-1", "hive/job-1"))
            .unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE coord_claims (
                owner TEXT, repo_name TEXT, job_id TEXT, agent_id TEXT,
                session_id TEXT, intent TEXT, declared_paths TEXT,
                owner_generation INTEGER, paused_at INTEGER
            );
            CREATE TABLE coord_messages (
                kind TEXT, from_agent_id TEXT, from_job_id TEXT,
                to_owner TEXT, to_repo_name TEXT, to_job_id TEXT,
                body TEXT, acked_at INTEGER
            );
            INSERT INTO coord_claims VALUES (
                'acme','sample','job-1','agent-a',NULL,NULL,'[]',1,NULL
            );
            INSERT INTO coord_messages VALUES (
                'handoff','agent-b','job-2','acme','sample','job-1','take over', NULL
            );
            ",
        )
        .unwrap();
        drop(conn);
        let data = load_view(&store, &WatchQuery::default(), None).unwrap();
        assert_eq!(data.entries[0].collab_status, CollabStatus::Waiting);
        assert_eq!(
            data.entries[0].coord.waiting_on.as_deref(),
            Some("handoff:agent-b:job-2")
        );
    }

    #[test]
    fn owner_filter_is_case_insensitive() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("leases.db");
        let store = LeaseStore::open(&db).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkout");
        std::fs::create_dir_all(&wt).unwrap();
        store
            .grant(grant_job(&repo, &wt, "job-1", "hive/job-1"))
            .unwrap();
        let query = WatchQuery {
            owner: Some("ACME".to_owned()),
            ..WatchQuery::default()
        };
        let data = load_view(&store, &query, None).unwrap();
        assert_eq!(data.entries.len(), 1);
        let query = WatchQuery {
            owner: Some("other".to_owned()),
            ..WatchQuery::default()
        };
        let data = load_view(&store, &query, None).unwrap();
        assert!(data.entries.is_empty());
    }

    #[test]
    fn merge_ready_lease_is_ready_for_integration() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("leases.db");
        let store = LeaseStore::open(&db).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkout");
        std::fs::create_dir_all(&wt).unwrap();
        store
            .grant(grant_job(&repo, &wt, "job-1", "hive/job-1"))
            .unwrap();
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE leases SET mode = 'MERGE_READY'", [])
            .unwrap();
        let data = load_view(&store, &WatchQuery::default(), None).unwrap();
        assert_eq!(
            data.entries[0].collab_status,
            CollabStatus::ReadyForIntegration
        );
    }
}
