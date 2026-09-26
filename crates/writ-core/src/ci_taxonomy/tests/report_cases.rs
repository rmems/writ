use serde_json::json;

use super::super::*;
use super::{ACTIONS_URL, CODACY_URL, KILO_URL, classify_named, raw_check};

#[test]
fn skipping_is_not_a_performed_pass() {
    let report = classify_checks(&[raw_check(&sample!(
        "Supabase Preview",
        "",
        "skipping",
        "https://supabase.com/preview"
    ))]);
    let collab = report.collaboration_status();
    assert_eq!(
        (
            report.all_passed(),
            report.job_ci_class(),
            report.failures().is_empty(),
            report.residual_codes().is_empty(),
            report.skipped_checks().len(),
            report.checks[0].observation,
            &report.checks[0].recommended_action,
            collab.skipped_count,
            collab.blocks_unrelated_workers,
            collab.continue_other_work,
            collab.ci_class,
        ),
        (
            false,
            CiClass::Unknown,
            true,
            true,
            1,
            ObservationKind::Skipping,
            &RecommendedAction::Ignore,
            1,
            false,
            true,
            CiClass::Unknown,
        )
    );
}

#[test]
fn skip_plus_success_is_all_passed() {
    let report = classify_checks(&[
        raw_check(&sample!("Build & Test", "CI", "pass", ACTIONS_URL)),
        raw_check(&sample!("Supabase Preview", "", "skipping", "")),
    ]);
    assert_eq!(
        (
            report.all_passed(),
            report.job_ci_class(),
            report.skipped_checks().len(),
            report.failures().is_empty(),
        ),
        (true, CiClass::Pass, 1, true)
    );
}

#[test]
fn required_skip_is_not_a_performed_pass() {
    let report = classify_checks_json(&json!([{
        "name": "optional-preview",
        "bucket": "skipping",
        "isRequired": true,
    }]))
    .unwrap();
    assert_eq!(
        (
            report.all_passed(),
            report.job_ci_class(),
            report.checks[0].observation,
            report.required_failures().is_empty(),
            report.collaboration_status().continue_other_work,
        ),
        (
            false,
            CiClass::Unknown,
            ObservationKind::Skipping,
            true,
            true
        )
    );
}

#[test]
fn pending_class_a_has_no_rerun_and_is_not_all_passed() {
    let report = classify_checks(&[raw_check(&sample!(
        "Build & Test",
        "CI",
        "pending",
        ACTIONS_URL
    ))]);
    let check = &report.checks[0];
    assert_eq!(
        (
            report.all_passed(),
            check.policies.is_empty(),
            should_rerun(check),
            rerun_command(check).is_none(),
            check.recommended_action == RecommendedAction::Wait,
            check.residual_code.is_none(),
        ),
        (false, true, false, true, true, true)
    );
}

#[test]
fn empty_rollup_is_not_success() {
    assert!(!classify_checks(&[]).all_passed());
}

#[test]
fn stale_is_a_failure_not_success() {
    let report = classify_checks(&[raw_check(&sample!("CI", "CI", "stale", ACTIONS_URL))]);
    assert_eq!((report.failures().len(), report.all_passed()), (1, false));
}

#[test]
fn mixed_report_separates_fixable_and_residual() {
    let report = classify_checks(&[
        raw_check(&sample!("Build & Test", "CI", "fail", ACTIONS_URL)),
        raw_check(&sample!(
            "Codacy Static Code Analysis",
            "",
            "fail",
            "https://app.codacy.com/gh/acme/example-org/pull-requests/1"
        )),
        raw_check(&sample!("Kilo Code Review", "", "fail", KILO_URL)),
        raw_check(&sample!("Supabase Preview", "", "skipping", "")),
    ]);
    assert_eq!(
        (
            report.all_passed(),
            report.failures().len(),
            report.fixable_failures().len(),
            report.class_a().len(),
            report.class_b().len(),
            report.class_c().len(),
            report.required_failures().is_empty(),
            report.unknown_requiredness().len(),
        ),
        (false, 3, 2, 1, 1, 2, true, 3)
    );
    assert_eq!(
        report.residual_codes(),
        vec!["class_c:kilo_fail".to_owned()]
    );
    assert_eq!(
        report.observation_codes(ObservationKind::UnknownRequiredness),
        vec![
            "class_c:kilo_fail".to_owned(),
            "unknown_requiredness:build_test".to_owned(),
            "unknown_requiredness:codacy_static_code_analysis".to_owned(),
        ]
    );
}

