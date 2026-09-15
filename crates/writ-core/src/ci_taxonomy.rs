//! CI check taxonomy for companion-skill PR monitoring (Linear RM-125 / GitHub #10).
//!
//! Classifies each GitHub status check so babysit workers **fix what is
//! fixable**, **rerun flaky Actions when appropriate**, and **never spam empty
//! commits** to kick third-party review bots.
//!
//! Inputs are `gh pr checks --json name,state,bucket,workflow,link` rows and
//! optional GraphQL `statusCheckRollup` nodes (`CheckRun` / `StatusContext`).
//! This module is policy, not a babysit loop: it does not merge, push, or
//! invoke `gh`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Check ownership / fixability class.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum CheckClass {
    /// First-party GitHub Actions or Azure build/test. Fix source; official rerun on flake.
    A,
    /// Codacy (and similar quality gates). Fix real findings; never empty-push.
    B,
    /// Third-party review status (Kilo, CodeRabbit, Gitar, …). Report residual only.
    C,
}

impl CheckClass {
    /// Stable class letter used in residual blocker codes (`class_a:…`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
        }
    }
}

/// Normalized check conclusion, covering `gh` buckets and GraphQL conclusions.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    TimedOut,
    ActionRequired,
    Skipped,
    Stale,
    Pending,
    StartupFailure,
}

impl CheckConclusion {
    /// `gh pr checks` bucket `pass` / GraphQL `SUCCESS`.
    #[must_use]
    pub const fn is_terminal_passing(self) -> bool {
        matches!(self, Self::Success | Self::Skipped | Self::Neutral)
    }

    /// Outcomes that still need a decision (fix, rerun, residual, or wait).
    #[must_use]
    pub const fn is_blocking_failure(self) -> bool {
        matches!(
            self,
            Self::Failure
                | Self::TimedOut
                | Self::StartupFailure
                | Self::ActionRequired
                | Self::Cancelled
                | Self::Stale
        )
    }
}

/// Allowed / forbidden agent actions for one classified check.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Official `gh run rerun` is allowed (Class A flake with a run id).
    Rerun,
    /// Diagnose logs and edit source in the assigned worktree.
    FixSource,
    /// After a real fix push, reply with SHA and agent attribution.
    ReplyWithSha,
    /// Record a residual blocker for watchlist / final report.
    MarkResidual,
    /// Do not attempt a code fix for this check itself.
    ReportOnly,
    /// Empty “chore: retrigger CI” commits are forbidden.
    ForbidEmptyCommit,
    /// Do not ignore a failing Class A/B check while continuing the cycle.
    ForbidIgnore,
}

/// What the agent should do next for this check.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecommendedAction {
    /// Pass / skip / cancel-as-success: no action.
    Ignore,
    /// Still running: continue other work; do not spam reruns.
    Wait,
    /// Fixable first-party or quality-gate failure.
    FixSource,
    /// Transient Actions failure: official rerun once.
    Rerun { run_id: u64 },
    /// Human or third-party gate the agent cannot close.
    Residual { code: String },
}

/// One parsed check from `gh pr checks` or `statusCheckRollup`.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckEntry {
    pub name: String,
    pub workflow_name: String,
    pub conclusion: CheckConclusion,
    pub status: String,
    pub description: String,
    pub details_url: String,
    pub run_id: Option<u64>,
    /// GraphQL `__typename` when present (`CheckRun` / `StatusContext`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typename: Option<String>,
}

/// A check plus class, policies, residual code, and recommended action.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClassifiedCheck {
    pub entry: CheckEntry,
    pub check_class: CheckClass,
    pub policies: BTreeSet<Policy>,
    pub reason: String,
    /// Structured residual for watchlist (#12) / final report (#16). `None` when not residual.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub residual_code: Option<String>,
    pub recommended_action: RecommendedAction,
}

/// Classification of a full PR check rollup.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClassificationReport {
    pub checks: Vec<ClassifiedCheck>,
}

