//! Classify a PR snapshot into watchlist status and residual blockers.

use super::probe::{CheckSnapshot, PrSnapshot};
use super::schema::WatchStatus;

pub(crate) fn classify_snapshot(snapshot: &PrSnapshot) -> (WatchStatus, Vec<String>) {
    let mergeable = snapshot
        .mergeable
        .as_deref()
        .unwrap_or("")
        .to_ascii_uppercase();
    if mergeable == "CONFLICTING" {
        return (WatchStatus::Conflict, vec!["conflict:mergeable".to_owned()]);
    }

    let mut blockers = Vec::new();
    let mut pending = classify_readiness(snapshot, &mergeable, &mut blockers);
    let failed = classify_checks(&snapshot.checks, &mut blockers, &mut pending);
    classify_review(snapshot.review_decision.as_deref(), &mut blockers);

    resolve_status(snapshot, &blockers, failed, pending)
}

/// Record draft and unresolved-mergeability blockers; returns whether either
/// forces a pending status.
fn classify_readiness(snapshot: &PrSnapshot, mergeable: &str, blockers: &mut Vec<String>) -> bool {
    let mut pending = false;
    // A draft PR is not ready to merge regardless of CI, so it cannot be
    // Healthy; surface it as pending with an explicit blocker.
    if snapshot.is_draft {
        pending = true;
        blockers.push("draft:true".to_owned());
    }
    // GitHub reports `mergeable == UNKNOWN` (or empty/missing) while the merge
    // is still being computed. Do not declare an unresolved merge Healthy;
    // treat it as pending until it resolves to MERGEABLE or CONFLICTING.
    if mergeable != "MERGEABLE" {
        pending = true;
        blockers.push("pending:mergeable_unknown".to_owned());
    }
    pending
}

/// Push class_a/class_b/class_c tokens for CI checks. Returns whether any
/// check failed hard; sets `pending` when a check is still in flight.
fn classify_checks(
    checks: &[CheckSnapshot],
    blockers: &mut Vec<String>,
    pending: &mut bool,
) -> bool {
    let mut failed = false;
    for check in checks {
        let state = check.state.to_ascii_uppercase();
        if matches!(
            state.as_str(),
            "FAILURE" | "FAIL" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "STARTUP_FAILURE" | "STALE"
        ) {
            failed = true;
            blockers.push(format!("class_a:{}", sanitize_token(&check.name)));
        } else if state == "ACTION_REQUIRED" {
            blockers.push(format!("class_b:{}", sanitize_token(&check.name)));
        } else if matches!(
            state.as_str(),
            "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | "UNKNOWN"
        ) {
            *pending = true;
            blockers.push(format!("class_c:{}", sanitize_token(&check.name)));
        }
    }
    failed
}

/// Push a `review:` blocker when the review decision still gates the merge.
fn classify_review(review_decision: Option<&str>, blockers: &mut Vec<String>) {
    if let Some(decision) = review_decision {
        let decision = decision.to_ascii_uppercase();
        if decision == "REVIEW_REQUIRED" || decision == "CHANGES_REQUESTED" {
            blockers.push(format!("review:{}", sanitize_token(&decision)));
        }
    }
}

/// Collapse the failed/pending/blocker signals into a single [`WatchStatus`].
fn resolve_status(
    snapshot: &PrSnapshot,
    blockers: &[String],
    failed: bool,
    pending: bool,
) -> (WatchStatus, Vec<String>) {
    let status = if failed {
        WatchStatus::Failed
    } else if pending || (snapshot.checks.is_empty() && blockers.is_empty()) {
        WatchStatus::Pending
    } else if !blockers.is_empty() {
        WatchStatus::Residual
    } else {
        WatchStatus::Healthy
    };
    (status, blockers.to_vec())
}

fn sanitize_token(name: &str) -> String {
    let token: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if token.is_empty() {
        "unnamed".to_owned()
    } else {
        token
    }
}
