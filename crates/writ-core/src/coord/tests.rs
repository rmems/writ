use super::*;
use crate::error::{Error, PolicyCode};
use crate::lease::{AllocateRequest, JobKey, LeaseStore};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

struct Harness {
    _temp: tempfile::TempDir,
    store: LeaseStore,
    path: std::path::PathBuf,
    repo: std::path::PathBuf,
    start: String,
}

impl Harness {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test User"]);
        git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
        let start = git(&repo, &["rev-parse", "HEAD"]);
        let path = temp.path().join("leases.db");
        let store = LeaseStore::open(&path).unwrap();
        Self {
            _temp: temp,
            store,
            path,
            repo,
            start,
        }
    }

    fn seed_job(&self, job_id: &str, branch: &str) -> std::path::PathBuf {
        self.seed_job_in(SeedJob {
            owner: "acme",
            repo_name: "sample",
            job_id,
            branch,
        })
    }

    fn seed_job_in(&self, spec: SeedJob<'_>) -> std::path::PathBuf {
        let worktree = self
            ._temp
            .path()
            .join("worktrees")
            .join(spec.owner)
            .join(spec.repo_name)
            .join(spec.job_id);
        fs::create_dir_all(&worktree).unwrap();
        let prepared = self
            .store
            .prepare_allocate(AllocateRequest {
                repo: &self.repo,
                owner: spec.owner,
                repo_name: spec.repo_name,
                job_id: spec.job_id,
                branch: spec.branch,
                worktree_path: &worktree,
                requested_start_point: "refs/heads/main",
                start_commit: &self.start,
                ttl: None,
            })
            .unwrap();
        self.store.mark_mutating(&prepared.operation_id).unwrap();
        self.store.commit_allocate(&prepared.operation_id).unwrap();
        worktree
    }
}

struct SeedJob<'a> {
    owner: &'a str,
    repo_name: &'a str,
    job_id: &'a str,
    branch: &'a str,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn announce<'a>(
    store: &'a LeaseStore,
    job_id: &'a str,
    agent: &'a str,
    paths: &'a [String],
) -> AnnounceResult {
    announce_in(
        store,
        AnnounceCase {
            owner: "acme",
            repo_name: "sample",
            job_id,
            agent,
            paths,
        },
    )
}

struct AnnounceCase<'a> {
    owner: &'a str,
    repo_name: &'a str,
    job_id: &'a str,
    agent: &'a str,
    paths: &'a [String],
}

fn announce_in(store: &LeaseStore, spec: AnnounceCase<'_>) -> AnnounceResult {
    store
        .announce(AnnounceRequest {
            owner: spec.owner,
            repo_name: spec.repo_name,
            job_id: spec.job_id,
            agent_id: spec.agent,
            session_id: Some("session-a"),
            agent_type: "worker",
            intent: Some("edit shared contract"),
            paths: spec.paths,
        })
        .unwrap()
}

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
}

#[test]
fn overlap_scan_is_scoped_to_the_current_owner_and_repo() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    harness.seed_job("job-b", "hive/job-b");
    harness.seed_job_in(SeedJob {
        owner: "other",
        repo_name: "sample",
        job_id: "job-c",
        branch: "hive/job-c",
    });
    harness.seed_job_in(SeedJob {
        owner: "acme",
        repo_name: "other",
        job_id: "job-d",
        branch: "hive/job-d",
    });
    let paths = [String::from("crates/writ-core/src")];

    announce(&harness.store, "job-b", "agent-b", &paths);
    announce_in(
        &harness.store,
        AnnounceCase {
            owner: "other",
            repo_name: "sample",
            job_id: "job-c",
            agent: "agent-c",
            paths: &paths,
        },
    );
    announce_in(
        &harness.store,
        AnnounceCase {
            owner: "acme",
            repo_name: "other",
            job_id: "job-d",
            agent: "agent-d",
            paths: &paths,
        },
    );

    let result = announce(&harness.store, "job-a", "agent-a", &paths);
    assert_eq!(result.overlaps.len(), 1);
    assert_eq!(result.overlaps[0].owner, "acme");
    assert_eq!(result.overlaps[0].repo_name, "sample");
    assert_eq!(result.overlaps[0].job_id, "job-b");
}