impl ClassificationReport {
    /// Class A checks (any conclusion).
    #[must_use]
    pub fn class_a(&self) -> Vec<&ClassifiedCheck> {
        self.checks
            .iter()
            .filter(|c| c.check_class == CheckClass::A)
            .collect()
    }

    /// Class B checks (any conclusion).
    #[must_use]
    pub fn class_b(&self) -> Vec<&ClassifiedCheck> {
        self.checks
            .iter()
            .filter(|c| c.check_class == CheckClass::B)
            .collect()
    }

    /// Class C checks (any conclusion).
    #[must_use]
    pub fn class_c(&self) -> Vec<&ClassifiedCheck> {
        self.checks
            .iter()
            .filter(|c| c.check_class == CheckClass::C)
            .collect()
    }

    /// Failed / action-required / cancelled / stale / timed-out checks.
    #[must_use]
    pub fn failures(&self) -> Vec<&ClassifiedCheck> {
        self.checks
            .iter()
            .filter(|c| c.entry.conclusion.is_blocking_failure())
            .collect()
    }

    /// Failures the agent may fix in source (Class A/B).
    #[must_use]
    pub fn fixable_failures(&self) -> Vec<&ClassifiedCheck> {
        self.failures()
            .into_iter()
            .filter(|c| matches!(c.check_class, CheckClass::A | CheckClass::B))
            .collect()
    }

    /// Failures that remain residual (Class C, or Class B human gates).
    #[must_use]
    pub fn residual_failures(&self) -> Vec<&ClassifiedCheck> {
        self.failures()
            .into_iter()
            .filter(|c| c.residual_code.is_some())
            .collect()
    }

    /// Distinct residual blocker codes suitable for watchlist / report notes.
    #[must_use]
    pub fn residual_codes(&self) -> Vec<String> {
        let mut codes: Vec<String> = self
            .checks
            .iter()
            .filter_map(|c| c.residual_code.clone())
            .collect();
        codes.sort();
        codes.dedup();
        codes
    }

    /// True when there is at least one check and every check is terminal-passing.
    ///
    /// `skipping` / `SKIPPED` counts as passing (non-blocking). Empty input is
    /// unknown, not success. Pending checks mean the rollup is not done.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.checks.is_empty()
            && self
                .checks
                .iter()
                .all(|c| c.entry.conclusion.is_terminal_passing())
    }
}

/// Parse one raw check object. Unknown conclusions become [`CheckConclusion::Pending`].
#[must_use]
pub fn parse_check_entry(raw: &Value) -> CheckEntry {
    let name = first_string(raw, &["name", "context"]);
    let workflow_name = workflow_name(raw);
    let description = first_string(raw, &["description", "title"]);
    let details_url = first_string(raw, &["link", "detailsUrl", "details_url", "targetUrl"]);
    let typename = optional_string(raw, &["__typename", "typename"]);
    let conclusion =
        normalize_conclusion(first_string(raw, &["conclusion", "bucket", "state"]).as_str());
    let status = {
        let s = first_string(raw, &["status"]);
        if !s.is_empty() {
            s
        } else if conclusion == CheckConclusion::Pending {
            "pending".to_owned()
        } else {
            "completed".to_owned()
        }
    };
    let run_id = extract_run_id(&details_url).or_else(|| rollup_run_id(raw));

    CheckEntry {
        name,
        workflow_name,
        conclusion,
        status,
        description,
        details_url,
        run_id,
        typename,
    }
}

/// Classify one parsed check.
#[must_use]
pub fn classify_check(entry: CheckEntry) -> ClassifiedCheck {
    let check_class = classify_class(&entry);
    let policies = policies_for(check_class, entry.conclusion, entry.run_id);
    let residual_code = residual_code(check_class, &entry);
    let recommended_action =
        recommended_action(check_class, &entry, &policies, residual_code.as_deref());
    let reason = explain(check_class, &entry);
    ClassifiedCheck {
        entry,
        check_class,
        policies,
        reason,
        residual_code,
        recommended_action,
    }
}

