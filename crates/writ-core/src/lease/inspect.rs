//! Read-only git evidence and lease classification.

use std::path::Path;
use std::process::Command;

use crate::error::Result;

use super::LeaseStore;
use super::types::{
    AllocationInspection, AllocationState, EvidenceClass, InspectRequest, JobKey, Lease,
};
use super::util::{now_secs, path_text};

const MISSING_LEASE: &str = "live worktree or branch with no lease row";
const EXPIRED_LIVE: &str = "lease heartbeat/ttl expired while worktree is still live";

pub(in crate::lease) struct GitEvidence {
    pub path_exists: bool,
    pub branch_ref: String,
    pub branch_commit: Option<String>,
    pub head_commit: Option<String>,
    pub worktree_registered: bool,
}

struct InspectParts<'a> {
    lease: &'a Option<Lease>,
    request: InspectRequest<'a>,
    branch: &'a str,
    evidence: GitEvidence,
    ttl_expired: bool,
    classification: EvidenceClass,
    conflicts: Vec<String>,
}

struct CommitCheck<'a> {
    observed: Option<&'a str>,
    expected: &'a str,
    label: &'a str,
    absent: &'a str,
}

impl LeaseStore {
    /// Report derived path, refs, HEAD, registration, and operation identity.
    ///
    /// This never mutates git state, never adopts a worktree, and never writes
    /// the lease store.
    pub fn inspect(&self, request: InspectRequest<'_>) -> Result<AllocationInspection> {
        let lease = self.find_job(JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        })?;
        Ok(inspect_now(&lease, request))
    }
}

pub(in crate::lease) fn inspect_now(
    lease: &Option<Lease>,
    request: InspectRequest<'_>,
) -> AllocationInspection {
    let branch = requested_branch(lease, request.branch);
    let evidence = inspect_git(request.repo_root, request.worktree_path, branch);
    let ttl_expired = lease.as_ref().is_some_and(ttl_is_expired);
    let (classification, conflicts) = classify(lease, &evidence, ttl_expired);
    inspection_from(InspectParts {
        lease,
        request,
        branch,
        evidence,
        ttl_expired,
        classification,
        conflicts,
    })
}

pub(in crate::lease) fn inspect_git(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
) -> GitEvidence {
    let branch_ref = branch_ref_for(branch);
    let commit = branch_commit(repo_root, &branch_ref);
    GitEvidence {
        path_exists: worktree_path.exists(),
        branch_ref,
        branch_commit: commit,
        head_commit: head_commit(worktree_path),
        worktree_registered: worktree_is_registered(repo_root, worktree_path),
    }
}

fn classify(
    lease: &Option<Lease>,
    evidence: &GitEvidence,
    ttl_expired: bool,
) -> (EvidenceClass, Vec<String>) {
    match lease {
        None => classify_absent(evidence),
        Some(lease) => classify_present(lease, evidence, ttl_expired),
    }
}

fn classify_absent(evidence: &GitEvidence) -> (EvidenceClass, Vec<String>) {
    if live_git_without_lease(evidence) {
        (EvidenceClass::MissingLease, vec![MISSING_LEASE.to_owned()])
    } else {
        (EvidenceClass::Absent, Vec::new())
    }
}

fn classify_present(
    lease: &Lease,
    evidence: &GitEvidence,
    ttl_expired: bool,
) -> (EvidenceClass, Vec<String>) {
    match present_class(lease, evidence, ttl_expired) {
        PresentClass::Terminal(class) => (class, Vec::new()),
        PresentClass::Orphaned => (EvidenceClass::OrphanedLease, vec![EXPIRED_LIVE.to_owned()]),
        PresentClass::Retryable => (EvidenceClass::Retryable, Vec::new()),
        PresentClass::Conflicts => classify_conflicts(lease, evidence),
    }
}

enum PresentClass {
    Terminal(EvidenceClass),
    Orphaned,
    Retryable,
    Conflicts,
}

fn present_class(lease: &Lease, evidence: &GitEvidence, ttl_expired: bool) -> PresentClass {
    if let Some(class) = terminal_evidence(lease.allocation_state) {
        return PresentClass::Terminal(class);
    }
    if expired_live_active(lease, evidence, ttl_expired) {
        return PresentClass::Orphaned;
    }
    if retryable(lease, evidence) {
        return PresentClass::Retryable;
    }
    PresentClass::Conflicts
}

fn terminal_evidence(state: AllocationState) -> Option<EvidenceClass> {
    match state {
        AllocationState::Tombstoned => Some(EvidenceClass::Tombstoned),
        AllocationState::Released => Some(EvidenceClass::Released),
        _ => None,
    }
}

fn expired_live_active(lease: &Lease, evidence: &GitEvidence, ttl_expired: bool) -> bool {
    lease.allocation_state == AllocationState::Active && ttl_expired && evidence.path_exists
}

fn retryable(lease: &Lease, evidence: &GitEvidence) -> bool {
    lease.allocation_state.is_in_progress() && no_mutation(evidence)
}