fn job_key_a() -> JobKey<'static> {
    JobKey {
        owner: "acme",
        repo_name: "sample",
        job_id: "job-a",
    }
}

fn assert_held(result: crate::error::Result<AnnounceResult>) {
    match result {
        Err(Error::PolicyViolation {
            code: PolicyCode::CoordClaimHeld,
            ..
        }) => {}
        other => panic!("expected held claim, got {other:?}"),
    }
}

fn sample_handoff(store: &LeaseStore, body: &str, to_job_id: Option<&str>) -> CoordMessage {
    store
        .propose_handoff(HandoffRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            from_agent_id: "agent-a",
            to_agent_id: "agent-b",
            to_job_id,
            expected_generation: Some(1),
            body,
        })
        .unwrap()
}

fn sample_ack(
    peer: &LeaseStore,
    message_id: i64,
) -> crate::error::Result<(CoordMessage, Option<CoordClaim>)> {
    peer.ack_message(AckRequest {
        message_id,
        owner: "acme",
        repo_name: "sample",
        job_id: "job-a",
        agent_id: "agent-b",
        session_id: Some("session-b"),
    })
}

#[test]
fn pause_does_not_release_or_delete_wip_and_requires_handoff_ack() {
    let harness = Harness::new();
    let worktree = harness.seed_job("job-a", "hive/job-a");
    let wip = worktree.join("wip.txt");
    fs::write(&wip, "keep me").unwrap();
    announce(
        &harness.store,
        "job-a",
        "agent-a",
        &[String::from("crates/writ-core/src/coord.rs")],
    );
    let peer = LeaseStore::open(&harness.path).unwrap();
    let (paused, help) = harness
        .store
        .pause_claim(PauseRequest {
            key: job_key_a(),
            agent_id: "agent-a",
            body: Some("owner crashed"),
        })
        .unwrap();
    assert!(paused.paused_at.is_some() && help.kind == MessageKind::Help);
    assert_eq!(fs::read_to_string(&wip).unwrap(), "keep me");
    assert_held(peer.announce(AnnounceRequest {
        owner: "acme",
        repo_name: "sample",
        job_id: "job-a",
        agent_id: "agent-b",
        session_id: Some("session-b"),
        agent_type: "worker",
        intent: Some("take over"),
        paths: &[String::from("crates/writ-core/src/coord.rs")],
    }));
    let handoff = sample_handoff(
        &harness.store,
        "paused owner transferring assignment",
        Some("job-a"),
    );
    let (ack, transferred) = sample_ack(&peer, handoff.id).unwrap();
    let transferred = transferred.expect("handoff ACK transfers the claim");
    assert_eq!(
        (
            ack.kind,
            transferred.agent_id.as_str(),
            transferred.owner_generation
        ),
        (MessageKind::Ack, "agent-b", 2)
    );
    assert!(sample_ack(&peer, handoff.id).is_err());
    assert_eq!(fs::read_to_string(&wip).unwrap(), "keep me");
}

fn assert_stale(err: Error) {
    match err {
        Error::PolicyViolation {
            code: PolicyCode::CoordStaleGeneration,
            ..
        } => {}
        other => panic!("expected stale generation, got {other:?}"),
    }
}

#[test]
fn stale_generation_handoff_is_rejected() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &[]);
    assert_stale(
        harness
            .store
            .propose_handoff(HandoffRequest {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-a",
                from_agent_id: "agent-a",
                to_agent_id: "agent-b",
                to_job_id: None,
                expected_generation: Some(99),
                body: "stale",
            })
            .unwrap_err(),
    );
    let peer = LeaseStore::open(&harness.path).unwrap();
    let first = sample_handoff(&harness.store, "first gen-1 offer", None);
    let leftover = sample_handoff(&harness.store, "leftover gen-1 offer", None);
    sample_ack(&peer, first.id).unwrap();
    assert_stale(sample_ack(&peer, leftover.id).unwrap_err());
}

