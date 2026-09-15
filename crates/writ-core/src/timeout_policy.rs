//! Named timeout policy, stuck detection, redispatch budget, and hang-recovery residuals.
//!
//! This module is the process-supervisor contract for GitHub / Linear issue
//! "Subagent timeouts and hang recovery" (RM-129). It does **not** implement
//! hive worker redispatch: `writ` never merges, never bare-force-pushes, and
//! never invents a commit SHA while recovering a stuck child.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// CLI / env key: wall-clock timeout in seconds (`0` = unlimited).
pub const ENV_TIMEOUT_SECS: &str = "WRIT_SUPERVISOR_TIMEOUT_SECS";
/// CLI / env key: SIGTERM grace period in seconds before SIGKILL.
pub const ENV_GRACE_SECS: &str = "WRIT_SUPERVISOR_GRACE_SECS";
/// CLI / env key: stall (no stdout/stderr) threshold in seconds (`0` = disabled).
pub const ENV_STALL_SECS: &str = "WRIT_SUPERVISOR_STALL_SECS";
/// CLI / env key: progress heartbeat interval in seconds (`0` = disabled).
pub const ENV_PROGRESS_SECS: &str = "WRIT_SUPERVISOR_PROGRESS_SECS";
/// CLI / env key: host-side retry cap after a residual (`0` = never redispatch).
pub const ENV_MAX_REDISPATCH: &str = "WRIT_SUPERVISOR_MAX_REDISPATCH";

/// Named default: unlimited wall-clock (CLI `--timeout` default).
pub const TIMEOUT_SECS_UNLIMITED: u64 = 0;
/// Named recommended default for host-dispatched worker processes.
pub const TIMEOUT_SECS_WORKER: u64 = 1_800;
/// Named recommended default for `cargo test` / clippy / fmt gates.
pub const TIMEOUT_SECS_GATE: u64 = 600;
/// Named default: seconds to wait after graceful cancel before kill.
pub const GRACE_SECS_DEFAULT: u64 = 5;
/// Named default: stall detection off.
pub const STALL_SECS_DEFAULT: u64 = 0;
/// Named default: progress heartbeat while waiting.
pub const PROGRESS_SECS_DEFAULT: u64 = 10;
/// Named default: at most one host redispatch after the original attempt.
pub const MAX_REDISPATCH_PER_ITEM_DEFAULT: u32 = 1;

/// Why the supervisor classified a child as stuck.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StuckReason {
    /// Wall-clock deadline (including permit wait) fired.
    WallClock,
    /// No stdout/stderr bytes for `--stall` / `WRIT_SUPERVISOR_STALL_SECS`.
    Stall,
    /// Timed out while waiting for a process-local max-parallel permit.
    PermitWait,
}

/// Last recovery action the supervisor took. Never includes merge or push.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStage {
    /// No child to cancel (permit-wait timeout).
    None,
    /// Graceful cancel was sent and the child exited before the kill deadline.
    GracefulCancel,
    /// Process-group / child kill ran (after grace, or immediately when grace is 0).
    Kill,
}

/// Watchlist / supervised-output residual for a timeout or hang.
///
/// Additive v1 field. Intentionally has **no** commit SHA, head, or identity
/// fields: a timeout residual must never be used as a fake verified-commit reply.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimeoutResidual {
    /// Stuck-detection criterion that fired.
    pub reason: StuckReason,
    /// Last recovery action taken by the supervisor.
    pub recovery_stage: RecoveryStage,
    /// Hosts must use [`RedispatchBudget`], not this field, to cap retries.
    /// A single `writ supervisor run` always reports `redispatch_count: 0`.
    pub redispatch_count: u32,
    /// Configured host cap (`max_redispatch_per_item`).
    pub max_redispatch_per_item: u32,
    /// Wall time from `run` start to residual, in milliseconds.
    pub elapsed_ms: u64,
    /// Milliseconds from run start to the last captured child byte, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_output_ms: Option<u64>,
}

impl TimeoutResidual {
    /// True when the configured cap forbids any host retry (`max == 0`).
    ///
    /// Do **not** treat `redispatch_count` from a `writ supervisor run` residual
    /// as consumed budget — that field is always `0` because writ does not retry.
    /// Hosts must call [`RedispatchBudget::try_acquire`] themselves.
    #[must_use]
    pub fn redispatch_forbidden(&self) -> bool {
        self.max_redispatch_per_item == 0
    }
}

/// Heartbeat emitted on stderr (CLI) or via [`ProgressCallback`] (library).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProgressReport {
    /// Milliseconds since `run` started.
    pub elapsed_ms: u64,
    /// Remaining wall-clock budget, if a timeout is set.
    pub remaining_ms: Option<u64>,
    /// Milliseconds since start of last child output, if any.
    pub last_output_ms: Option<u64>,
    /// Current wait phase.
    pub wait_phase: WaitPhase,
    /// Process-local active supervised children.
    pub active: usize,
}