/// Classify a JSON array of checks, or a rollup wrapper (see [`extract_check_nodes`]).
pub fn classify_checks_json(value: &Value) -> Result<ClassificationReport, String> {
    let nodes = extract_check_nodes(value)?;
    Ok(classify_checks(&nodes))
}

/// Parse UTF-8 JSON bytes and classify.
pub fn classify_from_slice(bytes: &[u8]) -> Result<ClassificationReport, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|err| format!("invalid JSON: {err}"))?;
    classify_checks_json(&value)
}

/// Envelope `data` payload for `writ --json ci classify`.
#[must_use]
pub fn classify_response_data(report: &ClassificationReport) -> Value {
    serde_json::json!({
        "checks": report.checks,
        "all_passed": report.all_passed(),
        "residual_codes": report.residual_codes(),
        "fixable_failure_count": report.fixable_failures().len(),
    })
}

/// Classify already-extracted check objects.
#[must_use]
pub fn classify_checks(raw_checks: &[Value]) -> ClassificationReport {
    ClassificationReport {
        checks: raw_checks
            .iter()
            .map(|raw| classify_check(parse_check_entry(raw)))
            .collect(),
    }
}

/// Pull check objects out of `gh pr checks` arrays and GraphQL rollup shapes.
pub fn extract_check_nodes(value: &Value) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(_) => {
            if let Some(Value::Array(items)) = value.get("checks") {
                return Ok(items.clone());
            }
            if let Some(nodes) = nested_nodes(value, &["statusCheckRollup", "contexts", "nodes"]) {
                return Ok(nodes);
            }
            if let Some(nodes) = nested_nodes(value, &["contexts", "nodes"]) {
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
        _ => Err("CI classify input must be a JSON array or object".to_owned()),
    }
}

/// Official rerun is allowed only for Class A flakes with a GitHub Actions run id.
#[must_use]
pub fn should_rerun(classified: &ClassifiedCheck) -> bool {
    classified.check_class == CheckClass::A
        && classified.policies.contains(&Policy::Rerun)
        && classified.entry.run_id.is_some()
        && matches!(
            classified.entry.conclusion,
            CheckConclusion::TimedOut
                | CheckConclusion::StartupFailure
                | CheckConclusion::Cancelled
        )
}

/// `gh` / `writ gh-safe` argv for an official rerun (`run rerun <id>`).
#[must_use]
pub fn rerun_command(classified: &ClassifiedCheck) -> Option<Vec<String>> {
    if !should_rerun(classified) {
        return None;
    }
    let run_id = classified.entry.run_id?;
    Some(vec![
        "run".to_owned(),
        "rerun".to_owned(),
        run_id.to_string(),
    ])
}

fn classify_class(entry: &CheckEntry) -> CheckClass {
    let text = combined_text(entry);
    if is_class_b(&text, &entry.details_url) {
        return CheckClass::B;
    }
    if is_class_c(&text, &entry.details_url) {
        return CheckClass::C;
    }
    if is_github_actions_url(&entry.details_url)
        || is_azure_build_url(&entry.details_url)
        || !entry.workflow_name.trim().is_empty()
    {
        return CheckClass::A;
    }
    CheckClass::C
}

fn is_class_b(text: &str, url: &str) -> bool {
    contains_any(
        text,
        &["codacy", "code climate", "codeclimate", "deepsource"],
    ) || contains_word(text, "sonarcloud")
        || contains_word(text, "sonarqube")
        || contains_word(text, "sonar")
        || contains_any(url, &["app.codacy.com", "codacy.com"])
}

fn is_class_c(text: &str, url: &str) -> bool {
    contains_word(text, "kilo")
        || contains_any(text, &["coderabbit", "code rabbit"])
        || contains_word(text, "gitar")
        || contains_any(text, &["code review"])
        || contains_word(text, "dependabot")
        || contains_word(text, "renovate")
        || (contains_word(text, "copilot") && contains_word(text, "review"))
        || contains_any(
            url,
            &["kilo.ai", "app.kilo.ai", "coderabbit.ai", "gitar.ai"],
        )
}