#[test]
fn handoff_ack_rejects_a_foreign_job_identity() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    harness.seed_job("job-b", "hive/job-b");
    announce(&harness.store, "job-a", "agent-a", &[]);
    announce(&harness.store, "job-b", "agent-b", &[]);
    let handoff = sample_handoff(&harness.store, "transfer job-a", Some("job-a"));
    let err = harness
        .store
        .ack_message(AckRequest {
            message_id: handoff.id,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-b",
            agent_id: "agent-b",
            session_id: Some("session-b"),
        })
        .unwrap_err();
    match err {
        Error::PolicyViolation {
            code: PolicyCode::CoordClaimMissing,
            message,
        } => assert!(
            message.contains("is not the source of handoff"),
            "{message}"
        ),
        other => panic!("expected source-scope rejection, got {other:?}"),
    }
    let claim = harness.store.find_claim(job_key_a()).unwrap().unwrap();
    assert_eq!(claim.agent_id, "agent-a");
}

#[test]
fn announce_rechecks_active_lease_inside_the_write_txn() {
    let harness = Harness::new();
    let worktree = harness._temp.path().join("worktrees/acme/sample/job-a");
    fs::create_dir_all(&worktree).unwrap();
    harness
        .store
        .prepare_allocate(AllocateRequest {
            repo: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            branch: "hive/job-a",
            worktree_path: &worktree,
            requested_start_point: "refs/heads/main",
            start_commit: &harness.start,
            ttl: None,
        })
        .unwrap();
    let err = harness
        .store
        .announce(AnnounceRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-a",
            session_id: None,
            agent_type: "worker",
            intent: None,
            paths: &[],
        })
        .unwrap_err();
    match err {
        Error::PolicyViolation {
            code: PolicyCode::CoordClaimMissing,
            message,
        } => assert!(message.contains("PREPARED"), "{message}"),
        other => panic!("expected missing ACTIVE lease, got {other:?}"),
    }
}

#[test]
fn repository_root_declaration_overlaps_nested_paths() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    harness.seed_job("job-b", "hive/job-b");
    announce(&harness.store, "job-a", "agent-a", &[String::from(".")]);
    let second = announce(
        &harness.store,
        "job-b",
        "agent-b",
        &[String::from("src/lib.rs")],
    );
    assert_eq!(second.overlaps.len(), 1);
    assert_eq!(second.overlaps[0].paths, vec![String::from(".")]);
}

#[test]
fn declared_paths_canonicalize_internal_dot_and_parent() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    harness.seed_job("job-b", "hive/job-b");
    announce(
        &harness.store,
        "job-a",
        "agent-a",
        &[String::from("crates/writ-core/src/coord.rs")],
    );
    let second = announce(
        &harness.store,
        "job-b",
        "agent-b",
        &[String::from("crates/writ-core/src/./lib/../coord.rs")],
    );
    assert_eq!(second.overlaps.len(), 1);
    assert_eq!(
        second.intent.paths,
        vec![String::from("crates/writ-core/src/coord.rs")]
    );
}

#[test]
fn announce_rejects_escaping_declared_paths() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    let err = harness
        .store
        .announce(AnnounceRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-a",
            session_id: Some("session-a"),
            agent_type: "worker",
            intent: Some("edit"),
            paths: &[String::from("src/../../secret")],
        })
        .unwrap_err();
    assert!(err.to_string().contains("repository-relative"));
}

#[test]
fn inbox_broadcasts_are_scoped_to_the_same_repository() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    harness.seed_job_in(SeedJob {
        owner: "acme",
        repo_name: "other",
        job_id: "job-b",
        branch: "hive/job-b",
    });
    announce(&harness.store, "job-a", "agent-a", &[String::from("src")]);
    announce_in(
        &harness.store,
        AnnounceCase {
            owner: "acme",
            repo_name: "other",
            job_id: "job-b",
            agent: "agent-b",
            paths: &[String::from("src")],
        },
    );
    let inbox = harness
        .store
        .inbox(JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
        })
        .unwrap();
    assert!(
        inbox
            .iter()
            .all(|message| message.from_repo_name != "other")
    );
}

