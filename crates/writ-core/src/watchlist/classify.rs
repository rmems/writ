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
        match check_class(&check.state) {
            CheckClass::Failed => {
                failed = true;
                blockers.push(format!("class_a:{}", sanitize_token(&check.name)));
            }
            CheckClass::ActionRequired => {
                blockers.push(format!("class_b:{}", sanitize_token(&check.name)));
            }
            CheckClass::Pending => {
                *pending = true;
                blockers.push(format!("class_c:{}", sanitize_token(&check.name)));
            }
            CheckClass::Other => {}
        }
    }
    failed
}

enum CheckClass {
    Failed,
    ActionRequired,
    Pending,
    Other,
}

fn check_class(state: &str) -> CheckClass {
    let state = state.to_ascii_uppercase();
    if is_failed_check(&state) {
        CheckClass::Failed
    } else if state == "ACTION_REQUIRED" {
        CheckClass::ActionRequired
    } else if is_pending_check(&state) {
        CheckClass::Pending
    } else {
        CheckClass::Other
    }
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
        return "failed".to_owned();
    }
    if pending {
        return "pending".to_owned();
    }
    if snapshot.checks.is_empty() && blockers.is_empty() {
        return "pending".to_owned();
    }
    if blockers.is_empty() {
        "healthy".to_owned()
    } else {
        "residual".to_owned()
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
            head_owner: Some("acme".to_owned()),
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

    fn check(name: &str, state: &str) -> CheckSnapshot {
        CheckSnapshot {
            name: name.to_owned(),
            state: state.to_owned(),
        }
    }

    fn classify_after(edit: impl FnOnce(&mut PrSnapshot)) -> (String, Vec<String>) {
        let mut pr = open_pr();
        edit(&mut pr);
        classify_snapshot(&pr)
    }

    #[test]
    fn snapshot_status_matrix() {
        let empty = classify_snapshot(&open_pr());
        assert_eq!(empty.0, "pending");
        assert!(empty.1.is_empty());

        let (status, blockers) = classify_after(|pr| pr.mergeable = Some("CONFLICTING".to_owned()));
        assert_eq!(status, "conflict");
        assert_eq!(blockers, vec!["conflict:mergeable"]);

        let (status, blockers) =
            classify_after(|pr| pr.checks = vec![check("Codacy", "ACTION_REQUIRED")]);
        assert_eq!(status, "residual");
        assert_eq!(blockers, vec!["class_b:codacy"]);

        let (status, _) = classify_after(|pr| pr.state = "MERGED".to_owned());
        assert_eq!(status, "merged");

        let (status, blockers) = classify_after(|pr| pr.state = "CLOSED".to_owned());
        assert_eq!(status, "closed");
        assert!(blockers.is_empty());

        let (status, blockers) = classify_after(|pr| {
            pr.is_draft = true;
            pr.checks = vec![check("ci", "SUCCESS")];
        });
        assert_eq!(status, "pending");
        assert!(blockers.iter().any(|b| b == "draft:true"));

        let (status, blockers) = classify_after(|pr| {
            pr.review_decision = Some("REVIEW_REQUIRED".to_owned());
            pr.checks = vec![check("CI / Test", "FAILURE")];
        });
        assert_eq!(status, "failed");
        assert!(blockers.iter().any(|b| b == "class_a:ci___test"));
        assert!(blockers.iter().any(|b| b == "review:review_required"));

        let (status, blockers) = classify_after(|pr| pr.checks = vec![check("ci", "SUCCESS")]);
        assert_eq!(status, "healthy");
        assert!(blockers.is_empty());

        let (status, blockers) = classify_after(|pr| {
            pr.review_decision = Some("CHANGES_REQUESTED".to_owned());
            pr.checks = vec![check("ci", "SUCCESS")];
        });
        assert_eq!(status, "residual");
        assert_eq!(blockers, vec!["review:changes_requested"]);
    }

    #[test]
    fn empty_check_name_sanitizes() {
        assert_eq!(sanitize_token(""), "unnamed");
        assert_eq!(sanitize_token("!!!"), "___");
    }
}
