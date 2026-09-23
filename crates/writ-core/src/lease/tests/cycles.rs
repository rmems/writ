use super::*;

fn active_job() -> RepoHarness {
    let harness = RepoHarness::new();
    let prepared = harness.prepare_and_add_worktree();
    harness
        .store
        .commit_allocate(&prepared.operation_id)
        .unwrap();
    harness
}

fn stored_fix_cycles(harness: &RepoHarness) -> Option<i64> {
    harness
        .store
        .find_job(harness.key())
        .unwrap()
        .unwrap()
        .fix_cycles
}

#[test]
fn fix_cycles_crash_before_mutation_does_not_count() {
    let harness = active_job();
    let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
    assert_eq!(token.authorized, 1);
    let recovered = harness
        .store
        .reconcile_fix_cycle(harness.key(), None)
        .unwrap();
    assert_eq!(recovered, FixCycleReconcile::Aborted { fix_cycles: 0 });
    assert_eq!(stored_fix_cycles(&harness), Some(0));
}

#[test]
fn fix_cycles_crash_after_proven_mutation_is_not_lost() {
    let harness = active_job();
    let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
    harness
        .store
        .mark_fix_cycle_mutating(&token.operation_id)
        .unwrap();
    let recovered = harness
        .store
        .reconcile_fix_cycle(harness.key(), Some(true))
        .unwrap();
    assert_eq!(recovered, FixCycleReconcile::Committed { fix_cycles: 1 });
    let again = harness
        .store
        .reconcile_fix_cycle(harness.key(), Some(true))
        .unwrap();
    assert_eq!(again, FixCycleReconcile::Idle { fix_cycles: 1 });
}

#[test]
fn fix_cycles_mutate_without_proof_stays_needs_attention() {
    let harness = active_job();
    let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
    harness
        .store
        .mark_fix_cycle_mutating(&token.operation_id)
        .unwrap();
    let recovered = harness
        .store
        .reconcile_fix_cycle(harness.key(), None)
        .unwrap();
    assert!(matches!(
        recovered,
        FixCycleReconcile::NeedsAttention { pending: 1, .. }
    ));
    assert_eq!(stored_fix_cycles(&harness), Some(0));
}
