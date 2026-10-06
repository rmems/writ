use super::*;

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
fn allocate_retries_of_completed_phases_are_idempotent() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
    let mutating = harness.store.mark_mutating(&prepared.operation_id).unwrap();
    let again = harness.store.mark_mutating(&prepared.operation_id).unwrap();
    assert_eq!(again.allocation_state, AllocationState::Mutating);
    assert_eq!(again.operation_id, mutating.operation_id);
    let active = harness
        .store
        .commit_allocate(&prepared.operation_id)
        .unwrap();
    let committed = harness
        .store
        .commit_allocate(&prepared.operation_id)
        .unwrap();
    assert_eq!(committed.allocation_state, AllocationState::Active);
    assert_eq!(committed.operation_id, active.operation_id);
}
