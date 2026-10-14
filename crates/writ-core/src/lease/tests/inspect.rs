use std::fs;

use super::{RepoHarness, add_job_worktree};
use crate::lease::{AllocationState, EvidenceClass, InspectRequest, LeaseStore};

#[cfg(unix)]
use crate::lease::inspect::inspect_git;
#[cfg(unix)]
use std::os::unix::fs::symlink;

#[test]
fn persisted_record_distinguishes_symbolic_resolved_and_operation() {
    let harness = RepoHarness::new();
    let lease = harness.store.prepare_allocate(harness.request()).unwrap();
    let inspection = harness.store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(
        inspection.requested_start_point.as_deref(),
        Some("refs/heads/main")
    );
    assert_eq!(
        inspection.resolved_start_commit.as_deref(),
        Some(harness.start.as_str())
    );
    assert_eq!(
        inspection.operation_id.as_deref(),
        Some(lease.operation_id.as_str())
    );
    assert_ne!(
        inspection.requested_start_point.as_deref(),
        inspection.resolved_start_commit.as_deref()
    );
}

#[test]
fn ttl_expiry_classifies_orphaned_lease() {
    let harness = expired_active_lease();
    let orphaned = harness.store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(orphaned.classification, EvidenceClass::OrphanedLease);
    assert!(orphaned.lease_present);
    assert!(orphaned.ttl_expired);
    assert!(orphaned.path_exists);
}

#[test]
fn missing_lease_is_distinct_from_ttl_expiry() {
    let harness = expired_active_lease();
    let missing_store = LeaseStore::open(harness.store.path().with_file_name("other.db")).unwrap();
    let missing = missing_store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(missing.classification, EvidenceClass::MissingLease);
    assert!(!missing.lease_present);
    assert!(missing.path_exists);
    assert!(!missing.ttl_expired);
}

#[test]
fn inspect_does_not_mutate_or_adopt() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    super::git(&harness.repo, &["branch", "hive/job-1", &harness.start]);
    let before = super::git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]);
    let _ = harness.store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(
        super::git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
        before
    );
    assert!(!harness.worktree.exists());
    assert_eq!(
        harness
            .store
            .find_job(harness.key())
            .unwrap()
            .unwrap()
            .allocation_state,
        AllocationState::Prepared
    );
}

fn expired_active_lease() -> RepoHarness {
    let harness = RepoHarness::new();
    let mut request = harness.request();
    request.ttl = Some(1);
    let prepared = harness.store.prepare_allocate(request).unwrap();
    harness.store.mark_mutating(&prepared.operation_id).unwrap();
    add_job_worktree(&harness);
    harness
        .store
        .commit_allocate(&prepared.operation_id)
        .unwrap();
    {
        let conn = harness.store.conn.lock().unwrap();
        conn.execute(
            "UPDATE leases SET heartbeat = 1, ttl = 1 WHERE job_id = 'job-1'",
            [],
        )
        .unwrap();
    }
    harness
}

// Exercises the `same_existing_path` tolerance inside `inspect_git`'s
// `worktree_registered` check. git reports the OS-canonical real worktree
// path, while the stored lease path reaches the same directory through a
// symlinked prefix (the macOS `/var -> /private/var` situation).
#[cfg(unix)]
#[test]
fn worktree_registered_tolerates_symlinked_stored_path() {
    let harness = RepoHarness::new();
    fs::create_dir_all(harness.worktree.parent().unwrap()).unwrap();
    add_job_worktree(&harness);

    let real_parent = harness.worktree.parent().unwrap();
    let alias_parent = harness.temp.path().join("aliased-worktrees");
    symlink(real_parent, &alias_parent).unwrap();
    let aliased_worktree = alias_parent.join("job-1");
    assert_ne!(aliased_worktree, harness.worktree);

    let evidence = inspect_git(&harness.repo, &aliased_worktree, "hive/job-1");
    assert!(
        evidence.worktree_registered,
        "symlinked stored path should still match git's canonical worktree path"
    );

    harness.store.prepare_allocate(harness.request()).unwrap();
    let inspection = harness
        .store
        .inspect(InspectRequest {
            repo_root: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            worktree_path: &aliased_worktree,
            branch: Some("hive/job-1"),
        })
        .unwrap();
    assert!(inspection.worktree_registered);
    assert_eq!(inspection.classification, EvidenceClass::Matching);
}
