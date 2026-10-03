use serde_json::json;

use super::super::*;
use super::{
    ACTIONS_URL, CODACY_URL, CODERABBIT_URL, GITAR_URL, KILO_URL, assert_outcome, assert_policies,
    classify_named,
};

#[test]
fn class_a_failure_is_source_fix_without_rerun() {
    let ci = classify_named(&sample!("Build & Test", "CI", "fail", ACTIONS_URL));
    assert_outcome(&ci, CheckClass::A, &RecommendedAction::FixSource, None);
    assert_policies(
        &ci,
        &[Policy::FixSource, Policy::ForbidEmptyCommit],
        &[Policy::Rerun],
    );
}

#[test]
fn class_a_azure_build_is_first_party() {
    let azure = classify_named(&sample!(
        "Limen-Neural.neuromod (BuildTest linux)",
        "",
        "fail",
        "https://dev.azure.com/acme/neuromod/_build/results?buildId=9"
    ));
    assert_eq!(
        (
            azure.check_class,
            azure.policies.contains(&Policy::FixSource)
        ),
        (CheckClass::A, true)
    );
}

#[test]
fn class_a_named_workflow_checks_stay_first_party() {
    let rustsec = classify_named(&sample!("rustsec", "Audit", "fail", ACTIONS_URL));
    let codecov = classify_named(&sample!("Codecov", "Coverage", "fail", ACTIONS_URL));
    let bare = classify_named(&sample!("cargo audit", "Security", "fail", ""));
    assert_eq!(
        (rustsec.check_class, codecov.check_class, bare.check_class,),
        (CheckClass::A, CheckClass::A, CheckClass::A)
    );
}

#[test]
fn class_b_codacy_failure_is_source_fix() {
    let fail = classify_named(&sample!(
        "Codacy Static Code Analysis",
        "",
        "fail",
        CODACY_URL
    ));
    assert_outcome(&fail, CheckClass::B, &RecommendedAction::FixSource, None);
    assert_policies(
        &fail,
        &[Policy::FixSource, Policy::ForbidEmptyCommit],
        &[Policy::Rerun],
    );
}

#[test]
fn class_b_action_required_is_residual() {
    let gate = classify_check(parse_check_entry(&serde_json::json!({
        "name": "Codacy Static Code Analysis",
        "link": CODACY_URL,
        "conclusion": "ACTION_REQUIRED",
        "__typename": "CheckRun",
    })));
    assert_outcome(
        &gate,
        CheckClass::B,
        &RecommendedAction::Residual {
            code: "class_b:codacy_action_required".to_owned(),
        },
        Some("class_b:codacy_action_required"),
    );
    assert_policies(
        &gate,
        &[Policy::MarkResidual, Policy::ForbidEmptyCommit],
        &[Policy::FixSource, Policy::ReplyWithSha],
    );
    assert!(!should_rerun(&gate));
}

#[test]
fn class_c_review_bots_never_empty_commit_or_rerun() {
    for (name, link, vendor) in [
        ("Kilo Code Review", KILO_URL, "kilo"),
        ("CodeRabbit", CODERABBIT_URL, "coderabbit"),
        ("Gitar", GITAR_URL, "gitar"),
    ] {
        let pending = classify_named(&sample!(name, "", "pending", link));
        let pending_residual = format!("class_c:{vendor}_pending");
        assert_outcome(
            &pending,
            CheckClass::C,
            &RecommendedAction::Wait,
            Some(pending_residual.as_str()),
        );
        assert!(pending.policies.is_empty());
        assert!(!should_rerun(&pending));

        let fail = classify_named(&sample!(name, "", "fail", link));
        let fail_residual = format!("class_c:{vendor}_fail");
        assert_outcome(
            &fail,
            CheckClass::C,
            &RecommendedAction::Residual {
                code: fail_residual.clone(),
            },
            Some(fail_residual.as_str()),
        );
        assert_policies(
            &fail,
            &[
                Policy::ReportOnly,
                Policy::MarkResidual,
                Policy::ForbidEmptyCommit,
            ],
            &[Policy::FixSource, Policy::Rerun],
        );
    }
}

