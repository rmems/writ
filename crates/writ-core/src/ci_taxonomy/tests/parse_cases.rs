use serde_json::json;

use super::super::*;
use super::{ACTIONS_URL, CODACY_URL, CODERABBIT_URL};

#[test]
fn gh_bucket_aliases_normalize() {
    let cases = [
        (json!({"bucket": "pass"}), CheckConclusion::Success),
        (json!({"bucket": "fail"}), CheckConclusion::Failure),
        (json!({"bucket": "skipping"}), CheckConclusion::Skipped),
        (json!({"bucket": "cancel"}), CheckConclusion::Cancelled),
        (json!({"state": "ERROR"}), CheckConclusion::Failure),
        (
            json!({"conclusion": "ACTION_REQUIRED"}),
            CheckConclusion::ActionRequired,
        ),
        (json!({"state": "EXPECTED"}), CheckConclusion::Pending),
        (json!({"bucket": "weird"}), CheckConclusion::Pending),
    ];
    for (raw, expected) in cases {
        assert_eq!(parse_check_entry(&raw).conclusion, expected);
    }
}

#[test]
fn parse_extracts_run_id_and_legacy_fields() {
    let entry = parse_check_entry(&json!({
        "name": "CI / test",
        "workflowName": "CI",
        "conclusion": "failure",
        "detailsUrl": ACTIONS_URL,
    }));
    assert_eq!(
        (entry.workflow_name.as_str(), entry.run_id, entry.conclusion,),
        ("CI", Some(12345), CheckConclusion::Failure)
    );
}

#[test]
fn parse_missing_fields_default_to_pending() {
    let entry = parse_check_entry(&json!({}));
    assert_eq!(
        (
            entry.name.is_empty(),
            entry.conclusion,
            entry.run_id,
            entry.requirement,
        ),
        (true, CheckConclusion::Pending, None, Requirement::Unknown,)
    );
}

#[test]
fn requirement_string_tokens_are_case_insensitive() {
    let required = parse_check_entry(&json!({"required": " Required "}));
    let advisory = parse_check_entry(&json!({"is_required": "OPTIONAL"}));
    let unknown = parse_check_entry(&json!({"required": "maybe"}));
    assert_eq!(
        (
            required.requirement,
            advisory.requirement,
            unknown.requirement,
        ),
        (
            Requirement::Required,
            Requirement::Advisory,
            Requirement::Unknown,
        )
    );
}

#[test]
fn requiredness_comes_from_payload_not_provider_name() {
    let absent = parse_check_entry(&json!({
        "name": "Codacy Static Code Analysis",
        "bucket": "fail",
        "link": CODACY_URL,
    }));
    let required = parse_check_entry(&json!({
        "name": "CodeRabbit",
        "bucket": "fail",
        "link": CODERABBIT_URL,
        "isRequired": true,
    }));
    let advisory = parse_check_entry(&json!({
        "name": "Build & Test",
        "workflow": "CI",
        "bucket": "fail",
        "link": ACTIONS_URL,
        "required": false,
    }));
    assert_eq!(
        (
            absent.requirement,
            absent.requirement.as_str(),
            required.requirement,
            advisory.requirement,
        ),
        (
            Requirement::Unknown,
            "unknown",
            Requirement::Required,
            Requirement::Advisory,
        )
    );
}

#[test]
fn rollup_database_id_fills_in_missing_run_url() {
    let raw = json!({
        "__typename": "CheckRun",
        "name": "CI",
        "conclusion": "FAILURE",
        "detailsUrl": "https://github.com/acme/example-org/actions",
        "checkSuite": { "workflowRun": { "databaseId": 77, "workflow": { "name": "CI" } } }
    });
    let entry = parse_check_entry(&raw);
    let run_id = entry.run_id;
    let workflow_name = entry.workflow_name.clone();
    let check_class = classify_check(entry).check_class;
    assert_eq!(
        (run_id, workflow_name.as_str(), check_class),
        (Some(77), "CI", CheckClass::A)
    );
}

#[test]
fn extract_nodes_from_array_and_checks_wrapper() {
    let array = json!([{"name": "CI", "bucket": "pass", "workflow": "CI"}]);
    let wrapped = json!({"checks": [{"name": "CI", "bucket": "pass"}]});
    assert_eq!(
        (
            extract_check_nodes(&array).unwrap().len(),
            extract_check_nodes(&wrapped).unwrap().len(),
            extract_check_nodes(&json!({"nope": true})).is_err(),
        ),
        (1, 1, true)
    );
}

#[test]
fn status_check_rollup_checkrun_and_status_context() {
    let rollup = json!({
        "statusCheckRollup": {
            "contexts": {
                "nodes": [
                    {
                        "__typename": "CheckRun",
                        "name": "Validate, Test & Doc",
                        "conclusion": "FAILURE",
                        "status": "COMPLETED",
                        "detailsUrl": ACTIONS_URL,
                        "checkSuite": {
                            "workflowRun": {
                                "databaseId": 99,
                                "workflow": { "name": "CI" }
                            }
                        }
                    },
                    {
                        "__typename": "StatusContext",
                        "context": "CodeRabbit",
                        "state": "PENDING",
                        "targetUrl": CODERABBIT_URL
                    }
                ]
            }
        }
    });
    let report = classify_checks_json(&rollup).unwrap();
    assert_eq!(
        (
            report.checks.len(),
            report.checks[0].check_class,
            report.checks[0].entry.run_id,
            report.checks[1].check_class,
            report.checks[1].residual_code.as_deref(),
        ),
        (
            2,
            CheckClass::A,
            Some(12345),
            CheckClass::C,
            Some("class_c:coderabbit_pending"),
        )
    );
}