fn is_github_actions_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/actions/runs/") && lower.contains("github.com")
}

fn is_azure_build_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("dev.azure.com") || lower.contains("visualstudio.com")
}

fn policies_for(
    check_class: CheckClass,
    conclusion: CheckConclusion,
    run_id: Option<u64>,
) -> BTreeSet<Policy> {
    if conclusion.is_terminal_passing() || conclusion == CheckConclusion::Pending {
        return BTreeSet::new();
    }

    let mut policies = BTreeSet::from([Policy::ForbidEmptyCommit]);
    match check_class {
        CheckClass::A => {
            policies.insert(Policy::ForbidIgnore);
            policies.insert(Policy::FixSource);
            policies.insert(Policy::ReplyWithSha);
            if run_id.is_some()
                && matches!(
                    conclusion,
                    CheckConclusion::TimedOut
                        | CheckConclusion::StartupFailure
                        | CheckConclusion::Cancelled
                )
            {
                policies.insert(Policy::Rerun);
            }
        }
        CheckClass::B => {
            policies.insert(Policy::ForbidIgnore);
            policies.insert(Policy::FixSource);
            policies.insert(Policy::ReplyWithSha);
            if conclusion == CheckConclusion::ActionRequired {
                policies.insert(Policy::MarkResidual);
            }
        }
        CheckClass::C => {
            policies.insert(Policy::ReportOnly);
            policies.insert(Policy::MarkResidual);
        }
    }
    policies
}

fn residual_code(check_class: CheckClass, entry: &CheckEntry) -> Option<String> {
    let vendor = vendor_slug(check_class, entry);
    match entry.conclusion {
        CheckConclusion::Success | CheckConclusion::Skipped | CheckConclusion::Neutral => None,
        CheckConclusion::Pending => match check_class {
            CheckClass::C => Some(format!("class_c:{vendor}_pending")),
            CheckClass::A | CheckClass::B => None,
        },
        CheckConclusion::ActionRequired => match check_class {
            CheckClass::B => Some(format!("class_b:{vendor}_action_required")),
            CheckClass::C => Some(format!("class_c:{vendor}_action_required")),
            CheckClass::A => Some(format!("class_a:{vendor}_action_required")),
        },
        _ => match check_class {
            CheckClass::C => Some(format!(
                "class_c:{vendor}_{}",
                conclusion_slug(entry.conclusion)
            )),
            CheckClass::B if entry.conclusion == CheckConclusion::Failure => None,
            CheckClass::B => Some(format!(
                "class_b:{vendor}_{}",
                conclusion_slug(entry.conclusion)
            )),
            CheckClass::A => None,
        },
    }
}

fn recommended_action(
    check_class: CheckClass,
    entry: &CheckEntry,
    policies: &BTreeSet<Policy>,
    residual: Option<&str>,
) -> RecommendedAction {
    if entry.conclusion.is_terminal_passing() {
        return RecommendedAction::Ignore;
    }
    if entry.conclusion == CheckConclusion::Pending {
        return RecommendedAction::Wait;
    }
    if check_class == CheckClass::A
        && policies.contains(&Policy::Rerun)
        && let Some(run_id) = entry.run_id
    {
        return RecommendedAction::Rerun { run_id };
    }
    if policies.contains(&Policy::FixSource) && residual.is_none() {
        return RecommendedAction::FixSource;
    }
    if let Some(code) = residual {
        return RecommendedAction::Residual {
            code: code.to_owned(),
        };
    }
    if policies.contains(&Policy::FixSource) {
        return RecommendedAction::FixSource;
    }
    RecommendedAction::Ignore
}

fn explain(check_class: CheckClass, entry: &CheckEntry) -> String {
    match check_class {
        CheckClass::A => format!(
            "Class A first-party CI '{}' — fix source or `gh run rerun`; never empty-commit",
            entry.name
        ),
        CheckClass::B => format!(
            "Class B quality gate '{}' — fix real findings; residual human gate if ACTION_REQUIRED",
            entry.name
        ),
        CheckClass::C => format!(
            "Class C third-party review '{}' — report residual; never empty-commit to retrigger",
            entry.name
        ),
    }
}

