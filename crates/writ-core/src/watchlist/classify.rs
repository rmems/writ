//! Classify a live GitHub PR snapshot into check status and residual codes.

use super::github::{CheckSnapshot, PrSnapshot};

/// Map GitHub metadata into a visibility `check_status` plus residual codes.
pub fn classify_snapshot(snapshot: &PrSnapshot) -> (String, Vec<String>) {
    if snapshot.state.eq_ignore_ascii_case("MERGED") {
        return ("merged".to_owned(), Vec::new());
    }
    if snapshot.state.eq_ignore_ascii_case("CLOSED") {
        return ("closed".to_owned(), Vec::new());
    }
    let mergeable = snapshot
        .mergeable
        .as_deref()
        .unwrap_or("")
        .to_ascii_uppercase();
    if mergeable == "CONFLICTING" {
        return ("conflict".to_owned(), vec!["conflict:mergeable".to_owned()]);
    }

    let mut blockers = Vec::new();
    let mut pending = classify_readiness(snapshot, &mergeable, &mut blockers);
    let failed = classify_checks(&snapshot.checks, &mut blockers, &mut pending);
    classify_review(snapshot.review_decision.as_deref(), &mut blockers);
    (
        resolve_status(snapshot, &blockers, failed, pending),
        blockers,
    )
}

fn classify_readiness(snapshot: &PrSnapshot, mergeable: &str, blockers: &mut Vec<String>) -> bool {
    let mut pending = false;
    if snapshot.is_draft {
        pending = true;
        blockers.push("draft:true".to_owned());
    }
    if mergeable != "MERGEABLE" {
        pending = true;
        blockers.push("pending:mergeable_unknown".to_owned());
    }
    pending
}

fn classify_checks(
    checks: &[CheckSnapshot],
    blockers: &mut Vec<String>,
    pending: &mut bool,
) -> bool {
    let mut failed = false;
    for check in checks {
        let state = check.state.to_ascii_uppercase();
        if is_failed_check(&state) {
            failed = true;
            blockers.push(format!("class_a:{}", sanitize_token(&check.name)));
        } else if state == "ACTION_REQUIRED" {
            blockers.push(format!("class_b:{}", sanitize_token(&check.name)));
        } else if is_pending_check(&state) {
            *pending = true;
            blockers.push(format!("class_c:{}", sanitize_token(&check.name)));
        }
    }
    failed
}

fn is_failed_check(state: &str) -> bool {
    matches!(
        state,
        "FAILURE" | "FAIL" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "STARTUP_FAILURE" | "STALE"
    )
}

fn is_pending_check(state: &str) -> bool {
    matches!(
        state,
        "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | "UNKNOWN"
    )
}

fn classify_review(review_decision: Option<&str>, blockers: &mut Vec<String>) {
    let Some(decision) = review_decision else {
        return;
    };
    let decision = decision.to_ascii_uppercase();
    if decision == "REVIEW_REQUIRED" || decision == "CHANGES_REQUESTED" {
        blockers.push(format!("review:{}", sanitize_token(&decision)));
    }
}

fn resolve_status(
    snapshot: &PrSnapshot,
    blockers: &[String],
    failed: bool,
    pending: bool,
) -> String {
    if failed {
        "failed".to_owned()
    } else if pending || (snapshot.checks.is_empty() && blockers.is_empty()) {
        "pending".to_owned()
    } else if !blockers.is_empty() {
        "residual".to_owned()
    } else {
        "healthy".to_owned()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watchlist::github::PrSnapshot;

    fn open_pr() -> PrSnapshot {
        PrSnapshot {
            repo: "acme/sample".to_owned(),
            number: 7,
            branch: "hive/job".to_owned(),
            base: "main".to_owned(),
            title: "t".to_owned(),
            url: "https://example.test/7".to_owned(),
            state: "OPEN".to_owned(),
            mergeable: Some("MERGEABLE".to_owned()),
            review_decision: None,
            is_draft: false,
            checks: Vec::new(),
        }
    }

    #[test]
    fn empty_checks_are_pending() {
        let (status, blockers) = classify_snapshot(&open_pr());
        assert_eq!(status, "pending");
        assert!(blockers.is_empty());
    }

    #[test]
    fn conflicting_mergeable_is_conflict() {
        let mut pr = open_pr();
        pr.mergeable = Some("CONFLICTING".to_owned());
        let (status, blockers) = classify_snapshot(&pr);
        assert_eq!(status, "conflict");
        assert_eq!(blockers, vec!["conflict:mergeable"]);
    }

    #[test]
    fn action_required_is_residual() {
        let mut pr = open_pr();
        pr.checks = vec![CheckSnapshot {
            name: "Codacy".to_owned(),
            state: "ACTION_REQUIRED".to_owned(),
        }];
        let (status, blockers) = classify_snapshot(&pr);
        assert_eq!(status, "residual");
        assert_eq!(blockers, vec!["class_b:codacy"]);
    }

    #[test]
    fn merged_is_terminal() {
        let mut pr = open_pr();
        pr.state = "MERGED".to_owned();
        let (status, _) = classify_snapshot(&pr);
        assert_eq!(status, "merged");
    }
}
