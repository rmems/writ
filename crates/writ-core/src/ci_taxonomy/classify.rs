//! Class, policy, and recommended-action decisions for one parsed check.

use std::collections::BTreeSet;

use serde_json::Value;

use super::parse::{extract_check_nodes, parse_check_entry};
use super::probe::{Fragments, Probe};
use super::{
    CheckClass, CheckConclusion, CheckEntry, ClassificationReport, ClassifiedCheck,
    ObservationKind, Policy, RecommendedAction, Requirement,
};

/// Classify one parsed check.
#[must_use]
pub fn classify_check(entry: CheckEntry) -> ClassifiedCheck {
    let check_class = classify_class(&entry);
    let policies = policies_for(check_class, entry.conclusion, entry.run_id);
    let residual_code = residual_code(check_class, &entry);
    let recommended_action = recommended_action(
        check_class,
        &entry,
        &policies,
        residual_code.as_deref().map(ResidualLabel),
    );
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

/// Official rerun is allowed only for Class A flakes with a GitHub Actions run id.
#[must_use]
pub fn should_rerun(classified: &ClassifiedCheck) -> bool {
    if classified.check_class != CheckClass::A {
        return false;
    }
    if !classified.policies.contains(&Policy::Rerun) {
        return false;
    }
    if classified.entry.run_id.is_none() {
        return false;
    }
    is_flake_conclusion(classified.entry.conclusion)
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

pub(super) fn observation_code(check: &ClassifiedCheck) -> String {
    if let Some(code) = &check.residual_code {
        return code.clone();
    }
    let name = Probe::lowercased(&check.entry.name).slug();
    format!("{}:{name}", check.observation.as_str())
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
        | CheckConclusion::StartupFailure => requirement_observation(requirement),
    }
}

fn requirement_observation(requirement: Requirement) -> ObservationKind {
    match requirement {
        Requirement::Required => ObservationKind::RequiredFailure,
        Requirement::Advisory => ObservationKind::AdvisoryFinding,
        Requirement::Unknown => ObservationKind::UnknownRequiredness,
    }
}

fn classify_class(entry: &CheckEntry) -> CheckClass {
    let text = Probe::from_entry_text(entry);
    let url = Probe::from_details_url(entry);
    if is_class_b(&text, &url) {
        return CheckClass::B;
    }
    if is_class_c(&text, &url) {
        return CheckClass::C;
    }
    if is_class_a(entry, &url) {
        return CheckClass::A;
    }
    CheckClass::C
}

/// First-party CI: a GitHub Actions/Azure build URL, or any named workflow.
fn is_class_a(entry: &CheckEntry, url: &Probe) -> bool {
    if is_github_actions_url(url) {
        return true;
    }
    if is_azure_build_url(url) {
        return true;
    }
    !entry.workflow_name.trim().is_empty()
}

fn is_class_b(text: &Probe, url: &Probe) -> bool {
    if text.contains_fragment(&Fragments(&[
        "codacy",
        "code climate",
        "codeclimate",
        "deepsource",
    ])) {
        return true;
    }
    if text.contains_word(&Fragments(&["sonarcloud", "sonarqube", "sonar"])) {
        return true;
    }
    url.contains_fragment(&Fragments(&["app.codacy.com", "codacy.com"]))
}

fn is_class_c(text: &Probe, url: &Probe) -> bool {
    if is_class_c_vendor_text(text) {
        return true;
    }
    if is_review_bot_text(text) {
        return true;
    }
    is_class_c_vendor_url(url)
}

/// Word-boundary vendor keywords that mark a third-party (Class C) review check.
fn is_class_c_vendor_text(text: &Probe) -> bool {
    if text.contains_word(&Fragments(&["kilo", "gitar", "dependabot", "renovate"])) {
        return true;
    }
    text.contains_fragment(&Fragments(&["coderabbit", "code rabbit"]))
}

/// Review-bot phrases: an explicit "code review" gate, or Copilot review checks.
fn is_review_bot_text(text: &Probe) -> bool {
    if text.contains_fragment(&Fragments(&["code review"])) {
        return true;
    }
    if !text.contains_word(&Fragments(&["copilot"])) {
        return false;
    }
    text.contains_word(&Fragments(&["review"]))
}

/// Vendor hostnames that mark a third-party (Class C) review check.
fn is_class_c_vendor_url(url: &Probe) -> bool {
    url.contains_fragment(&Fragments(&[
        "kilo.ai",
        "app.kilo.ai",
        "coderabbit.ai",
        "gitar.ai",
    ]))
}

fn is_github_actions_url(url: &Probe) -> bool {
    if !url.contains_fragment(&Fragments(&["/actions/runs/"])) {
        return false;
    }
    url.contains_fragment(&Fragments(&["github.com"]))
}

fn is_azure_build_url(url: &Probe) -> bool {
    if url.contains_fragment(&Fragments(&["dev.azure.com"])) {
        return true;
    }
    url.contains_fragment(&Fragments(&["visualstudio.com"]))
}

fn policies_for(
    check_class: CheckClass,
    conclusion: CheckConclusion,
    run_id: Option<u64>,
) -> BTreeSet<Policy> {
    if conclusion.is_non_blocking() {
        return BTreeSet::new();
    }
    if conclusion == CheckConclusion::Pending {
        return BTreeSet::new();
    }

    let mut policies = BTreeSet::from([Policy::ForbidEmptyCommit]);
    match check_class {
        CheckClass::A => insert_class_a_policies(&mut policies, conclusion, run_id),
        CheckClass::B => insert_class_b_policies(&mut policies, conclusion),
        CheckClass::C => {
            policies.insert(Policy::ReportOnly);
            policies.insert(Policy::MarkResidual);
        }
    }
    policies
}

fn insert_class_a_policies(
    policies: &mut BTreeSet<Policy>,
    conclusion: CheckConclusion,
    run_id: Option<u64>,
) {
    policies.insert(Policy::ForbidIgnore);
    policies.insert(Policy::FixSource);
    policies.insert(Policy::ReplyWithSha);
    if run_id.is_none() {
        return;
    }
    if is_flake_conclusion(conclusion) {
        policies.insert(Policy::Rerun);
    }
}

fn insert_class_b_policies(policies: &mut BTreeSet<Policy>, conclusion: CheckConclusion) {
    policies.insert(Policy::ForbidIgnore);
    policies.insert(Policy::FixSource);
    policies.insert(Policy::ReplyWithSha);
    if conclusion == CheckConclusion::ActionRequired {
        policies.insert(Policy::MarkResidual);
    }
}

fn is_flake_conclusion(conclusion: CheckConclusion) -> bool {
    matches!(
        conclusion,
        CheckConclusion::TimedOut | CheckConclusion::StartupFailure | CheckConclusion::Cancelled
    )
}

struct VendorSlug(String);

fn residual_code(check_class: CheckClass, entry: &CheckEntry) -> Option<String> {
    let vendor = VendorSlug(vendor_slug(check_class, entry));
    match entry.conclusion {
        CheckConclusion::Success | CheckConclusion::Skipped | CheckConclusion::Neutral => None,
        CheckConclusion::Pending => pending_residual(check_class, &vendor),
        CheckConclusion::ActionRequired => Some(action_required_residual(check_class, &vendor)),
        _ => blocking_residual(check_class, entry, &vendor),
    }
}

fn pending_residual(check_class: CheckClass, vendor: &VendorSlug) -> Option<String> {
    match check_class {
        CheckClass::C => Some(format!("class_c:{}_pending", vendor.0)),
        CheckClass::A | CheckClass::B => None,
    }
}

fn action_required_residual(check_class: CheckClass, vendor: &VendorSlug) -> String {
    match check_class {
        CheckClass::B => format!("class_b:{}_action_required", vendor.0),
        CheckClass::C => format!("class_c:{}_action_required", vendor.0),
        CheckClass::A => format!("class_a:{}_action_required", vendor.0),
    }
}

fn blocking_residual(
    check_class: CheckClass,
    entry: &CheckEntry,
    vendor: &VendorSlug,
) -> Option<String> {
    let slug = conclusion_slug(entry.conclusion);
    match check_class {
        CheckClass::C => Some(format!("class_c:{}_{slug}", vendor.0)),
        CheckClass::B if entry.conclusion == CheckConclusion::Failure => None,
        CheckClass::B => Some(format!("class_b:{}_{slug}", vendor.0)),
        CheckClass::A => None,
    }
}

struct ResidualLabel<'a>(&'a str);

