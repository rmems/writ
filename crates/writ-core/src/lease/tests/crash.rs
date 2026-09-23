use super::*;
use crate::error::PolicyCode;
use std::fs;

#[test]
fn crash_before_mutation_is_retryable_without_reserving_identity() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
    assert_eq!(prepared.allocation_state, AllocationState::Prepared);
    assert_eq!(prepared.requested_start_point, "refs/heads/main");
    assert_eq!(prepared.start_commit, harness.start);
    assert_ne!(prepared.requested_start_point, prepared.start_commit);
    assert_outcome(&harness.store, harness.key(), &harness.repo, "retry");
    assert!(harness.store.find_job(harness.key()).unwrap().is_none());
    let again = harness.store.prepare_allocate(harness.request()).unwrap();
    assert_ne!(again.operation_id, prepared.operation_id);
}

#[test]
fn allocate_advance_rejects_invalid_source_states() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();

    let err = harness
        .store
        .commit_allocate(&prepared.operation_id)
        .unwrap_err();
    assert!(err.to_string().contains("PREPARED -> ACTIVE"));
    assert_eq!(
        harness
            .store
            .find_by_operation(&prepared.operation_id)
            .unwrap()
            .unwrap()
            .allocation_state,
        AllocationState::Prepared
    );

    harness.store.mark_mutating(&prepared.operation_id).unwrap();
    harness
        .store
        .commit_allocate(&prepared.operation_id)
        .unwrap();
    let err = harness
        .store
        .mark_mutating(&prepared.operation_id)
        .unwrap_err();
    assert!(err.to_string().contains("ACTIVE -> MUTATING"));

    {
        let conn = harness.store.conn.lock().unwrap();
        conn.execute(
            "UPDATE leases SET allocation_state = 'NEEDS_ATTENTION' WHERE operation_id = ?1",
            [&prepared.operation_id],
        )
        .unwrap();
    }
    let err = harness
        .store
        .mark_mutating(&prepared.operation_id)
        .unwrap_err();
    assert!(err.to_string().contains("NEEDS_ATTENTION -> MUTATING"));
}

#[test]
fn needs_attention_can_abort_after_residual_state_is_removed() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    fs::create_dir_all(&harness.worktree).unwrap();
    fs::write(harness.worktree.join("occupied"), "partial\n").unwrap();
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );

    fs::remove_dir_all(&harness.worktree).unwrap();
    assert_outcome(&harness.store, harness.key(), &harness.repo, "retry");
    assert!(harness.store.find_job(harness.key()).unwrap().is_none());
}

#[test]
fn unknown_allocation_state_stays_needs_attention_without_mutation() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
    {
        let conn = harness.store.conn.lock().unwrap();
        conn.execute(
            "UPDATE leases SET allocation_state = 'FUTURE_STATE' WHERE operation_id = ?1",
            [&prepared.operation_id],
        )
        .unwrap();
    }

    let outcome = assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
    let ReconcileOutcome::NeedsAttention { lease, inspection } = outcome else {
        panic!("expected needs attention, got {outcome:?}");
    };
    assert_eq!(lease.allocation_state, AllocationState::Unknown);
    assert_eq!(inspection.allocation_state.as_deref(), Some("UNKNOWN"));
    let stored: String = harness
        .store
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT allocation_state FROM leases WHERE operation_id = ?1",
            [&prepared.operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, "FUTURE_STATE");
    assert_eq!(AllocationState::parse("ABORTED"), AllocationState::Aborted);
}

#[test]
fn crash_after_branch_creation_is_fail_closed_without_cleanup() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    harness
        .store
        .mark_mutating(
            &harness
                .store
                .find_job(harness.key())
                .unwrap()
                .unwrap()
                .operation_id,
        )
        .unwrap();
    git(&harness.repo, &["branch", "hive/job-1", &harness.start]);
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
    assert_eq!(
        git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
        harness.start
    );
    assert!(!harness.worktree.exists());
    let lease = harness.store.find_job(harness.key()).unwrap().unwrap();
    assert_eq!(lease.allocation_state, AllocationState::NeedsAttention);
}

#[test]
fn crash_after_filesystem_creation_is_fail_closed_without_cleanup() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    fs::create_dir_all(&harness.worktree).unwrap();
    fs::write(harness.worktree.join("occupied"), "partial\n").unwrap();
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
    assert_eq!(
        fs::read_to_string(harness.worktree.join("occupied")).unwrap(),
        "partial\n"
    );
}