/// Whether the supervisor is queued or already running a child.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitPhase {
    /// Waiting for a process-local max-parallel permit.
    PermitWait,
    /// Child is running (or draining pipes).
    Child,
}

impl WaitPhase {
    /// Stable snake_case token for progress lines.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PermitWait => "permit_wait",
            Self::Child => "child",
        }
    }
}

/// Callback used by library callers (and the CLI) to report wait progress.
pub type ProgressCallback = Arc<dyn Fn(&ProgressReport) + Send + Sync>;

/// Per-run timeout / recovery knobs. Library [`Default`] preserves historical
/// kill-immediately behaviour; CLI named defaults live in [`TimeoutPolicy::cli_named`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TimeoutPolicy {
    /// SIGTERM grace before SIGKILL. `Duration::ZERO` skips graceful cancel.
    pub grace: Duration,
    /// No-output stall threshold. `None` disables stall detection.
    pub stall: Option<Duration>,
    /// Progress heartbeat interval. `None` disables heartbeats.
    pub progress: Option<Duration>,
    /// Host redispatch cap recorded on residuals. The supervisor never retries.
    pub max_redispatch_per_item: u32,
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self {
            grace: Duration::ZERO,
            stall: None,
            progress: None,
            max_redispatch_per_item: MAX_REDISPATCH_PER_ITEM_DEFAULT,
        }
    }
}

impl TimeoutPolicy {
    /// Named CLI defaults (`grace=5s`, `progress=10s`, stall off, redispatch cap 1).
    #[must_use]
    pub fn cli_named() -> Self {
        Self {
            grace: Duration::from_secs(GRACE_SECS_DEFAULT),
            stall: None,
            progress: Some(Duration::from_secs(PROGRESS_SECS_DEFAULT)),
            max_redispatch_per_item: MAX_REDISPATCH_PER_ITEM_DEFAULT,
        }
    }

    /// Build a policy from CLI / env numeric seconds (`0` disables optional knobs).
    #[must_use]
    pub fn from_seconds(
        grace_secs: u64,
        stall_secs: u64,
        progress_secs: u64,
        max_redispatch: u32,
    ) -> Self {
        Self {
            grace: Duration::from_secs(grace_secs),
            stall: (stall_secs > 0).then(|| Duration::from_secs(stall_secs)),
            progress: (progress_secs > 0).then(|| Duration::from_secs(progress_secs)),
            max_redispatch_per_item: max_redispatch,
        }
    }

    /// Parse optional knobs from process env, falling back to [`Self::cli_named`].
    #[must_use]
    pub fn from_env() -> Self {
        let named = Self::cli_named();
        Self::from_seconds(
            env_u64(ENV_GRACE_SECS, named.grace.as_secs()),
            env_u64(ENV_STALL_SECS, STALL_SECS_DEFAULT),
            env_u64(ENV_PROGRESS_SECS, named.progress.map_or(0, |d| d.as_secs())),
            env_u32(ENV_MAX_REDISPATCH, named.max_redispatch_per_item),
        )
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Host-side cap that prevents infinite retry loops.
///
/// `writ supervisor run` does **not** consume this budget. A harness that
/// re-invokes the supervisor after a residual must call [`RedispatchBudget::try_acquire`]
/// once per retry. Mutating `git` / `gh` commands remain policy-checked on every
/// attempt; recovery never merges and never bare-force-pushes.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RedispatchBudget {
    max_per_item: u32,
    used: u32,
}

/// The host has already redispatched this item `max_redispatch_per_item` times.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RedispatchExhausted {
    /// Configured cap.
    pub max_per_item: u32,
    /// Attempts already consumed.
    pub used: u32,
}

impl RedispatchBudget {
    /// Create a budget. `0` means the host must never redispatch.
    #[must_use]
    pub fn new(max_per_item: u32) -> Self {
        Self {
            max_per_item,
            used: 0,
        }
    }

    /// Named default budget (`max_redispatch_per_item` = 1).
    #[must_use]
    pub fn named_default() -> Self {
        Self::new(MAX_REDISPATCH_PER_ITEM_DEFAULT)
    }

    /// Configured cap.
    #[must_use]
    pub fn max_per_item(&self) -> u32 {
        self.max_per_item
    }

    /// Redispatches already consumed.
    #[must_use]
    pub fn used(&self) -> u32 {
        self.used
    }

    /// Remaining redispatches (`0` means mark residual and free the slot).
    #[must_use]
    pub fn remaining(&self) -> u32 {
        self.max_per_item.saturating_sub(self.used)
    }