fn no_mutation(evidence: &GitEvidence) -> bool {
    !evidence.path_exists
        && evidence.branch_commit.is_none()
        && evidence.head_commit.is_none()
        && !evidence.worktree_registered
}

fn live_git_without_lease(evidence: &GitEvidence) -> bool {
    evidence.path_exists || evidence.branch_commit.is_some() || evidence.worktree_registered
}

fn classify_conflicts(lease: &Lease, evidence: &GitEvidence) -> (EvidenceClass, Vec<String>) {
    let conflicts = conflict_list(lease, evidence);
    if conflicts.is_empty() {
        (EvidenceClass::Matching, conflicts)
    } else {
        (EvidenceClass::NeedsAttention, conflicts)
    }
}

fn conflict_list(lease: &Lease, evidence: &GitEvidence) -> Vec<String> {
    let mut conflicts = Vec::new();
    push_if_missing(
        &mut conflicts,
        evidence.path_exists,
        "derived path is missing",
    );
    push_if_missing(
        &mut conflicts,
        evidence.worktree_registered,
        "worktree is not registered",
    );
    push_commit_conflict(
        &mut conflicts,
        CommitCheck {
            observed: evidence.branch_commit.as_deref(),
            expected: &lease.start_commit,
            label: "branch commit",
            absent: "branch commit is absent",
        },
    );
    push_commit_conflict(
        &mut conflicts,
        CommitCheck {
            observed: evidence.head_commit.as_deref(),
            expected: &lease.start_commit,
            label: "worker HEAD",
            absent: "worker HEAD is absent",
        },
    );
    conflicts
}

fn push_if_missing(conflicts: &mut Vec<String>, present: bool, message: &str) {
    if !present {
        conflicts.push(message.to_owned());
    }
}

fn push_commit_conflict(conflicts: &mut Vec<String>, check: CommitCheck<'_>) {
    match check.observed {
        Some(commit) if commit == check.expected => {}
        Some(commit) => conflicts.push(format!(
            "{} {commit} != resolved start {}",
            check.label, check.expected
        )),
        None => conflicts.push(check.absent.to_owned()),
    }
}

fn ttl_is_expired(lease: &Lease) -> bool {
    let (Some(ttl), Some(heartbeat)) = (lease.ttl, lease.heartbeat) else {
        return false;
    };
    now_secs().saturating_sub(heartbeat) > ttl
}

fn requested_branch<'a>(lease: &'a Option<Lease>, fallback: Option<&'a str>) -> &'a str {
    lease
        .as_ref()
        .map(|row| row.branch.as_str())
        .or(fallback)
        .unwrap_or("")
}

fn inspection_from(parts: InspectParts<'_>) -> AllocationInspection {
    AllocationInspection {
        operation_id: parts.lease.as_ref().map(|row| row.operation_id.clone()),
        allocation_state: parts
            .lease
            .as_ref()
            .map(|row| row.allocation_state.as_str().to_owned()),
        requested_start_point: parts
            .lease
            .as_ref()
            .map(|row| row.requested_start_point.clone()),
        resolved_start_commit: parts.lease.as_ref().map(|row| row.start_commit.clone()),
        derived_path: parts.request.worktree_path.to_path_buf(),
        path_exists: parts.evidence.path_exists,
        branch_ref: prefer_evidence_ref(parts.evidence.branch_ref, branch_ref_for(parts.branch)),
        branch_commit: parts.evidence.branch_commit,
        head_commit: parts.evidence.head_commit,
        worktree_registered: parts.evidence.worktree_registered,
        repo_identity: path_text(parts.request.repo_root),
        lease_present: parts.lease.is_some(),
        ttl_expired: parts.ttl_expired,
        classification: parts.classification,
        conflicts: parts.conflicts,
    }
}

fn prefer_evidence_ref(evidence_ref: String, fallback: String) -> String {
    if evidence_ref.is_empty() {
        fallback
    } else {
        evidence_ref
    }
}

fn branch_ref_for(branch: &str) -> String {
    if branch.is_empty() {
        String::new()
    } else {
        format!("refs/heads/{branch}")
    }
}

fn branch_commit(repo_root: &Path, branch_ref: &str) -> Option<String> {
    if branch_ref.is_empty() {
        None
    } else {
        optional_git_stdout(
            repo_root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{branch_ref}^{{commit}}"),
            ],
        )
    }
}

fn head_commit(worktree_path: &Path) -> Option<String> {
    optional_git_stdout(
        worktree_path,
        &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
    )
}

fn worktree_is_registered(repo_root: &Path, worktree_path: &Path) -> bool {
    optional_git_stdout(repo_root, &["worktree", "list", "--porcelain"]).is_some_and(|listing| {
        listing
            .lines()
            .any(|line| registered_worktree_line(line, worktree_path))
    })
}

fn registered_worktree_line(line: &str, worktree_path: &Path) -> bool {
    line.strip_prefix("worktree ")
        .is_some_and(|path| crate::paths::same_existing_path(Path::new(path), worktree_path))
}

fn optional_git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
}