#[test]
fn crash_after_registration_is_fail_closed_when_head_is_absent() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    git(
        &harness.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "--",
            harness.worktree.to_str().unwrap(),
            &harness.start,
        ],
    );
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
    assert!(harness.worktree.exists());
    let listing = git(&harness.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        classify::porcelain_lists_worktree(&listing, &harness.worktree),
        "fail-closed reconcile must keep the registered worktree; listing={listing:?} expected={:?}",
        harness.worktree
    );
}

#[test]
fn matching_interrupted_allocation_is_promoted_with_canonical_commit() {
    let harness = RepoHarness::new();
    harness.prepare_and_add_worktree();
    let outcome = assert_outcome(&harness.store, harness.key(), &harness.repo, "promoted");
    let ReconcileOutcome::Promoted { lease, inspection } = outcome else {
        panic!("expected promote, got {outcome:?}");
    };
    assert_eq!(lease.start_commit, harness.start);
    assert_eq!(lease.requested_start_point, "refs/heads/main");
    assert_eq!(lease.allocation_state, AllocationState::Active);
    assert_eq!(lease.mode, LeaseMode::WriterLocked);
    assert_eq!(
        inspection.head_commit.as_deref(),
        Some(harness.start.as_str())
    );
    assert_eq!(inspection.classification, EvidenceClass::Matching);
    assert!(!inspection.ttl_expired);
}

#[test]
fn conflicting_head_stays_fail_closed() {
    let harness = RepoHarness::new();
    harness.prepare_and_add_worktree();
    let moved = later_commit(&harness.repo);
    git(
        &harness.repo,
        &["update-ref", "refs/heads/hive/job-1", &moved],
    );
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
    assert_eq!(
        git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
        moved
    );
    assert!(harness.worktree.exists());
}

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

fn refuse_after_terminal(
    finalize: fn(&LeaseStore, &std::path::Path) -> crate::error::Result<Option<Lease>>,
    expected: &str,
    code: PolicyCode,
) {
    let harness = RepoHarness::new();
    harness.prepare_and_add_worktree();
    let other = LeaseStore::open(harness.store.path()).unwrap();
    finalize(&other, &harness.worktree).unwrap();
    assert_outcome(&harness.store, harness.key(), &harness.repo, expected);
    let err = harness
        .store
        .prepare_allocate(harness.request())
        .unwrap_err();
    assert_policy(err, code);
}

#[test]
fn concurrent_reconcile_cannot_resurrect_tombstoned_lease() {
    refuse_after_terminal(
        LeaseStore::tombstone_by_path,
        "tombstoned",
        PolicyCode::LeaseTombstoned,
    );
}

#[test]
fn concurrent_reconcile_cannot_resurrect_released_lease() {
    let harness = RepoHarness::new();
    let prepared = harness.prepare_and_add_worktree();
    harness
        .store
        .commit_allocate(&prepared.operation_id)
        .unwrap();
    let other = LeaseStore::open(harness.store.path()).unwrap();
    other.release_by_path(&harness.worktree).unwrap();
    assert_outcome(&harness.store, harness.key(), &harness.repo, "released");
    let err = harness
        .store
        .prepare_allocate(harness.request())
        .unwrap_err();
    assert_policy(err, PolicyCode::LeaseReleased);
}

#[test]
fn ttl_expiry_and_missing_lease_are_distinct() {
    let harness = RepoHarness::new();
    let mut request = harness.request();
    request.ttl = Some(1);
    let prepared = harness.store.prepare_allocate(request).unwrap();
    harness.store.mark_mutating(&prepared.operation_id).unwrap();
    git(
        &harness.repo,
        &[
            "worktree",
            "add",
            "-b",
            "hive/job-1",
            "--",
            harness.worktree.to_str().unwrap(),
            &harness.start,
        ],
    );
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
    let orphaned = harness.store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(orphaned.classification, EvidenceClass::OrphanedLease);
    assert!(orphaned.lease_present && orphaned.ttl_expired && orphaned.path_exists);
    let missing_store = LeaseStore::open(harness.store.path().with_file_name("other.db")).unwrap();
    let missing = missing_store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(missing.classification, EvidenceClass::MissingLease);
    assert!(!missing.lease_present && missing.path_exists && !missing.ttl_expired);
}

#[test]
fn inspect_does_not_mutate_or_adopt() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    git(&harness.repo, &["branch", "hive/job-1", &harness.start]);
    let before = git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]);
    let _ = harness.store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(
        git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
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
