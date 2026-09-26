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

mod classify;
mod parse;
mod probe;

#[cfg(test)]
mod tests;

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
            .map(classify::observation_code)
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
        if self.checks.is_empty() {
            return false;
        }
        if !self
            .checks
            .iter()
            .all(|c| c.entry.conclusion.is_non_blocking())
        {
            return false;
        }
        self.has_performed_pass()
    }

    fn has_performed_pass(&self) -> bool {
        self.checks
            .iter()
            .any(|c| c.entry.conclusion.is_terminal_passing())
    }

    fn has_performed_required_pass(&self) -> bool {
        self.checks.iter().any(check_is_required_pass)
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
        if self.all_passed() {
            return CiClass::Pass;
        }
        if self.has_performed_required_pass() {
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

fn check_is_required_pass(check: &ClassifiedCheck) -> bool {
    if check.entry.requirement != Requirement::Required {
        return false;
    }
    check.entry.conclusion.is_terminal_passing()
}

pub use classify::{
    classify_check, classify_checks, classify_checks_json, classify_from_slice, rerun_command,
    should_rerun,
};
pub use parse::{extract_check_nodes, parse_check_entry};

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