fn recommended_action(
    check_class: CheckClass,
    entry: &CheckEntry,
    policies: &BTreeSet<Policy>,
    residual: Option<ResidualLabel<'_>>,
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
    if let Some(ResidualLabel(code)) = residual {
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
    if check_class != CheckClass::A {
        return None;
    }
    if !policies.contains(&Policy::Rerun) {
        return None;
    }
    entry
        .run_id
        .map(|run_id| RecommendedAction::Rerun { run_id })
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
    text_needles: Fragments<'static>,
    text_match: TextMatch,
    url_needles: Fragments<'static>,
}

/// Known residual vendors, in precedence order (codacy first, matching the
/// prior if-chain).
const VENDOR_MATCHES: &[VendorMatch] = &[
    VendorMatch {
        slug: "codacy",
        text_needles: Fragments(&["codacy"]),
        text_match: TextMatch::Substring,
        url_needles: Fragments(&["codacy"]),
    },
    VendorMatch {
        slug: "kilo",
        text_needles: Fragments(&["kilo"]),
        text_match: TextMatch::Word,
        url_needles: Fragments(&["kilo"]),
    },
    VendorMatch {
        slug: "coderabbit",
        text_needles: Fragments(&["coderabbit", "code rabbit"]),
        text_match: TextMatch::Substring,
        url_needles: Fragments(&["coderabbit"]),
    },
    VendorMatch {
        slug: "gitar",
        text_needles: Fragments(&["gitar"]),
        text_match: TextMatch::Word,
        url_needles: Fragments(&["gitar"]),
    },
];

fn vendor_slug(check_class: CheckClass, entry: &CheckEntry) -> String {
    let text = Probe::from_entry_text(entry);
    let url = Probe::from_details_url(entry);
    if let Some(vendor) = VENDOR_MATCHES
        .iter()
        .find(|vendor| vendor.matches(&text, &url))
    {
        return vendor.slug.to_owned();
    }
    match check_class {
        CheckClass::B => "quality".to_owned(),
        CheckClass::C => "third_party".to_owned(),
        CheckClass::A => Probe::lowercased(&entry.name).slug(),
    }
}

impl VendorMatch {
    /// Text OR url match, preserving the original word/substring semantics.
    fn matches(&self, text: &Probe, lower_url: &Probe) -> bool {
        if self.matches_text(text) {
            return true;
        }
        lower_url.contains_fragment(&self.url_needles)
    }

    fn matches_text(&self, text: &Probe) -> bool {
        match self.text_match {
            TextMatch::Substring => text.contains_fragment(&self.text_needles),
            TextMatch::Word => text.contains_word(&self.text_needles),
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