fn vendor_slug(check_class: CheckClass, entry: &CheckEntry) -> String {
    let text = combined_text(entry);
    if contains_any(&text, &["codacy"]) || entry.details_url.to_ascii_lowercase().contains("codacy")
    {
        return "codacy".to_owned();
    }
    if contains_word(&text, "kilo") || entry.details_url.to_ascii_lowercase().contains("kilo") {
        return "kilo".to_owned();
    }
    if contains_any(&text, &["coderabbit", "code rabbit"])
        || entry
            .details_url
            .to_ascii_lowercase()
            .contains("coderabbit")
    {
        return "coderabbit".to_owned();
    }
    if contains_word(&text, "gitar") || entry.details_url.to_ascii_lowercase().contains("gitar") {
        return "gitar".to_owned();
    }
    match check_class {
        CheckClass::B => "quality".to_owned(),
        CheckClass::C => "third_party".to_owned(),
        CheckClass::A => slug_name(&entry.name),
    }
}

fn conclusion_slug(conclusion: CheckConclusion) -> &'static str {
    match conclusion {
        CheckConclusion::Success => "pass",
        CheckConclusion::Failure => "fail",
        CheckConclusion::Neutral => "neutral",
        CheckConclusion::Cancelled => "cancelled",
        CheckConclusion::TimedOut => "timed_out",
        CheckConclusion::ActionRequired => "action_required",
        CheckConclusion::Skipped => "skipping",
        CheckConclusion::Stale => "stale",
        CheckConclusion::Pending => "pending",
        CheckConclusion::StartupFailure => "startup_failure",
    }
}

fn slug_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "check".to_owned()
    } else {
        trimmed.chars().take(40).collect()
    }
}

fn combined_text(entry: &CheckEntry) -> String {
    format!(
        "{} {} {} {}",
        entry.name, entry.workflow_name, entry.description, entry.details_url
    )
}

