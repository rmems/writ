//! Grant and finalize must report the row they wrote inside their own
//! transaction, not whatever a post-commit lookup happens to find.

use rusqlite::params;

use super::*;
use crate::error::Error;

#[test]
fn grant_returns_the_row_written_in_its_own_transaction() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("checkouts/one");
    let granted = store
        .grant(LeaseGrant {
            repo: &repo,
            owner: "local",
            repo_name: "repo",
            job_id: "job-a",
            branch: "hive/job-a",
            worktree_path: &wt,
            start_commit: "abc123",
        })
        .unwrap();
    assert_eq!(granted.job_id, "job-a");
    assert_eq!(granted.allocation_state, AllocationState::Active);
    assert!(granted.released_at.is_none());

    // The returned lease is the committed row: same row id, same operation
    // id, same path as a fresh lookup after the fact.
    let stored = store
        .find_job(JobKey {
            owner: "local",
            repo_name: "repo",
            job_id: "job-a",
        })
        .unwrap()
        .unwrap();
    assert_eq!(granted.row_id, stored.row_id);
    assert_eq!(granted.operation_id, stored.operation_id);
    assert_eq!(granted.worktree_path, stored.worktree_path);
}

#[test]
fn release_by_path_returns_the_row_it_finalized_not_the_newest_released_row() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("checkouts/shared");
    let grant_a = LeaseGrant {
        repo: &repo,
        owner: "local",
        repo_name: "repo",
        job_id: "job-a",
        branch: "hive/job-a",
        worktree_path: &wt,
        start_commit: "abc123",
    };
    let grant_b = LeaseGrant {
        job_id: "job-b",
        branch: "hive/job-b",
        ..grant_a
    };

    // job-a gets the lower row id, job-b the higher one. A regrant of job-a
    // refreshes its existing row rather than inserting a new one.
    let first_a = store.grant(grant_a).unwrap();
    store.release_by_path(&wt).unwrap();
    let first_b = store.grant(grant_b).unwrap();
    let released_b = store.release_by_path(&wt).unwrap().unwrap();
    assert_eq!(released_b.row_id, first_b.row_id);
    let again_a = store.grant(grant_a).unwrap();
    assert_eq!(again_a.row_id, first_a.row_id);
    assert!(first_a.row_id < first_b.row_id);

    // Make job-b's released row sort first under the path lookup's
    // `released_at DESC, id DESC` order. A release that re-queried by path
    // after commit would report job-b even though it finalized job-a.
    {
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "UPDATE leases SET released_at = ?1 WHERE id = ?2",
            params![released_b.released_at.unwrap() + 100_000, first_b.row_id],
        )
        .unwrap();
    }

    let released = store.release_by_path(&wt).unwrap().unwrap();
    assert_eq!(released.job_id, "job-a");
    assert_eq!(released.row_id, first_a.row_id);
    assert_eq!(released.allocation_state, AllocationState::Released);
    assert!(released.released_at.is_some());

    // With no live row left, the path lookup selects job-b. Tombstone goes
    // through the same finalize path and must report that same row.
    let tomb = store.tombstone_by_path(&wt).unwrap().unwrap();
    assert_eq!(tomb.row_id, first_b.row_id);
    assert_eq!(tomb.allocation_state, AllocationState::Tombstoned);
}

#[test]
fn missing_row_error_names_the_operation() {
    let err = super::super::grant::missing_row_error("grant lease");
    assert!(matches!(
        err,
        Error::LeaseStore {
            context: "grant lease",
            ..
        }
    ));
    assert_eq!(
        err.to_string(),
        "grant lease: lease row missing after write"
    );
}
