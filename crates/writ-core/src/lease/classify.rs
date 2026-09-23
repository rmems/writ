//! Lease-vs-git evidence classification.

use std::path::Path;
use std::process::Command;

use super::{
    AllocationInspection, AllocationState, EvidenceClass, InspectRequest, Lease, now_secs,
    path_text,
};

pub(super) struct GitEvidence {
    pub path_exists: bool,
    pub branch_ref: String,
    pub branch_commit: Option<String>,
    pub head_commit: Option<String>,
    pub worktree_registered: bool,
    pub checkout_common_dir: Option<std::path::PathBuf>,
    pub repo_root_common_dir: Option<std::path::PathBuf>,
}

/// Match a `git worktree list --porcelain` dump against a stored path.
///
/// Git may spell the same directory with different separators, casing, or a
/// canonical prefix than `Path::to_string_lossy`, so callers must not use
/// substring `contains`.
pub(super) fn porcelain_lists_worktree(listing: &str, worktree_path: &Path) -> bool {
    listing.lines().any(|line| {
        line.strip_prefix("worktree ")
            .is_some_and(|path| crate::paths::same_existing_path(Path::new(path), worktree_path))
    })
}

pub(super) fn inspect_git(repo_root: &Path, worktree_path: &Path, branch: &str) -> GitEvidence {
    let branch_ref = if branch.is_empty() {
        String::new()
    } else {
        format!("refs/heads/{branch}")
    };
    let branch_commit = if branch_ref.is_empty() {
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
    };
    let head_commit = optional_git_stdout(
        worktree_path,
        &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
    );
    let listed = optional_git_stdout(repo_root, &["worktree", "list", "--porcelain"])
        .is_some_and(|listing| porcelain_lists_worktree(&listing, worktree_path));
    // Harness-owned standalone clones are not in another repo's `worktree list`.
    let worktree_registered = listed || worktree_path.join(".git").exists();
    GitEvidence {
        path_exists: worktree_path.exists(),
        branch_ref,
        branch_commit,
        head_commit,
        worktree_registered,
        checkout_common_dir: git_common_dir(worktree_path),
        repo_root_common_dir: git_common_dir(repo_root),
    }
}

fn classify(
    lease: &Option<Lease>,
    evidence: &GitEvidence,
    _path: &Path,
    ttl_expired: bool,
) -> (EvidenceClass, Vec<String>) {
    match lease {
        None => classify_missing_lease(evidence),
        Some(lease) => classify_existing(lease, evidence, ttl_expired),
    }
}

fn classify_missing_lease(evidence: &GitEvidence) -> (EvidenceClass, Vec<String>) {
    if live_checkout_without_lease(evidence) {
        (
            EvidenceClass::MissingLease,
            vec!["live worktree or branch with no lease row".to_owned()],
        )
    } else {
        (EvidenceClass::Absent, Vec::new())
    }
}

fn classify_existing(
    lease: &Lease,
    evidence: &GitEvidence,
    ttl_expired: bool,
) -> (EvidenceClass, Vec<String>) {
    if let Some(class) = classify_terminal_or_retryable(lease, evidence, ttl_expired) {
        return (class, Vec::new());
    }
    if orphaned_live_lease(lease, evidence, ttl_expired) {
        return (
            EvidenceClass::OrphanedLease,
            vec!["lease heartbeat/ttl expired while worktree is still live".to_owned()],
        );
    }
    let conflicts = identity_conflicts(lease, evidence);
    if conflicts.is_empty() {
        (EvidenceClass::Matching, conflicts)
    } else {
        (EvidenceClass::NeedsAttention, conflicts)
    }
}

fn classify_terminal_or_retryable(
    lease: &Lease,
    evidence: &GitEvidence,
    _ttl_expired: bool,
) -> Option<EvidenceClass> {
    match lease.allocation_state {
        AllocationState::Tombstoned => Some(EvidenceClass::Tombstoned),
        AllocationState::Released => Some(EvidenceClass::Released),
        _ if no_git_mutation(evidence) && lease.allocation_state.is_in_progress() => {
            Some(EvidenceClass::Retryable)
        }
        _ => None,
    }
}

fn no_git_mutation(evidence: &GitEvidence) -> bool {
    !evidence.path_exists
        && evidence.branch_commit.is_none()
        && evidence.head_commit.is_none()
        && !evidence.worktree_registered
}

