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

use crate::status::CiClass;

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

/// Whether GitHub (or the check payload) marked this check as required.
///
/// Parsed only from `isRequired` / `is_required` / `required`. A provider name
/// never decides requiredness. Absent fields are [`Requirement::Unknown`], which
/// is reported and is not a writ merge gate.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    /// Payload marked this check required (`isRequired: true`).
    Required,
    /// Payload marked this check not required (`isRequired: false`).
    Advisory,
    /// Requiredness field absent; do not infer pass or fail from the vendor.
    Unknown,
}

impl Requirement {
    /// Stable snake_case token for residual / observation codes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Advisory => "advisory",
            Self::Unknown => "unknown",
        }
    }
}

/// How this check should be treated in a cycle: a required failure, a reported
/// finding, a wait, or an external access/configuration problem.
///
/// GitHub remains the required-check authority. Observation kinds other than
/// [`ObservationKind::RequiredFailure`] are not writ merge gates.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    /// Terminal success (including neutral).
    Success,
    /// `skipping` / `SKIPPED` — non-blocking.
    Skipping,
    /// Still running; continue other work.
    Pending,
    /// [`Requirement::Required`] and a blocking failure the agent may need to fix.
    RequiredFailure,
    /// Payload said not required; report the finding, do not treat as a writ gate.
    AdvisoryFinding,
    /// Dashboard / login / configuration gate (`ACTION_REQUIRED`), not a source fix.
    ExternalAccess,
    /// Blocking outcome with unknown requiredness; report, do not invent a gate.
    UnknownRequiredness,
}

impl ObservationKind {
    /// Stable snake_case token for observation codes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Skipping => "skipping",
            Self::Pending => "pending",
            Self::RequiredFailure => "required_failure",
            Self::AdvisoryFinding => "advisory_finding",
            Self::ExternalAccess => "external_access",
            Self::UnknownRequiredness => "unknown_requiredness",
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
    /// Performed terminal pass: `gh pr checks` bucket `pass` / GraphQL `SUCCESS` / `NEUTRAL`.
    ///
    /// [`Self::Skipped`] is non-blocking but is **not** a performed pass.
    #[must_use]
    pub const fn is_terminal_passing(self) -> bool {
        matches!(self, Self::Success | Self::Neutral)
    }

    /// Outcomes that do not fail the cycle: performed pass or an explicit skip.
    #[must_use]
    pub const fn is_non_blocking(self) -> bool {
        self.is_terminal_passing() || matches!(self, Self::Skipped)
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
    /// From `isRequired` / `required` only. Never inferred from the vendor name.
    pub requirement: Requirement,
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
    /// Required vs advisory vs pending vs external-access vs unknown.
    pub observation: ObservationKind,
}

/// Concise CI observation for collaboration/status consumers (RM-139 / RM-127).
///
/// This is a derived view of [`ClassificationReport`]. It is **not** a second
/// store: `ci classify` does not write `watched.json` or lease rows. Callers
/// copy these fields into shared status surfaces when they already have a
/// classify result.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CollaborationCiStatus {
    /// Same tokens as [`CiClass`]. `fail` only when GitHub marked a required check failed.
    pub ci_class: CiClass,
    /// Always `false`: an external/advisory/unknown gate must not freeze other jobs.
    pub blocks_unrelated_workers: bool,
    /// `true` unless this rollup has a required-check failure.
    pub continue_other_work: bool,
    pub required_failure_count: usize,
    pub pending_count: usize,
    pub skipped_count: usize,
    pub advisory_finding_count: usize,
    pub external_access_count: usize,
    pub unknown_requiredness_count: usize,
    pub residual_codes: Vec<String>,
    pub fixable_failure_count: usize,
    pub forbid_empty_retrigger_commit: bool,
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

    /// Failures whose recommended action is a source fix (not rerun or residual).
    #[must_use]
    pub fn fixable_failures(&self) -> Vec<&ClassifiedCheck> {
        self.failures()
            .into_iter()
            .filter(|c| matches!(c.recommended_action, RecommendedAction::FixSource))
            .collect()
    }

    /// Actual required-check failures. GitHub (via `isRequired`) is the authority.
    #[must_use]
    pub fn required_failures(&self) -> Vec<&ClassifiedCheck> {
        self.with_observation(ObservationKind::RequiredFailure)
    }

