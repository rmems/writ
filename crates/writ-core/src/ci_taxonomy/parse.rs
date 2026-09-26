//! Parse `gh pr checks` rows and GraphQL `statusCheckRollup` nodes.

use serde_json::Value;

use super::probe::{Fragments, Probe};
use super::{CheckConclusion, CheckEntry, Requirement};

/// Keys looked up in order. The first non-empty value wins.
struct FieldKeys(&'static [&'static str]);

/// Parse one raw check object. Unknown conclusions become [`CheckConclusion::Pending`].
#[must_use]
pub fn parse_check_entry(raw: &Value) -> CheckEntry {
    let name = first_string(raw, &FieldKeys(&["name", "context"]));
    let workflow_name = workflow_name(raw);
    let description = first_string(raw, &FieldKeys(&["description", "title"]));
    let details_url = first_string(
        raw,
        &FieldKeys(&["link", "detailsUrl", "details_url", "targetUrl"]),
    );
    let typename = optional_string(raw, &FieldKeys(&["__typename", "typename"]));
    let conclusion = normalize_conclusion(&Probe::lowercased(
        first_string(raw, &FieldKeys(&["conclusion", "bucket", "state"])).trim(),
    ));
    let status = status_text(raw, conclusion);
    let run_id = Probe::lowercased(&details_url)
        .actions_run_id()
        .or_else(|| rollup_run_id(raw));
    let requirement = parse_requirement(raw);

    CheckEntry {
        name,
        workflow_name,
        conclusion,
        status,
        description,
        details_url,
        run_id,
        requirement,
        typename,
    }
}

/// Pull check objects out of `gh pr checks` arrays and GraphQL rollup shapes.
pub fn extract_check_nodes(value: &Value) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(_) => extract_object_nodes(value),
        _ => Err("CI classify input must be a JSON array or object".to_owned()),
    }
}

fn extract_object_nodes(value: &Value) -> Result<Vec<Value>, String> {
    if let Some(Value::Array(items)) = value.get("checks") {
        return Ok(items.clone());
    }
    if let Some(nodes) = nested_nodes(
        value,
        &FieldKeys(&["statusCheckRollup", "contexts", "nodes"]),
    ) {
        return Ok(nodes);
    }
    if let Some(nodes) = nested_nodes(value, &FieldKeys(&["contexts", "nodes"])) {
        return Ok(nodes);
    }
    if let Some(Value::Array(items)) = value.get("nodes") {
        return Ok(items.clone());
    }
    if let Some(data) = value.get("data") {
        return extract_check_nodes(data);
    }
    Err(
        "expected a JSON array of checks, `{checks:[…]}`, or a statusCheckRollup node list"
            .to_owned(),
    )
}

fn status_text(raw: &Value, conclusion: CheckConclusion) -> String {
    let status = first_string(raw, &FieldKeys(&["status"]));
    if !status.is_empty() {
        return status;
    }
    if conclusion == CheckConclusion::Pending {
        return "pending".to_owned();
    }
    "completed".to_owned()
}

fn parse_requirement(raw: &Value) -> Requirement {
    for key in ["isRequired", "is_required", "required"] {
        if let Some(requirement) = requirement_from_value(raw.get(key)) {
            return requirement;
        }
    }
    Requirement::Unknown
}

fn requirement_from_value(value: Option<&Value>) -> Option<Requirement> {
    match value {
        Some(Value::Bool(true)) => Some(Requirement::Required),
        Some(Value::Bool(false)) => Some(Requirement::Advisory),
        Some(Value::String(text)) => requirement_from_token(&Probe::lowercased(text.trim())),
        _ => None,
    }
}

fn requirement_from_token(token: &Probe) -> Option<Requirement> {
    if token.equals_any(&Fragments(&["true", "required"])) {
        return Some(Requirement::Required);
    }
    if token.equals_any(&Fragments(&["false", "advisory", "optional"])) {
        return Some(Requirement::Advisory);
    }
    None
}

fn normalize_conclusion(value: &Probe) -> CheckConclusion {
    match value.text() {
        "pass" | "success" => CheckConclusion::Success,
        "fail" | "failure" | "error" => CheckConclusion::Failure,
        "pending" | "queued" | "in_progress" | "expected" => CheckConclusion::Pending,
        "skipping" | "skipped" => CheckConclusion::Skipped,
        "cancel" | "cancelled" | "canceled" => CheckConclusion::Cancelled,
        "neutral" => CheckConclusion::Neutral,
        "timed_out" | "timedout" => CheckConclusion::TimedOut,
        "action_required" => CheckConclusion::ActionRequired,
        "stale" => CheckConclusion::Stale,
        "startup_failure" => CheckConclusion::StartupFailure,
        _ => CheckConclusion::Pending,
    }
}

fn rollup_run_id(raw: &Value) -> Option<u64> {
    raw.pointer("/checkSuite/workflowRun/databaseId")
        .and_then(Value::as_u64)
        .or_else(|| {
            raw.pointer("/checkSuite/workflowRun/id")
                .and_then(Value::as_u64)
        })
}

fn workflow_name(raw: &Value) -> String {
    let direct = first_string(raw, &FieldKeys(&["workflow", "workflowName"]));
    if !direct.is_empty() {
        return direct;
    }
    first_string_at(
        raw,
        &FieldKeys(&["/checkSuite/workflowRun/workflow/name", "/workflow/name"]),
    )
}

fn first_string(raw: &Value, keys: &FieldKeys) -> String {
    for key in keys.0 {
        match raw.get(*key) {
            Some(Value::String(s)) if !s.is_empty() => return s.clone(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

fn optional_string(raw: &Value, keys: &FieldKeys) -> Option<String> {
    let value = first_string(raw, keys);
    if value.is_empty() { None } else { Some(value) }
}

fn first_string_at(raw: &Value, pointers: &FieldKeys) -> String {
    for pointer in pointers.0 {
        if let Some(Value::String(s)) = raw.pointer(pointer)
            && !s.is_empty()
        {
            return s.clone();
        }
    }
    String::new()
}

fn nested_nodes(value: &Value, path: &FieldKeys) -> Option<Vec<Value>> {
    let mut cur = value;
    for key in path.0 {
        cur = cur.get(*key)?;
    }
    cur.as_array().cloned()
}