#[test]
fn required_failure_observation_is_fix_source() {
    let required = classify_check(parse_check_entry(&json!({
        "name": "Build & Test",
        "workflow": "CI",
        "bucket": "fail",
        "link": ACTIONS_URL,
        "isRequired": true,
    })));
    assert_eq!(
        (required.observation, required.recommended_action),
        (
            ObservationKind::RequiredFailure,
            RecommendedAction::FixSource
        )
    );
}

#[test]
fn advisory_failure_is_not_first_party_gate() {
    let advisory = classify_check(parse_check_entry(&json!({
        "name": "qlty check",
        "bucket": "fail",
        "link": "https://qlty.sh/gh/acme/example-org/pull/1",
        "isRequired": false,
    })));
    assert_eq!(
        (advisory.observation, advisory.check_class),
        (ObservationKind::AdvisoryFinding, CheckClass::C)
    );
}

#[test]
fn unknown_and_external_and_pending_observations() {
    let unknown = classify_named(&sample!("Build & Test", "CI", "fail", ACTIONS_URL));
    let external = classify_check(parse_check_entry(&json!({
        "name": "Codacy Static Code Analysis",
        "link": CODACY_URL,
        "conclusion": "ACTION_REQUIRED",
        "isRequired": true,
    })));
    let pending = classify_named(&sample!("Build & Test", "CI", "pending", ACTIONS_URL));
    assert_eq!(
        (
            unknown.observation,
            external.observation,
            external.residual_code.as_deref(),
            pending.observation,
        ),
        (
            ObservationKind::UnknownRequiredness,
            ObservationKind::ExternalAccess,
            Some("class_b:codacy_action_required"),
            ObservationKind::Pending,
        )
    );
}

#[test]
fn class_c_is_required_only_when_payload_says_so() {
    let required_bot = classify_check(parse_check_entry(&json!({
        "name": "Kilo Code Review",
        "bucket": "fail",
        "link": KILO_URL,
        "isRequired": true,
    })));
    let unnamed = classify_named(&sample!("Kilo Code Review", "", "fail", KILO_URL));
    assert_eq!(
        (
            required_bot.check_class,
            required_bot.observation,
            unnamed.observation,
        ),
        (
            CheckClass::C,
            ObservationKind::RequiredFailure,
            ObservationKind::UnknownRequiredness,
        )
    );
}

fn source_fix_and_residuals() -> ClassificationReport {
    classify_checks_json(&json!([
        {
            "name": "Build & Test",
            "workflow": "CI",
            "bucket": "fail",
            "link": ACTIONS_URL,
            "isRequired": true,
        },
        {
            "name": "Build & Test",
            "workflow": "CI",
            "bucket": "timed_out",
            "link": ACTIONS_URL,
        },
        {
            "name": "Codacy Static Code Analysis",
            "link": CODACY_URL,
            "conclusion": "ACTION_REQUIRED",
        },
        {
            "name": "Kilo Code Review",
            "bucket": "fail",
            "link": KILO_URL,
        }
    ]))
    .unwrap()
}

#[test]
fn fixable_failures_are_source_fixes_only() {
    let report = source_fix_and_residuals();
    assert_eq!(
        (
            report.fixable_failures().len(),
            report.fixable_failures()[0].entry.name.as_str(),
            report.required_failures().len(),
            report.external_access().len(),
            should_rerun(report.fixable_failures()[0]),
            should_rerun(&report.checks[1]),
        ),
        (1, "Build & Test", 1, 1, false, true)
    );
    assert_eq!(
        report.observation_codes(ObservationKind::ExternalAccess),
        vec!["class_b:codacy_action_required".to_owned()]
    );
}

#[test]
fn classify_response_unknown_is_not_a_gate() {
    let report = classify_checks(&[raw_check(&sample!(
        "Build & Test",
        "CI",
        "fail",
        ACTIONS_URL
    ))]);
    let data = classify_response_data(&report);
    assert_eq!(data["required_failure_count"], 0);
    assert_eq!(data["github_is_required_check_authority"], true);
    assert_eq!(data["unknown_requiredness_is_not_a_writ_merge_gate"], true);
}

