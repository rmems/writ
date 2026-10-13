use serde_json::{Value, json};

use super::*;

const ACTIONS_URL: &str = "https://github.com/acme/example-org/actions/runs/12345";
const CODACY_URL: &str = "https://app.codacy.com/gh/acme/example-org/pull-requests/12";
const KILO_URL: &str = "https://app.kilo.ai/review/1";
const CODERABBIT_URL: &str = "https://coderabbit.ai/review/1";
const GITAR_URL: &str = "https://gitar.ai/r/1";

struct Sample<'a> {
    name: &'a str,
    workflow: &'a str,
    bucket: &'a str,
    link: &'a str,
}

fn raw_check(sample: &Sample<'_>) -> Value {
    json!({
        "name": sample.name,
        "workflow": sample.workflow,
        "bucket": sample.bucket,
        "state": sample.bucket,
        "link": sample.link,
    })
}

fn classify_named(sample: &Sample<'_>) -> ClassifiedCheck {
    classify_check(parse_check_entry(&raw_check(sample)))
}

macro_rules! sample {
    ($name:expr, $workflow:expr, $bucket:expr, $link:expr) => {
        crate::ci_taxonomy::tests::Sample {
            name: $name,
            workflow: $workflow,
            bucket: $bucket,
            link: $link,
        }
    };
}

/// Class, recommended action, and residual code in one equality check.
fn assert_outcome(
    classified: &ClassifiedCheck,
    expected_class: CheckClass,
    expected_action: &RecommendedAction,
    expected_residual: Option<&str>,
) {
    assert_eq!(
        (
            classified.check_class,
            &classified.recommended_action,
            classified.residual_code.as_deref(),
        ),
        (expected_class, expected_action, expected_residual)
    );
}

/// Every policy in `present` is set, and every policy in `absent` is not.
fn assert_policies(classified: &ClassifiedCheck, present: &[Policy], absent: &[Policy]) {
    let missing = present
        .iter()
        .filter(|policy| !classified.policies.contains(policy))
        .count();
    let unexpected = absent
        .iter()
        .filter(|policy| classified.policies.contains(policy))
        .count();
    assert_eq!((missing, unexpected), (0, 0));
}

mod class_cases;
mod parse_cases;
mod report_cases;