#[test]
fn unknown_third_party_without_workflow_is_class_c() {
    let classified = classify_named(&sample!(
        "ci/circleci",
        "",
        "fail",
        "https://circleci.com/gh/acme/example-org/123"
    ));
    assert_eq!(classified.check_class, CheckClass::C);
    assert!(classified.policies.contains(&Policy::ReportOnly));
    assert!(!classified.policies.contains(&Policy::FixSource));
}

#[test]
fn class_a_timeout_prefers_official_rerun() {
    let classified = classify_named(&sample!("Build & Test", "CI", "timed_out", ACTIONS_URL));
    assert_outcome(
        &classified,
        CheckClass::A,
        &RecommendedAction::Rerun { run_id: 12345 },
        None,
    );
    assert_policies(&classified, &[Policy::ForbidEmptyCommit], &[]);
    assert_eq!(
        (should_rerun(&classified), rerun_command(&classified)),
        (
            true,
            Some(vec![
                "run".to_owned(),
                "rerun".to_owned(),
                "12345".to_owned()
            ])
        )
    );
}

#[test]
fn class_a_cancelled_without_run_id_does_not_rerun() {
    let classified = classify_named(&sample!("Build & Test", "CI", "cancel", ""));
    assert_eq!(
        (should_rerun(&classified), classified.recommended_action,),
        (false, RecommendedAction::FixSource)
    );
}

#[test]
fn class_b_does_not_rerun() {
    let classified = classify_named(&sample!("Codacy Static Code Analysis", "", "timed_out", ""));
    assert_eq!(
        (
            should_rerun(&classified),
            rerun_command(&classified).is_none(),
        ),
        (false, true)
    );
}

#[test]
fn codacy_name_overrides_actions_workflow() {
    let classified = classify_named(&sample!("Codacy CI Analysis", "CI", "fail", ACTIONS_URL));
    assert_eq!(classified.check_class, CheckClass::B);
}

#[test]
fn kilo_overrides_nonempty_workflow() {
    let classified = classify_named(&sample!("Kilo Code Review", "Kilo", "fail", ""));
    assert_eq!(
        (
            classified.check_class,
            classified.policies.contains(&Policy::Rerun),
        ),
        (CheckClass::C, false)
    );
}

#[test]
fn vendor_word_in_actions_url_path_stays_class_a() {
    // Owner/repo path contains a vendor word, but it is a GitHub Actions URL.
    let kilo_path = classify_named(&sample!(
        "build",
        "CI",
        "fail",
        "https://github.com/kilo/app/actions/runs/5"
    ));
    let codacy_org = classify_named(&sample!(
        "build",
        "CI",
        "fail",
        "https://github.com/codacy-tools/app/actions/runs/9"
    ));
    assert_eq!(
        (kilo_path.check_class, codacy_org.check_class),
        (CheckClass::A, CheckClass::A)
    );
}

#[test]
fn genuine_vendor_checks_still_classify_b_and_c() {
    // Genuine Kilo by name and by host-specific URL.
    let kilo_name = classify_named(&sample!("Kilo Code Review", "", "fail", ""));
    let kilo_url = classify_named(&sample!("review", "", "fail", KILO_URL));
    // Genuine Codacy by name and by host-specific URL.
    let codacy_name = classify_named(&sample!("Codacy Static Code Analysis", "", "fail", ""));
    let codacy_url = classify_named(&sample!("quality", "", "fail", CODACY_URL));
    assert_eq!(
        (
            kilo_name.check_class,
            kilo_url.check_class,
            codacy_name.check_class,
            codacy_url.check_class,
        ),
        (CheckClass::C, CheckClass::C, CheckClass::B, CheckClass::B)
    );
}

#[test]
fn observation_code_does_not_end_in_separator_after_slug_truncation() {
    let report = classify_checks_json(&json!([{
        "name": format!("{} suffix", "a".repeat(39)),
        "workflow": "CI",
        "bucket": "fail",
        "link": ACTIONS_URL,
    }]))
    .unwrap();

    assert_eq!(
        report.observation_codes(ObservationKind::UnknownRequiredness),
        vec!["unknown_requiredness:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
    );
}