#[test]
fn broadcast_ack_is_scoped_to_the_sender_repository() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    harness.seed_job("job-c", "hive/job-c");
    harness.seed_job_in(SeedJob {
        owner: "acme",
        repo_name: "other",
        job_id: "job-b",
        branch: "hive/job-b",
    });
    let announced = announce(&harness.store, "job-a", "agent-a", &[String::from("src")]);
    announce(&harness.store, "job-c", "agent-c", &[String::from("docs")]);
    announce_in(
        &harness.store,
        AnnounceCase {
            owner: "acme",
            repo_name: "other",
            job_id: "job-b",
            agent: "agent-b",
            paths: &[String::from("src")],
        },
    );
    let intent_id = announced.intent.id;
    let foreign = harness.store.ack_message(AckRequest {
        message_id: intent_id,
        owner: "acme",
        repo_name: "other",
        job_id: "job-b",
        agent_id: "agent-b",
        session_id: Some("session-a"),
    });
    assert!(
        foreign.is_err(),
        "foreign-repo ACK must not mark a broadcast"
    );
    let local = harness
        .store
        .ack_message(AckRequest {
            message_id: intent_id,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-c",
            agent_id: "agent-c",
            session_id: Some("session-a"),
        })
        .unwrap();
    assert!(local.0.kind == MessageKind::Ack);
    let original = harness
        .store
        .inbox(job_key_a())
        .unwrap()
        .into_iter()
        .find(|message| message.id == intent_id)
        .unwrap();
    assert!(original.acked_at.is_some());
}

#[test]
fn send_ack_routes_through_ack_message() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &[]);
    let help = harness
        .store
        .send_message(SendRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-a",
            kind: MessageKind::Help,
            body: "need a review",
            to_agent_id: None,
            to_owner: Some("acme"),
            to_repo_name: Some("sample"),
            to_job_id: Some("job-a"),
            paths: &[],
            ack_of: None,
        })
        .unwrap();
    harness
        .store
        .send_message(SendRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-a",
            kind: MessageKind::Ack,
            body: "acked",
            to_agent_id: None,
            to_owner: None,
            to_repo_name: None,
            to_job_id: None,
            paths: &[],
            ack_of: Some(help.id),
        })
        .unwrap();
    let original = harness
        .store
        .inbox(job_key_a())
        .unwrap()
        .into_iter()
        .find(|message| message.id == help.id)
        .unwrap();
    assert!(original.acked_at.is_some());
}

#[test]
fn incomplete_recipients_are_rejected() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &[]);
    let err = harness
        .store
        .send_message(SendRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-a",
            kind: MessageKind::Help,
            body: "partial",
            to_agent_id: None,
            to_owner: None,
            to_repo_name: None,
            to_job_id: Some("job-b"),
            paths: &[],
            ack_of: None,
        })
        .unwrap_err();
    assert!(err.to_string().contains("complete owner/repository/job"));
}

#[test]
fn session_change_increments_owner_generation() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    let first = announce(&harness.store, "job-a", "agent-a", &[]);
    assert_eq!(first.claim.owner_generation, 1);
    let second = harness
        .store
        .announce(AnnounceRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-a",
            session_id: Some("session-restarted"),
            agent_type: "worker",
            intent: Some("restart"),
            paths: &[],
        })
        .unwrap();
    assert_eq!(second.claim.owner_generation, 2);
}

#[test]
fn regrant_clears_stale_coord_claim() {
    let harness = Harness::new();
    let worktree = harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &[]);
    harness.store.release_by_path(&worktree).unwrap();
    harness
        .store
        .grant(crate::lease::LeaseGrant {
            repo: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            branch: "hive/job-a",
            worktree_path: &worktree,
            start_commit: &harness.start,
        })
        .unwrap();
    let claimed = announce(&harness.store, "job-a", "agent-b", &[]);
    assert_eq!(claimed.claim.agent_id, "agent-b");
}

#[test]
fn blocker_kind_is_accepted() {
    assert_eq!(MessageKind::parse("blocker").unwrap(), MessageKind::Blocker);
}

#[test]
fn prepared_lease_cannot_announce() {
    let harness = Harness::new();
    let worktree = harness._temp.path().join("worktrees/prepared");
    fs::create_dir_all(&worktree).unwrap();
    harness
        .store
        .prepare_allocate(AllocateRequest {
            repo: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-prep",
            branch: "hive/prep",
            worktree_path: &worktree,
            requested_start_point: "refs/heads/main",
            start_commit: &harness.start,
            ttl: None,
        })
        .unwrap();
    let err = harness
        .store
        .announce(AnnounceRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-prep",
            agent_id: "agent-a",
            session_id: Some("session-a"),
            agent_type: "worker",
            intent: None,
            paths: &[],
        })
        .unwrap_err();
    assert!(err.to_string().contains("ACTIVE"));
}