    /// Reported advisory findings; not writ merge gates.
    #[must_use]
    pub fn advisory_findings(&self) -> Vec<&ClassifiedCheck> {
        self.with_observation(ObservationKind::AdvisoryFinding)
    }

    /// Checks still running.
    #[must_use]
    pub fn pending_checks(&self) -> Vec<&ClassifiedCheck> {
        self.with_observation(ObservationKind::Pending)
    }

    /// `skipping` / `SKIPPED` checks. Non-blocking and not a performed pass.
    #[must_use]
    pub fn skipped_checks(&self) -> Vec<&ClassifiedCheck> {
        self.with_observation(ObservationKind::Skipping)
    }

    /// Dashboard / login / configuration problems (`ACTION_REQUIRED`).
    #[must_use]
    pub fn external_access(&self) -> Vec<&ClassifiedCheck> {
        self.with_observation(ObservationKind::ExternalAccess)
    }

    /// Blocking outcomes whose requiredness was not present on the payload.
    #[must_use]
    pub fn unknown_requiredness(&self) -> Vec<&ClassifiedCheck> {
        self.with_observation(ObservationKind::UnknownRequiredness)
    }

    fn with_observation(&self, kind: ObservationKind) -> Vec<&ClassifiedCheck> {
        self.checks
            .iter()
            .filter(|c| c.observation == kind)
            .collect()
    }

    /// Codes for one observation kind (residual code when present, else a stable label).
    #[must_use]
    pub fn observation_codes(&self, kind: ObservationKind) -> Vec<String> {
        let mut codes: Vec<String> = self
            .with_observation(kind)
            .into_iter()
            .map(observation_code)
            .collect();
        codes.sort();
        codes.dedup();
        codes
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

    /// True when every check is non-blocking **and** at least one actually passed.
    ///
    /// `skipping` / `SKIPPED` does not fail the cycle, but a skip-only rollup is
    /// not a performed pass. Empty input is unknown, not success. Pending checks
    /// mean the rollup is not done.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.checks.is_empty()
            && self
                .checks
                .iter()
                .all(|c| c.entry.conclusion.is_non_blocking())
            && self.has_performed_pass()
    }

    fn has_performed_pass(&self) -> bool {
        self.checks
            .iter()
            .any(|c| c.entry.conclusion.is_terminal_passing())
    }

    fn has_performed_required_pass(&self) -> bool {
        self.checks.iter().any(|c| {
            c.entry.requirement == Requirement::Required && c.entry.conclusion.is_terminal_passing()
        })
    }

    /// Job-level rollup for [`JobStatus.ci_class`](crate::status::JobStatus).
    ///
    /// `fail` is only [`ObservationKind::RequiredFailure`]. Advisory findings,
    /// `ACTION_REQUIRED` external-access, pending, skipping, and unknown
    /// requiredness are not treated as a writ merge gate. Skip-only rollups are
    /// [`CiClass::Unknown`], not [`CiClass::Pass`].
    #[must_use]
    pub fn job_ci_class(&self) -> CiClass {
        if self.checks.is_empty() {
            return CiClass::Unknown;
        }
        if !self.required_failures().is_empty() {
            return CiClass::Fail;
        }
        if !self.pending_checks().is_empty() {
            return CiClass::Pending;
        }
        if self.all_passed() || self.has_performed_required_pass() {
            return CiClass::Pass;
        }
        CiClass::Unknown
    }