fn normalize_conclusion(value: &str) -> CheckConclusion {
    match value.trim().to_ascii_lowercase().as_str() {
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

fn extract_run_id(url: &str) -> Option<u64> {
    let marker = "/actions/runs/";
    let lower = url.to_ascii_lowercase();
    let idx = lower.find(marker)?;
    let rest = &url[idx + marker.len()..];
    let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if id.is_empty() { None } else { id.parse().ok() }
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
    let direct = first_string(raw, &["workflow", "workflowName"]);
    if !direct.is_empty() {
        return direct;
    }
    first_string_at(
        raw,
        &["/checkSuite/workflowRun/workflow/name", "/workflow/name"],
    )
}

fn first_string(raw: &Value, keys: &[&str]) -> String {
    for key in keys {
        match raw.get(*key) {
            Some(Value::String(s)) if !s.is_empty() => return s.clone(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

fn optional_string(raw: &Value, keys: &[&str]) -> Option<String> {
    let s = first_string(raw, keys);
    if s.is_empty() { None } else { Some(s) }
}

fn first_string_at(raw: &Value, pointers: &[&str]) -> String {
    for pointer in pointers {
        if let Some(Value::String(s)) = raw.pointer(pointer)
            && !s.is_empty()
        {
            return s.clone();
        }
    }
    String::new()
}

fn nested_nodes(value: &Value, path: &[&str]) -> Option<Vec<Value>> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_array().cloned()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    needles.iter().any(|n| lower.contains(n))
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let needle = word.to_ascii_lowercase();
    haystack
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| part == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn actions_url() -> &'static str {
        "https://github.com/acme/example-org/actions/runs/12345"
    }

    fn raw_check(name: &str, workflow: &str, conclusion: &str, link: &str) -> Value {
        json!({
            "name": name,
            "workflow": workflow,
            "bucket": conclusion,
            "state": conclusion,
            "link": link,
        })
    }

    fn classify_named(name: &str, workflow: &str, bucket: &str, link: &str) -> ClassifiedCheck {
        classify_check(parse_check_entry(&raw_check(name, workflow, bucket, link)))
    }

    #[test]
    fn gh_bucket_aliases_normalize() {
        assert_eq!(
            parse_check_entry(&json!({"bucket": "pass"})).conclusion,
            CheckConclusion::Success
        );
        assert_eq!(
            parse_check_entry(&json!({"bucket": "fail"})).conclusion,
            CheckConclusion::Failure
        );
        assert_eq!(
            parse_check_entry(&json!({"bucket": "skipping"})).conclusion,
            CheckConclusion::Skipped
        );
        assert_eq!(
            parse_check_entry(&json!({"bucket": "cancel"})).conclusion,
            CheckConclusion::Cancelled
        );
        assert_eq!(
            parse_check_entry(&json!({"state": "ERROR"})).conclusion,
            CheckConclusion::Failure
        );
        assert_eq!(
            parse_check_entry(&json!({"conclusion": "ACTION_REQUIRED"})).conclusion,
            CheckConclusion::ActionRequired
        );
        assert_eq!(
            parse_check_entry(&json!({"state": "EXPECTED"})).conclusion,
            CheckConclusion::Pending
        );
        assert_eq!(
            parse_check_entry(&json!({"bucket": "weird"})).conclusion,
            CheckConclusion::Pending
        );
    }

    #[test]
    fn parse_extracts_run_id_and_legacy_fields() {
        let entry = parse_check_entry(&json!({
            "name": "CI / test",
            "workflowName": "CI",
            "conclusion": "failure",
            "detailsUrl": actions_url(),
        }));
        assert_eq!(entry.workflow_name, "CI");
        assert_eq!(entry.run_id, Some(12345));
        assert_eq!(entry.conclusion, CheckConclusion::Failure);
    }

    #[test]
    fn parse_missing_fields_default_to_pending() {
        let entry = parse_check_entry(&json!({}));
        assert!(entry.name.is_empty());
        assert_eq!(entry.conclusion, CheckConclusion::Pending);
        assert_eq!(entry.run_id, None);
    }

    #[test]
    fn class_a_actions_and_azure() {
        let ci = classify_named("Build & Test", "CI", "fail", actions_url());
        assert_eq!(ci.check_class, CheckClass::A);
        assert!(ci.policies.contains(&Policy::FixSource));
        assert!(ci.policies.contains(&Policy::ForbidEmptyCommit));
        assert!(!ci.policies.contains(&Policy::Rerun));
        assert_eq!(ci.recommended_action, RecommendedAction::FixSource);
        assert!(ci.residual_code.is_none());

        let azure = classify_named(
            "Limen-Neural.neuromod (BuildTest linux)",
            "",
            "fail",
            "https://dev.azure.com/acme/neuromod/_build/results?buildId=9",
        );
        assert_eq!(azure.check_class, CheckClass::A);
        assert!(azure.policies.contains(&Policy::FixSource));

        let rustsec = classify_named("rustsec", "Audit", "fail", actions_url());
        assert_eq!(rustsec.check_class, CheckClass::A);

        let codecov_action = classify_named("Codecov", "Coverage", "fail", actions_url());
        assert_eq!(codecov_action.check_class, CheckClass::A);
    }

    #[test]
    fn class_a_nonempty_workflow_without_url_is_actions() {
        let classified = classify_named("cargo audit", "Security", "fail", "");
        assert_eq!(classified.check_class, CheckClass::A);
    }

    #[test]
    fn class_b_codacy_and_action_required_residual() {
        let fail = classify_named(
            "Codacy Static Code Analysis",
            "",
            "fail",
            "https://app.codacy.com/gh/acme/example-org/pull-requests/12",
        );
        assert_eq!(fail.check_class, CheckClass::B);
        assert!(fail.policies.contains(&Policy::FixSource));
        assert!(!fail.policies.contains(&Policy::Rerun));
        assert!(fail.policies.contains(&Policy::ForbidEmptyCommit));
        assert_eq!(fail.recommended_action, RecommendedAction::FixSource);

        let gate = classify_check(parse_check_entry(&json!({
            "name": "Codacy Static Code Analysis",
            "link": "https://app.codacy.com/gh/acme/example-org/pull-requests/12",
            "conclusion": "ACTION_REQUIRED",
            "__typename": "CheckRun",
        })));
        assert_eq!(gate.check_class, CheckClass::B);
        assert!(gate.policies.contains(&Policy::MarkResidual));
        assert_eq!(
            gate.residual_code.as_deref(),
            Some("class_b:codacy_action_required")
        );
        assert_eq!(
            gate.recommended_action,
            RecommendedAction::Residual {
                code: "class_b:codacy_action_required".to_owned()
            }
        );
        assert!(!should_rerun(&gate));
    }

    #[test]
    fn class_c_review_bots_never_empty_commit_or_rerun() {
        for (name, link, vendor) in [
            ("Kilo Code Review", "https://app.kilo.ai/review/1", "kilo"),
            ("CodeRabbit", "https://coderabbit.ai/review/1", "coderabbit"),
            ("Gitar", "https://gitar.ai/r/1", "gitar"),
        ] {
            let pending = classify_named(name, "", "pending", link);
            assert_eq!(pending.check_class, CheckClass::C);
            assert!(pending.policies.is_empty());
            assert_eq!(pending.recommended_action, RecommendedAction::Wait);
            assert_eq!(
                pending.residual_code,
                Some(format!("class_c:{vendor}_pending"))
            );
            assert!(!should_rerun(&pending));

            let fail = classify_named(name, "", "fail", link);
            assert_eq!(fail.check_class, CheckClass::C);
            assert!(fail.policies.contains(&Policy::ReportOnly));
            assert!(fail.policies.contains(&Policy::MarkResidual));
            assert!(fail.policies.contains(&Policy::ForbidEmptyCommit));
            assert!(!fail.policies.contains(&Policy::FixSource));
            assert!(!fail.policies.contains(&Policy::Rerun));
            assert_eq!(fail.residual_code, Some(format!("class_c:{vendor}_fail")));
        }
    }

    #[test]
    fn unknown_third_party_without_workflow_is_class_c() {
        let classified = classify_named(
            "ci/circleci",
            "",
            "fail",
            "https://circleci.com/gh/acme/example-org/123",
        );
        assert_eq!(classified.check_class, CheckClass::C);
        assert!(classified.policies.contains(&Policy::ReportOnly));
        assert!(!classified.policies.contains(&Policy::FixSource));
    }

    #[test]
    fn skipping_is_non_blocking() {
        let report = classify_checks(&[raw_check(
            "Supabase Preview",
            "",
            "skipping",
            "https://supabase.com/preview",
        )]);
        assert!(report.all_passed());
        assert!(report.failures().is_empty());
        assert!(report.residual_codes().is_empty());
        assert_eq!(
            report.checks[0].recommended_action,
            RecommendedAction::Ignore
        );
    }

    #[test]
    fn pending_class_a_has_no_rerun_and_is_not_all_passed() {
        let report = classify_checks(&[raw_check("Build & Test", "CI", "pending", actions_url())]);
        assert!(!report.all_passed());
        assert!(report.checks[0].policies.is_empty());
        assert!(!should_rerun(&report.checks[0]));
        assert!(rerun_command(&report.checks[0]).is_none());
        assert_eq!(report.checks[0].recommended_action, RecommendedAction::Wait);
        assert!(report.checks[0].residual_code.is_none());
    }

    #[test]
    fn class_a_timeout_prefers_official_rerun() {
        let classified = classify_named("Build & Test", "CI", "timed_out", actions_url());
        assert!(should_rerun(&classified));
        assert_eq!(
            rerun_command(&classified),
            Some(vec![
                "run".to_owned(),
                "rerun".to_owned(),
                "12345".to_owned()
            ])
        );
        assert_eq!(
            classified.recommended_action,
            RecommendedAction::Rerun { run_id: 12345 }
        );
        assert!(classified.policies.contains(&Policy::ForbidEmptyCommit));
    }

    #[test]
    fn class_a_cancelled_without_run_id_does_not_rerun() {
        let classified = classify_named("Build & Test", "CI", "cancel", "");
        assert!(!should_rerun(&classified));
        assert_eq!(classified.recommended_action, RecommendedAction::FixSource);
    }

    #[test]
    fn class_b_does_not_rerun() {
        let classified = classify_named("Codacy Static Code Analysis", "", "timed_out", "");
        assert!(!should_rerun(&classified));
        assert!(rerun_command(&classified).is_none());
    }

    #[test]
    fn empty_rollup_is_not_success() {
        assert!(!classify_checks(&[]).all_passed());
    }

    #[test]
    fn mixed_report_separates_fixable_and_residual() {
        let report = classify_checks(&[
            raw_check("Build & Test", "CI", "fail", actions_url()),
            raw_check(
                "Codacy Static Code Analysis",
                "",
                "fail",
                "https://app.codacy.com/gh/acme/example-org/pull-requests/1",
            ),
            raw_check(
                "Kilo Code Review",
                "",
                "fail",
                "https://app.kilo.ai/review/1",
            ),
            raw_check("Supabase Preview", "", "skipping", ""),
        ]);
        assert!(!report.all_passed());
        assert_eq!(report.failures().len(), 3);
        assert_eq!(report.fixable_failures().len(), 2);
        assert_eq!(
            report.residual_codes(),
            vec!["class_c:kilo_fail".to_owned()]
        );
        assert_eq!(report.class_a().len(), 1);
        assert_eq!(report.class_b().len(), 1);
        assert_eq!(report.class_c().len(), 2);
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
                            "detailsUrl": actions_url(),
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
                            "targetUrl": "https://coderabbit.ai/r/1"
                        }
                    ]
                }
            }
        });
        let report = classify_checks_json(&rollup).unwrap();
        assert_eq!(report.checks.len(), 2);
        assert_eq!(report.checks[0].check_class, CheckClass::A);
        assert_eq!(report.checks[0].entry.run_id, Some(12345));
        assert_eq!(report.checks[1].check_class, CheckClass::C);
        assert_eq!(
            report.checks[1].residual_code.as_deref(),
            Some("class_c:coderabbit_pending")
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
        assert_eq!(entry.run_id, Some(77));
        assert_eq!(entry.workflow_name, "CI");
        assert_eq!(classify_check(entry).check_class, CheckClass::A);
    }

    #[test]
    fn codacy_name_overrides_actions_workflow() {
        let classified = classify_named("Codacy CI Analysis", "CI", "fail", actions_url());
        assert_eq!(classified.check_class, CheckClass::B);
    }

    #[test]
    fn kilo_overrides_nonempty_workflow() {
        let classified = classify_named("Kilo Code Review", "Kilo", "fail", "");
        assert_eq!(classified.check_class, CheckClass::C);
        assert!(!classified.policies.contains(&Policy::Rerun));
    }

    #[test]
    fn extract_nodes_from_array_and_checks_wrapper() {
        let array = json!([{"name": "CI", "bucket": "pass", "workflow": "CI"}]);
        assert_eq!(extract_check_nodes(&array).unwrap().len(), 1);
        let wrapped = json!({"checks": [{"name": "CI", "bucket": "pass"}]});
        assert_eq!(extract_check_nodes(&wrapped).unwrap().len(), 1);
        assert!(extract_check_nodes(&json!({"nope": true})).is_err());
    }

    #[test]
    fn stale_is_a_failure_not_success() {
        let report = classify_checks(&[raw_check("CI", "CI", "stale", actions_url())]);
        assert_eq!(report.failures().len(), 1);
        assert!(!report.all_passed());
    }
}
