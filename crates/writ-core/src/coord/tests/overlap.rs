use super::*;

#[test]
fn two_store_connections_exchange_advisory_overlap() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    harness.seed_job("job-b", "hive/job-b");
    let peer = LeaseStore::open(&harness.path).unwrap();

    let first = announce(
        &harness.store,
        "job-a",
        "agent-a",
        &[String::from("crates/writ-core/src/coord.rs")],
    );
    assert!(first.overlaps.is_empty());
    let second = announce(
        &peer,
        "job-b",
        "agent-b",
        &[String::from("crates/writ-core/src")],
    );
    assert_eq!(second.overlaps.len(), 1);
    assert!(second.overlaps[0].advisory);
    assert_eq!(second.overlaps[0].job_id, "job-a");

    let inbox = peer
        .inbox(JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
        })
        .unwrap();
    assert!(
        inbox
            .iter()
            .any(|message| message.kind == MessageKind::Overlap && message.from_job_id == "job-b")
    );
    let announcing_inbox = peer
        .inbox(JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-b",
        })
        .unwrap();
    assert!(
        announcing_inbox.iter().any(|message| {
            message.kind == MessageKind::Overlap && message.from_job_id == "job-a"
        })
    );
}

#[test]
fn reannounce_with_disjoint_paths_clears_obsolete_overlap() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    harness.seed_job("job-b", "hive/job-b");
    announce(
        &harness.store,
        "job-a",
        "agent-a",
        &[String::from("src/shared")],
    );
    announce(
        &harness.store,
        "job-b",
        "agent-b",
        &[String::from("src/shared")],
    );

    announce(&harness.store, "job-a", "agent-a", &[String::from("src/a")]);

    assert!(
        harness
            .store
            .inbox(job_key_a())
            .unwrap()
            .iter()
            .all(|message| message.kind != MessageKind::Overlap || message.acked_at.is_some())
    );
}

#[test]
fn release_clears_overlap_messages_for_live_peers() {
    let harness = Harness::new();
    let worktree_a = harness.seed_job("job-a", "hive/job-a");
    harness.seed_job("job-b", "hive/job-b");
    announce(
        &harness.store,
        "job-a",
        "agent-a",
        &[String::from("src/shared")],
    );
    announce(
        &harness.store,
        "job-b",
        "agent-b",
        &[String::from("src/shared")],
    );

    harness.store.release_by_path(&worktree_a).unwrap();

    assert!(
        harness
            .store
            .inbox(JobKey {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-b",
            })
            .unwrap()
            .iter()
            .all(|message| message.kind != MessageKind::Overlap || message.acked_at.is_some())
    );
}