#[test]
fn classify_response_records_unknown_code_and_note() {
    let report = classify_checks(&[raw_check(&sample!(
        "Build & Test",
        "CI",
        "fail",
        ACTIONS_URL
    ))]);
    let data = classify_response_data(&report);
    assert_eq!(
        data["unknown_requiredness_codes"],
        json!(["unknown_requiredness:build_test"])
    );
    assert_eq!(data["skipped_codes"], json!([]));
    assert!(
        data["operator_note"]
            .as_str()
            .unwrap()
            .contains("GitHub is the required-check authority")
    );
}

#[test]
fn classify_response_collaboration_stays_open() {
    let report = classify_checks(&[raw_check(&sample!(
        "Build & Test",
        "CI",
        "fail",
        ACTIONS_URL
    ))]);
    let data = classify_response_data(&report);
    assert_eq!(
        (
            data["collaboration"]["ci_class"].as_str(),
            data["collaboration"]["blocks_unrelated_workers"].as_bool(),
            data["collaboration"]["continue_other_work"].as_bool(),
            data["collaboration"]["forbid_empty_retrigger_commit"].as_bool(),
            data["collaboration"]["skipped_count"].as_u64(),
        ),
        (
            Some("unknown"),
            Some(false),
            Some(true),
            Some(true),
            Some(0)
        )
    );
}

#[test]
fn unknown_failure_does_not_freeze_unrelated_workers() {
    let unknown_fail = classify_checks(&[raw_check(&sample!(
        "Build & Test",
        "CI",
        "fail",
        ACTIONS_URL
    ))]);
    let collab = unknown_fail.collaboration_status();
    assert_eq!(
        (
            unknown_fail.job_ci_class(),
            collab.blocks_unrelated_workers,
            collab.continue_other_work,
            collab.unknown_requiredness_count,
        ),
        (CiClass::Unknown, false, true, 1)
    );
}

#[test]
fn pending_rollup_continues_other_work() {
    let pending = classify_checks(&[raw_check(&sample!(
        "Build & Test",
        "CI",
        "pending",
        ACTIONS_URL
    ))]);
    assert_eq!(
        (
            pending.job_ci_class(),
            pending.collaboration_status().continue_other_work,
        ),
        (CiClass::Pending, true)
    );
}

#[test]
fn external_access_does_not_freeze_unrelated_workers() {
    let external = classify_checks_json(&json!([{
        "name": "Codacy Static Code Analysis",
        "link": CODACY_URL,
        "conclusion": "ACTION_REQUIRED",
    }]))
    .unwrap();
    let collab = external.collaboration_status();
    assert_eq!(
        (
            external.job_ci_class(),
            collab.external_access_count,
            collab.blocks_unrelated_workers,
        ),
        (CiClass::Unknown, 1, false)
    );
}

#[test]
fn required_failure_stops_other_work_without_blocking_unrelated() {
    let required_fail = classify_checks_json(&json!([{
        "name": "Build & Test",
        "workflow": "CI",
        "bucket": "fail",
        "link": ACTIONS_URL,
        "isRequired": true,
    }]))
    .unwrap();
    let collab = required_fail.collaboration_status();
    assert_eq!(
        (
            required_fail.job_ci_class(),
            collab.continue_other_work,
            collab.blocks_unrelated_workers,
        ),
        (CiClass::Fail, false, false)
    );
}

#[test]
fn required_pass_with_advisory_fail_is_pass() {
    let report = classify_checks_json(&json!([
        {
            "name": "Build & Test",
            "workflow": "CI",
            "bucket": "pass",
            "link": ACTIONS_URL,
            "isRequired": true,
        },
        {
            "name": "Codacy Static Code Analysis",
            "bucket": "fail",
            "link": CODACY_URL,
            "isRequired": false,
        }
    ]))
    .unwrap();
    assert_eq!(
        (
            report.job_ci_class(),
            report.collaboration_status().advisory_finding_count,
        ),
        (CiClass::Pass, 1)
    );
}

#[test]
fn empty_report_ci_class_is_unknown() {
    assert_eq!(
        ClassificationReport::default().job_ci_class(),
        CiClass::Unknown
    );
}