    /// Compact payload for RM-139 / RM-127. Not persisted here.
    #[must_use]
    pub fn collaboration_status(&self) -> CollaborationCiStatus {
        CollaborationCiStatus {
            ci_class: self.job_ci_class(),
            blocks_unrelated_workers: false,
            continue_other_work: self.required_failures().is_empty(),
            required_failure_count: self.required_failures().len(),
            pending_count: self.pending_checks().len(),
            skipped_count: self.skipped_checks().len(),
            advisory_finding_count: self.advisory_findings().len(),
            external_access_count: self.external_access().len(),
            unknown_requiredness_count: self.unknown_requiredness().len(),
            residual_codes: self.residual_codes(),
            fixable_failure_count: self.fixable_failures().len(),
            forbid_empty_retrigger_commit: true,
        }
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

/// Classify one parsed check.
#[must_use]
pub fn classify_check(entry: CheckEntry) -> ClassifiedCheck {
    let check_class = classify_class(&entry);
    let policies = policies_for(check_class, entry.conclusion, entry.run_id);
    let residual_code = residual_code(check_class, &entry);
    let recommended_action =
        recommended_action(check_class, &entry, &policies, residual_code.as_deref());
    let observation = observation_kind(entry.requirement, entry.conclusion);
    let reason = explain(check_class, &entry);
    ClassifiedCheck {
        entry,
        check_class,
        policies,
        reason,
        residual_code,
        recommended_action,
        observation,
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
        "required_failure_count": report.required_failures().len(),
        "required_failure_codes": report.observation_codes(ObservationKind::RequiredFailure),
        "advisory_finding_codes": report.observation_codes(ObservationKind::AdvisoryFinding),
        "pending_codes": report.observation_codes(ObservationKind::Pending),
        "skipped_codes": report.observation_codes(ObservationKind::Skipping),
        "external_access_codes": report.observation_codes(ObservationKind::ExternalAccess),
        "unknown_requiredness_codes": report.observation_codes(ObservationKind::UnknownRequiredness),
        "github_is_required_check_authority": true,
        "unknown_requiredness_is_not_a_writ_merge_gate": true,
        "operator_note": "GitHub is the required-check authority. A provider name does not decide requiredness. Unknown requiredness, advisory findings, pending results, skipped analyses, and external access/configuration problems are reported and are not writ merge gates. A skipped analysis is not a performed pass.",
        "collaboration": report.collaboration_status(),
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

fn parse_requirement(raw: &Value) -> Requirement {
    for key in ["isRequired", "is_required", "required"] {
        match raw.get(key) {
            Some(Value::Bool(true)) => return Requirement::Required,
            Some(Value::Bool(false)) => return Requirement::Advisory,
            Some(Value::String(s)) => {
                let lower = s.trim().to_ascii_lowercase();
                if lower == "true" || lower == "required" {
                    return Requirement::Required;
                }
                if lower == "false" || lower == "advisory" || lower == "optional" {
                    return Requirement::Advisory;
                }
            }
            _ => {}
        }
    }
    Requirement::Unknown
}

fn observation_kind(requirement: Requirement, conclusion: CheckConclusion) -> ObservationKind {
    match conclusion {
        CheckConclusion::Success | CheckConclusion::Neutral => ObservationKind::Success,
        CheckConclusion::Skipped => ObservationKind::Skipping,
        CheckConclusion::Pending => ObservationKind::Pending,
        CheckConclusion::ActionRequired => ObservationKind::ExternalAccess,
        CheckConclusion::Failure
        | CheckConclusion::Cancelled
        | CheckConclusion::TimedOut
        | CheckConclusion::Stale
        | CheckConclusion::StartupFailure => match requirement {
            Requirement::Required => ObservationKind::RequiredFailure,
            Requirement::Advisory => ObservationKind::AdvisoryFinding,
            Requirement::Unknown => ObservationKind::UnknownRequiredness,
        },
    }
}

fn observation_code(check: &ClassifiedCheck) -> String {
    if let Some(code) = &check.residual_code {
        return code.clone();
    }
    let name = slug_name(&check.entry.name);
    format!("{}:{name}", check.observation.as_str())
}

fn classify_class(entry: &CheckEntry) -> CheckClass {
    let text = combined_text(entry);
    if is_class_b(&text, &entry.details_url) {
        return CheckClass::B;
    }
    if is_class_c(&text, &entry.details_url) {
        return CheckClass::C;
    }
    if is_class_a(entry) {
        return CheckClass::A;
    }
    CheckClass::C
}

/// First-party CI: a GitHub Actions/Azure build URL, or any named workflow.
fn is_class_a(entry: &CheckEntry) -> bool {
    is_github_actions_url(&entry.details_url)
        || is_azure_build_url(&entry.details_url)
        || !entry.workflow_name.trim().is_empty()
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
    is_class_c_vendor_text(text) || is_review_bot_text(text) || is_class_c_vendor_url(url)
}

/// Word-boundary vendor keywords that mark a third-party (Class C) review check.
fn is_class_c_vendor_text(text: &str) -> bool {
    const WORD_NEEDLES: &[&str] = &["kilo", "gitar", "dependabot", "renovate"];
    WORD_NEEDLES.iter().any(|word| contains_word(text, word))
        || contains_any(text, &["coderabbit", "code rabbit"])
}

/// Review-bot phrases: an explicit "code review" gate, or Copilot review checks.
fn is_review_bot_text(text: &str) -> bool {
    contains_any(text, &["code review"])
        || (contains_word(text, "copilot") && contains_word(text, "review"))
}

/// Vendor hostnames that mark a third-party (Class C) review check.
fn is_class_c_vendor_url(url: &str) -> bool {
    contains_any(
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
    if conclusion.is_non_blocking() || conclusion == CheckConclusion::Pending {
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
    if entry.conclusion.is_non_blocking() {
        return RecommendedAction::Ignore;
    }
    if entry.conclusion == CheckConclusion::Pending {
        return RecommendedAction::Wait;
    }
    if let Some(rerun) = rerun_action(check_class, entry, policies) {
        return rerun;
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

/// Official rerun for a Class A flake that carries a `Rerun` policy and a run id.
fn rerun_action(
    check_class: CheckClass,
    entry: &CheckEntry,
    policies: &BTreeSet<Policy>,
) -> Option<RecommendedAction> {
    if check_class == CheckClass::A && policies.contains(&Policy::Rerun) {
        return entry
            .run_id
            .map(|run_id| RecommendedAction::Rerun { run_id });
    }
    None
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

/// How a vendor's text keywords are matched, preserving the original per-vendor
/// semantics: `codacy`/`coderabbit` matched by substring (`contains_any`);
/// `kilo`/`gitar` matched on word boundaries (`contains_word`).
#[derive(Clone, Copy)]
enum TextMatch {
    /// Substring match over any of the needles.
    Substring,
    /// Word-boundary match over any of the needles.
    Word,
}

/// One vendor fingerprint: canonical slug, text keywords + how they match, and
/// a lowercase url substring.
struct VendorMatch {
    slug: &'static str,
    text_needles: &'static [&'static str],
    text_match: TextMatch,
    url_needle: &'static str,
}

/// Known residual vendors, in precedence order (codacy first, matching the
/// prior if-chain).
const VENDOR_MATCHES: &[VendorMatch] = &[
    VendorMatch {
        slug: "codacy",
        text_needles: &["codacy"],
        text_match: TextMatch::Substring,
        url_needle: "codacy",
    },
    VendorMatch {
        slug: "kilo",
        text_needles: &["kilo"],
        text_match: TextMatch::Word,
        url_needle: "kilo",
    },
    VendorMatch {
        slug: "coderabbit",
        text_needles: &["coderabbit", "code rabbit"],
        text_match: TextMatch::Substring,
        url_needle: "coderabbit",
    },
    VendorMatch {
        slug: "gitar",
        text_needles: &["gitar"],
        text_match: TextMatch::Word,
        url_needle: "gitar",
    },
];

fn vendor_slug(check_class: CheckClass, entry: &CheckEntry) -> String {
    let text = combined_text(entry);
    let url = entry.details_url.to_ascii_lowercase();
    if let Some(vendor) = VENDOR_MATCHES
        .iter()
        .find(|vendor| vendor.matches(&text, &url))
    {
        return vendor.slug.to_owned();
    }
    match check_class {
        CheckClass::B => "quality".to_owned(),
        CheckClass::C => "third_party".to_owned(),
        CheckClass::A => slug_name(&entry.name),
    }
}

impl VendorMatch {
    /// Text OR url match, preserving the original word/substring semantics.
    fn matches(&self, text: &str, lower_url: &str) -> bool {
        self.matches_text(text) || lower_url.contains(self.url_needle)
    }

    fn matches_text(&self, text: &str) -> bool {
        match self.text_match {
            TextMatch::Substring => contains_any(text, self.text_needles),
            TextMatch::Word => self.text_needles.iter().any(|w| contains_word(text, w)),
        }
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

    /// Assert a classified check's class, its exact recommended action, and its
    /// residual code (`None` for no residual), collapsing the common trio of
    /// per-check assertions into one call.
    fn assert_outcome(
        classified: &ClassifiedCheck,
        expected_class: CheckClass,
        expected_action: &RecommendedAction,
        expected_residual: Option<&str>,
    ) {
        assert_eq!(classified.check_class, expected_class);
        assert_eq!(&classified.recommended_action, expected_action);
        assert_eq!(classified.residual_code.as_deref(), expected_residual);
    }

    /// Assert that a classified check carries every policy in `present` and none
    /// of the policies in `absent`.
    fn assert_policies(classified: &ClassifiedCheck, present: &[Policy], absent: &[Policy]) {
        for policy in present {
            assert!(
                classified.policies.contains(policy),
                "expected policy {policy:?} to be present"
            );
        }
        for policy in absent {
            assert!(
                !classified.policies.contains(policy),
                "expected policy {policy:?} to be absent"
            );
        }
    }

    /// Per-class check counts for a report: `(class_a, class_b, class_c)`.
    fn assert_class_counts(report: &ClassificationReport, expected: (usize, usize, usize)) {
        assert_eq!(
            (
                report.class_a().len(),
                report.class_b().len(),
                report.class_c().len(),
            ),
            expected
        );
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
        assert_eq!(entry.requirement, Requirement::Unknown);
    }

    #[test]
    fn class_a_actions_and_azure() {
        let ci = classify_named("Build & Test", "CI", "fail", actions_url());
        assert_outcome(&ci, CheckClass::A, &RecommendedAction::FixSource, None);
        assert_policies(
            &ci,
            &[Policy::FixSource, Policy::ForbidEmptyCommit],
            &[Policy::Rerun],
        );

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
        assert_outcome(&fail, CheckClass::B, &RecommendedAction::FixSource, None);
        assert_policies(
            &fail,
            &[Policy::FixSource, Policy::ForbidEmptyCommit],
            &[Policy::Rerun],
        );

        let gate = classify_check(parse_check_entry(&json!({
            "name": "Codacy Static Code Analysis",
            "link": "https://app.codacy.com/gh/acme/example-org/pull-requests/12",
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
        assert_policies(&gate, &[Policy::MarkResidual], &[]);
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
            let pending_residual = format!("class_c:{vendor}_pending");
            assert_outcome(
                &pending,
                CheckClass::C,
                &RecommendedAction::Wait,
                Some(pending_residual.as_str()),
            );
            assert!(pending.policies.is_empty());
            assert!(!should_rerun(&pending));

            let fail = classify_named(name, "", "fail", link);
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
        assert!(!report.all_passed());
        assert_eq!(report.job_ci_class(), CiClass::Unknown);
        assert!(report.failures().is_empty());
        assert!(report.residual_codes().is_empty());
        assert_eq!(report.skipped_checks().len(), 1);
        assert_eq!(report.checks[0].observation, ObservationKind::Skipping);
        assert_eq!(
            report.checks[0].recommended_action,
            RecommendedAction::Ignore
        );
        let collab = report.collaboration_status();
        assert_eq!(collab.skipped_count, 1);
        assert!(!collab.blocks_unrelated_workers);
        assert!(collab.continue_other_work);
        assert_eq!(collab.ci_class, CiClass::Unknown);
    }

    #[test]
    fn skip_plus_success_is_all_passed() {
        let report = classify_checks(&[
            raw_check("Build & Test", "CI", "pass", actions_url()),
            raw_check("Supabase Preview", "", "skipping", ""),
        ]);
        assert!(report.all_passed());
        assert_eq!(report.job_ci_class(), CiClass::Pass);
        assert_eq!(report.skipped_checks().len(), 1);
        assert!(report.failures().is_empty());
    }

    #[test]
    fn required_skip_is_not_a_performed_pass() {
        let report = classify_checks_json(&json!([{
            "name": "optional-preview",
            "bucket": "skipping",
            "isRequired": true,
        }]))
        .unwrap();
        assert!(!report.all_passed());
        assert_eq!(report.job_ci_class(), CiClass::Unknown);
        assert_eq!(report.checks[0].observation, ObservationKind::Skipping);
        assert!(report.required_failures().is_empty());
        assert!(report.collaboration_status().continue_other_work);
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
        assert_outcome(
            &classified,
            CheckClass::A,
            &RecommendedAction::Rerun { run_id: 12345 },
            None,
        );
        assert_policies(&classified, &[Policy::ForbidEmptyCommit], &[]);
        assert!(should_rerun(&classified));
        assert_eq!(
            rerun_command(&classified),
            Some(vec![
                "run".to_owned(),
                "rerun".to_owned(),
                "12345".to_owned()
            ])
        );
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
        assert_class_counts(&report, (1, 1, 2));
        assert!(report.required_failures().is_empty());
        assert_eq!(report.unknown_requiredness().len(), 3);
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

    #[test]
    fn requiredness_comes_from_payload_not_provider_name() {
        let absent = parse_check_entry(&json!({
            "name": "Codacy Static Code Analysis",
            "bucket": "fail",
            "link": "https://app.codacy.com/gh/acme/example-org/pull-requests/12",
        }));
        assert_eq!(absent.requirement, Requirement::Unknown);
        assert_eq!(absent.requirement.as_str(), "unknown");

        let required = parse_check_entry(&json!({
            "name": "CodeRabbit",
            "bucket": "fail",
            "link": "https://coderabbit.ai/review/1",
            "isRequired": true,
        }));
        assert_eq!(required.requirement, Requirement::Required);

        let advisory = parse_check_entry(&json!({
            "name": "Build & Test",
            "workflow": "CI",
            "bucket": "fail",
            "link": actions_url(),
            "required": false,
        }));
        assert_eq!(advisory.requirement, Requirement::Advisory);
    }

    #[test]
    fn observation_kind_splits_required_advisory_unknown_and_external() {
        let required = classify_check(parse_check_entry(&json!({
            "name": "Build & Test",
            "workflow": "CI",
            "bucket": "fail",
            "link": actions_url(),
            "isRequired": true,
        })));
        assert_eq!(required.observation, ObservationKind::RequiredFailure);
        assert_eq!(required.recommended_action, RecommendedAction::FixSource);

        let advisory = classify_check(parse_check_entry(&json!({
            "name": "qlty check",
            "bucket": "fail",
            "link": "https://qlty.sh/gh/acme/example-org/pull/1",
            "isRequired": false,
        })));
        assert_eq!(advisory.observation, ObservationKind::AdvisoryFinding);
        assert_eq!(advisory.check_class, CheckClass::C);

        let unknown = classify_named("Build & Test", "CI", "fail", actions_url());
        assert_eq!(unknown.observation, ObservationKind::UnknownRequiredness);

        let external = classify_check(parse_check_entry(&json!({
            "name": "Codacy Static Code Analysis",
            "link": "https://app.codacy.com/gh/acme/example-org/pull-requests/12",
            "conclusion": "ACTION_REQUIRED",
            "isRequired": true,
        })));
        assert_eq!(external.observation, ObservationKind::ExternalAccess);
        assert_eq!(
            external.residual_code.as_deref(),
            Some("class_b:codacy_action_required")
        );

        let pending = classify_named("Build & Test", "CI", "pending", actions_url());
        assert_eq!(pending.observation, ObservationKind::Pending);
    }

    #[test]
    fn class_c_is_required_only_when_payload_says_so() {
        let required_bot = classify_check(parse_check_entry(&json!({
            "name": "Kilo Code Review",
            "bucket": "fail",
            "link": "https://app.kilo.ai/review/1",
            "isRequired": true,
        })));
        assert_eq!(required_bot.check_class, CheckClass::C);
        assert_eq!(required_bot.observation, ObservationKind::RequiredFailure);

        let unnamed = classify_named(
            "Kilo Code Review",
            "",
            "fail",
            "https://app.kilo.ai/review/1",
        );
        assert_eq!(unnamed.observation, ObservationKind::UnknownRequiredness);
    }

    #[test]
    fn fixable_failures_are_source_fixes_only() {
        let report = classify_checks_json(&json!([
            {
                "name": "Build & Test",
                "workflow": "CI",
                "bucket": "fail",
                "link": actions_url(),
                "isRequired": true,
            },
            {
                "name": "Build & Test",
                "workflow": "CI",
                "bucket": "timed_out",
                "link": actions_url(),
            },
            {
                "name": "Codacy Static Code Analysis",
                "link": "https://app.codacy.com/gh/acme/example-org/pull-requests/12",
                "conclusion": "ACTION_REQUIRED",
            },
            {
                "name": "Kilo Code Review",
                "bucket": "fail",
                "link": "https://app.kilo.ai/review/1",
            }
        ]))
        .unwrap();
        assert_eq!(report.fixable_failures().len(), 1);
        assert_eq!(report.fixable_failures()[0].entry.name, "Build & Test");
        assert_eq!(report.required_failures().len(), 1);
        assert_eq!(report.external_access().len(), 1);
        assert_eq!(
            report.observation_codes(ObservationKind::ExternalAccess),
            vec!["class_b:codacy_action_required".to_owned()]
        );
        assert!(!should_rerun(report.fixable_failures()[0]));
        assert!(should_rerun(&report.checks[1]));
    }

    #[test]
    fn classify_response_data_does_not_treat_unknown_as_a_gate() {
        let report = classify_checks(&[raw_check("Build & Test", "CI", "fail", actions_url())]);
        let data = classify_response_data(&report);
        assert_eq!(data["required_failure_count"], 0);
        assert_eq!(data["github_is_required_check_authority"], true);
        assert_eq!(data["unknown_requiredness_is_not_a_writ_merge_gate"], true);
        assert_eq!(
            data["unknown_requiredness_codes"],
            json!(["unknown_requiredness:build_test"])
        );
        assert!(
            data["operator_note"]
                .as_str()
                .unwrap()
                .contains("GitHub is the required-check authority")
        );
        assert_eq!(data["collaboration"]["ci_class"], "unknown");
        assert_eq!(data["collaboration"]["blocks_unrelated_workers"], false);
        assert_eq!(data["collaboration"]["continue_other_work"], true);
        assert_eq!(data["collaboration"]["forbid_empty_retrigger_commit"], true);
        assert_eq!(data["collaboration"]["skipped_count"], 0);
        assert_eq!(data["skipped_codes"], json!([]));
    }

    #[test]
    fn collaboration_status_does_not_freeze_unrelated_workers() {
        let unknown_fail =
            classify_checks(&[raw_check("Build & Test", "CI", "fail", actions_url())]);
        assert_eq!(unknown_fail.job_ci_class(), CiClass::Unknown);
        let collab = unknown_fail.collaboration_status();
        assert!(!collab.blocks_unrelated_workers);
        assert!(collab.continue_other_work);
        assert_eq!(collab.unknown_requiredness_count, 1);

        let pending = classify_checks(&[raw_check("Build & Test", "CI", "pending", actions_url())]);
        assert_eq!(pending.job_ci_class(), CiClass::Pending);
        assert!(pending.collaboration_status().continue_other_work);

        let external = classify_checks_json(&json!([{
            "name": "Codacy Static Code Analysis",
            "link": "https://app.codacy.com/gh/acme/example-org/pull-requests/12",
            "conclusion": "ACTION_REQUIRED",
        }]))
        .unwrap();
        assert_eq!(external.job_ci_class(), CiClass::Unknown);
        assert_eq!(external.collaboration_status().external_access_count, 1);
        assert!(!external.collaboration_status().blocks_unrelated_workers);

        let required_fail = classify_checks_json(&json!([{
            "name": "Build & Test",
            "workflow": "CI",
            "bucket": "fail",
            "link": actions_url(),
            "isRequired": true,
        }]))
        .unwrap();
        assert_eq!(required_fail.job_ci_class(), CiClass::Fail);
        assert!(!required_fail.collaboration_status().continue_other_work);
        assert!(
            !required_fail
                .collaboration_status()
                .blocks_unrelated_workers
        );

        let required_pass_advisory_fail = classify_checks_json(&json!([
            {
                "name": "Build & Test",
                "workflow": "CI",
                "bucket": "pass",
                "link": actions_url(),
                "isRequired": true,
            },
            {
                "name": "Codacy Static Code Analysis",
                "bucket": "fail",
                "link": "https://app.codacy.com/gh/acme/example-org/pull-requests/12",
                "isRequired": false,
            }
        ]))
        .unwrap();
        assert_eq!(required_pass_advisory_fail.job_ci_class(), CiClass::Pass);
        assert_eq!(
            required_pass_advisory_fail
                .collaboration_status()
                .advisory_finding_count,
            1
        );

        let empty = ClassificationReport::default();
        assert_eq!(empty.job_ci_class(), CiClass::Unknown);
    }
}