    /// Consume one redispatch. Errors when the cap is exhausted.
    pub fn try_acquire(&mut self) -> Result<(), RedispatchExhausted> {
        if self.used >= self.max_per_item {
            return Err(RedispatchExhausted {
                max_per_item: self.max_per_item,
                used: self.used,
            });
        }
        self.used += 1;
        Ok(())
    }
}

/// Format a progress heartbeat for stderr (diagnostics; never stdout).
#[must_use]
pub fn format_progress_line(report: &ProgressReport) -> String {
    let remaining = match report.remaining_ms {
        Some(ms) => format!("{}s", ms / 1000),
        None => "unlimited".to_owned(),
    };
    let last_output = match report.last_output_ms {
        Some(ms) => format!("{}s", ms / 1000),
        None => "none".to_owned(),
    };
    format!(
        "writ supervisor: elapsed={}s remaining={remaining} last_output={last_output} wait={} active={}",
        report.elapsed_ms / 1000,
        report.wait_phase.as_str(),
        report.active
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_defaults_are_stable() {
        assert_eq!(TIMEOUT_SECS_UNLIMITED, 0);
        assert_eq!(TIMEOUT_SECS_WORKER, 1_800);
        assert_eq!(TIMEOUT_SECS_GATE, 600);
        assert_eq!(GRACE_SECS_DEFAULT, 5);
        assert_eq!(STALL_SECS_DEFAULT, 0);
        assert_eq!(PROGRESS_SECS_DEFAULT, 10);
        assert_eq!(MAX_REDISPATCH_PER_ITEM_DEFAULT, 1);
        assert_eq!(ENV_TIMEOUT_SECS, "WRIT_SUPERVISOR_TIMEOUT_SECS");
        assert_eq!(ENV_GRACE_SECS, "WRIT_SUPERVISOR_GRACE_SECS");
        assert_eq!(ENV_STALL_SECS, "WRIT_SUPERVISOR_STALL_SECS");
        assert_eq!(ENV_PROGRESS_SECS, "WRIT_SUPERVISOR_PROGRESS_SECS");
        assert_eq!(ENV_MAX_REDISPATCH, "WRIT_SUPERVISOR_MAX_REDISPATCH");
    }

    #[test]
    fn redispatch_budget_caps_retries() {
        let mut budget = RedispatchBudget::new(1);
        budget.try_acquire().expect("first redispatch allowed");
        let err = budget.try_acquire().expect_err("second redispatch blocked");
        assert_eq!(err.max_per_item, 1);
        assert_eq!(err.used, 1);
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn zero_budget_never_redispatches() {
        let mut budget = RedispatchBudget::new(0);
        assert!(budget.try_acquire().is_err());
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn residual_has_no_fake_sha_fields() {
        let residual = TimeoutResidual {
            reason: StuckReason::WallClock,
            recovery_stage: RecoveryStage::Kill,
            redispatch_count: 0,
            max_redispatch_per_item: 1,
            elapsed_ms: 200,
            last_output_ms: None,
        };
        let v = serde_json::to_value(&residual).unwrap();
        let obj = v.as_object().expect("object");
        for key in obj.keys() {
            let lower = key.to_ascii_lowercase();
            assert!(
                !lower.contains("sha") && !lower.contains("commit") && !lower.contains("head"),
                "timeout residual must not carry identity field `{key}`"
            );
        }
        assert!(!residual.redispatch_forbidden());
        let exhausted = TimeoutResidual {
            max_redispatch_per_item: 0,
            ..residual.clone()
        };
        assert!(exhausted.redispatch_forbidden());
    }

    #[test]
    fn recovery_stage_has_no_merge_or_push_variant() {
        let stages = [
            RecoveryStage::None,
            RecoveryStage::GracefulCancel,
            RecoveryStage::Kill,
        ];
        for stage in stages {
            let token = serde_json::to_string(&stage).unwrap();
            assert!(!token.contains("merge"), "{token}");
            assert!(!token.contains("push"), "{token}");
        }
    }

    #[test]
    fn from_seconds_zero_disables_optional_knobs() {
        let policy = TimeoutPolicy::from_seconds(5, 0, 0, 1);
        assert_eq!(policy.grace, Duration::from_secs(5));
        assert!(policy.stall.is_none());
        assert!(policy.progress.is_none());
        assert_eq!(policy.max_redispatch_per_item, 1);
    }

    #[test]
    fn progress_line_is_stderr_diagnostics() {
        let line = format_progress_line(&ProgressReport {
            elapsed_ms: 5_000,
            remaining_ms: Some(25_000),
            last_output_ms: None,
            wait_phase: WaitPhase::Child,
            active: 1,
        });
        assert!(line.starts_with("writ supervisor:"));
        assert!(line.contains("wait=child"));
        assert!(line.contains("remaining=25s"));
    }
}