fn identity_conflicts(lease: &Lease, evidence: &GitEvidence) -> Vec<String> {
    let mut conflicts = Vec::new();
    if !evidence.path_exists {
        conflicts.push("derived path is missing".to_owned());
    }
    if !evidence.worktree_registered {
        conflicts.push("worktree is not registered".to_owned());
    }
    if let Some(conflict) = repo_identity_conflict(lease, evidence) {
        conflicts.push(conflict);
    }
    if named_git_branch(&lease.branch)
        && let Some(conflict) = commit_conflict(
            evidence.branch_commit.as_deref(),
            &lease.start_commit,
            "branch commit",
            "branch commit is absent",
        )
    {
        conflicts.push(conflict);
    }
    if let Some(conflict) = commit_conflict(
        evidence.head_commit.as_deref(),
        &lease.start_commit,
        "worker HEAD",
        "worker HEAD is absent",
    ) {
        conflicts.push(conflict);
    }
    conflicts
}

fn commit_conflict(
    observed: Option<&str>,
    expected: &str,
    present_label: &str,
    absent: &str,
) -> Option<String> {
    match observed {
        Some(commit) if commit == expected => None,
        Some(commit) => Some(format!(
            "{present_label} {commit} != resolved start {expected}"
        )),
        None if expected.is_empty() => None,
        None => Some(absent.to_owned()),
    }
}

fn repo_identity_conflict(_lease: &Lease, evidence: &GitEvidence) -> Option<String> {
    match (
        evidence.checkout_common_dir.as_deref(),
        evidence.repo_root_common_dir.as_deref(),
    ) {
        (Some(checkout), Some(repo_root))
            if crate::paths::same_existing_path(checkout, repo_root) =>
        {
            None
        }
        (Some(_), Some(_)) => {
            Some("checkout and repo_root do not share the stored lease's git identity".to_owned())
        }
        _ if evidence.path_exists => {
            Some("checkout and repo_root do not share the stored lease's git identity".to_owned())
        }
        _ => None,
    }
}

fn git_common_dir(root: &Path) -> Option<std::path::PathBuf> {
    let raw = optional_git_stdout(root, &["rev-parse", "--git-common-dir"])?;
    let candidate = Path::new(&raw);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    Some(crate::paths::canonicalize_for_tools(&joined).unwrap_or(joined))
}

fn named_git_branch(branch: &str) -> bool {
    !branch.is_empty() && branch != "(detached)"
}

fn ttl_is_expired(lease: &Lease) -> bool {
    let (Some(ttl), Some(heartbeat)) = (lease.ttl, lease.heartbeat) else {
        return false;
    };
    now_secs().saturating_sub(heartbeat) > ttl
}

pub(super) fn inspect_now(
    lease: &Option<Lease>,
    request: InspectRequest<'_>,
) -> AllocationInspection {
    let branch = lease
        .as_ref()
        .map(|row| row.branch.as_str())
        .or(request.branch)
        .unwrap_or("");
    let branch_ref = if branch.is_empty() {
        String::new()
    } else {
        format!("refs/heads/{branch}")
    };
    let evidence = inspect_git(request.repo_root, request.worktree_path, branch);
    let ttl_expired = lease.as_ref().is_some_and(ttl_is_expired);
    let (classification, conflicts) =
        classify(lease, &evidence, request.worktree_path, ttl_expired);
    AllocationInspection {
        operation_id: lease.as_ref().map(|row| row.operation_id.clone()),
        allocation_state: lease
            .as_ref()
            .map(|row| row.allocation_state.as_str().to_owned()),
        requested_start_point: lease.as_ref().map(|row| row.requested_start_point.clone()),
        resolved_start_commit: lease.as_ref().map(|row| row.start_commit.clone()),
        derived_path: request.worktree_path.to_path_buf(),
        path_exists: evidence.path_exists,
        branch_ref: if evidence.branch_ref.is_empty() {
            branch_ref
        } else {
            evidence.branch_ref
        },
        branch_commit: evidence.branch_commit,
        head_commit: evidence.head_commit,
        worktree_registered: evidence.worktree_registered,
        repo_identity: path_text(request.repo_root),
        lease_present: lease.is_some(),
        ttl_expired,
        classification,
        conflicts,
    }
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

fn live_checkout_without_lease(evidence: &GitEvidence) -> bool {
    evidence.path_exists || evidence.branch_commit.is_some() || evidence.worktree_registered
}

fn orphaned_live_lease(lease: &Lease, evidence: &GitEvidence, ttl_expired: bool) -> bool {
    lease.allocation_state == AllocationState::Active && ttl_expired && evidence.path_exists
}
