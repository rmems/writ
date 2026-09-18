//! Named timeout policy, stuck-class taxonomy, and redispatch budget.
//!
//! `writ` supervises **one process execution**. It does not retry, re-dispatch
//! workers, merge, or push. Harness-level retry uses
//! [`DEFAULT_MAX_REDISPATCH_PER_ITEM`] and [`can_redispatch`].
//! Operator policy: `docs/timeout-policy.md`.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Progress tick callback. Diagnostics only — never JSON stdout.
pub type ProgressCallback = Arc<dyn Fn(&ProgressSnapshot) + Send + Sync>;

/// Default hard/worker/step/idle/orchestrator value: disabled (`0` on the CLI).
pub const DEFAULT_WORKER_SECS: u64 = 0;
/// Default per-step cap: disabled.
pub const DEFAULT_STEP_SECS: u64 = 0;
/// Default idle (no-output) hang detector: disabled.
pub const DEFAULT_IDLE_SECS: u64 = 0;
/// Default orchestrator wait budget: disabled (falls back to worker).
pub const DEFAULT_ORCHESTRATOR_SECS: u64 = 0;
/// Default grace between soft-cancel and kill, in seconds.
pub const DEFAULT_GRACE_SECS: u64 = 5;
/// Default progress-tick interval while waiting, in seconds.
pub const DEFAULT_PROGRESS_SECS: u64 = 15;
/// Default extra dispatches a harness may perform after the first terminal failure.
///
/// The supervisor itself never retries (`redispatch_count` is always `0`).
pub const DEFAULT_MAX_REDISPATCH_PER_ITEM: u32 = 1;

/// Why a worker was classified stuck (RM-15 / original GH#15).
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutClass {
    /// Elapsed wall-clock reached the worker, step, or orchestrator limit.
    Hard,
    /// Process still alive but no captured stdout/stderr for `idle`.
    Idle,
    /// Child PID/handle gone without a terminal status.
    LostChild,
    /// Harness redispatch budget exhausted (never emitted by `Supervisor::run`).
    RedispatchExhausted,
}

impl TimeoutClass {
    /// Watchlist / report residual token (`timeout:hard`, …).
    #[must_use]
    pub const fn residual_blocker(self) -> &'static str {
        match self {
            Self::Hard => "timeout:hard",
            Self::Idle => "timeout:idle",
            Self::LostChild => "timeout:lost_child",
            Self::RedispatchExhausted => "timeout:redispatch_exhausted",
        }
    }

    /// Timeouts are residuals, not successful fix attempts. They must not
    /// increment `fix_count` / `fix_cycles`.
    #[must_use]
    pub const fn counts_toward_fix_cap(self) -> bool {
        false
    }
}

/// Lifecycle step surfaced in progress ticks.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorStep {
    /// Waiting for a process-local max-parallel permit.
    PermitWait,
    /// Child is running.
    Running,
    /// Soft-cancel / kill in progress.
    Recovering,
    /// Reaping pipes after the child exited.
    Draining,
}

/// Snapshot emitted while the supervisor waits on the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// Currently held permits.
    pub active: usize,
    /// Configured process-local max-parallel.
    pub max_parallel: usize,
    /// Wall time since this `run` started (includes permit wait).
    pub elapsed: Duration,
    /// Time since the last captured stdout/stderr byte (or spawn).
    pub idle_for: Duration,
    /// Coarse phase of the wait.
    pub step: SupervisorStep,
}

impl ProgressSnapshot {
    /// Human one-liner for stderr diagnostics (not JSON stdout).
    #[must_use]
    pub fn format_line(&self) -> String {
        let step = match self.step {
            SupervisorStep::PermitWait => "permit_wait",
            SupervisorStep::Running => "running",
            SupervisorStep::Recovering => "recovering",
            SupervisorStep::Draining => "draining",
        };
        format!(
            "supervisor: active={}/{} elapsed={}s idle={}s step={step}",
            self.active,
            self.max_parallel,
            self.elapsed.as_secs(),
            self.idle_for.as_secs()
        )
    }
}

/// Per-run timeout and recovery knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutPolicy {
    /// Hard wall-clock for the worker, including permit wait. `None` = no cap.
    pub worker: Option<Duration>,
    /// Optional per-step cap; the child sees `min(remaining worker, step)`.
    pub step: Option<Duration>,
    /// Kill when no captured output arrives for this long. `None` = disabled.
    pub idle: Option<Duration>,
    /// Pool-level wait budget. `None` falls back to [`Self::worker`].
    pub orchestrator: Option<Duration>,
    /// Soft-cancel then kill delay. `ZERO` means SIGKILL immediately.
    pub grace: Duration,
    /// Harness retry budget. Supervisor never consumes this.
    pub max_redispatch_per_item: u32,
    /// Optional progress tick period. `None` = no ticks.
    pub progress_every: Option<Duration>,
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self {
            worker: None,
            step: None,
            idle: None,
            orchestrator: None,
            grace: Duration::from_secs(DEFAULT_GRACE_SECS),
            max_redispatch_per_item: DEFAULT_MAX_REDISPATCH_PER_ITEM,
            progress_every: Some(Duration::from_secs(DEFAULT_PROGRESS_SECS)),
        }
    }
}

impl TimeoutPolicy {
    /// Compatibility constructor for the original `run(..., timeout, ...)` API:
    /// wall-clock only, immediate kill, no idle, no progress ticks.
    #[must_use]
    pub fn from_worker_timeout(worker: Option<Duration>) -> Self {
        Self {
            worker,
            step: None,
            idle: None,
            orchestrator: None,
            grace: Duration::ZERO,
            max_redispatch_per_item: DEFAULT_MAX_REDISPATCH_PER_ITEM,
            progress_every: None,
        }
    }

    /// Permit-wait + overall budget: `min(orchestrator, worker)` when both set.
    #[must_use]
    pub fn wall_clock_limit(&self) -> Option<Duration> {
        match (self.orchestrator, self.worker) {
            (Some(orchestrator), Some(worker)) => Some(orchestrator.min(worker)),
            (Some(orchestrator), None) => Some(orchestrator),
            (None, Some(worker)) => Some(worker),
            (None, None) => None,
        }
    }

    /// Child-side cap after `elapsed` of the wall-clock budget has been spent.
    #[must_use]
    pub fn child_limit(&self, elapsed: Duration) -> Option<Duration> {
        let remaining_worker = self.wall_clock_limit().map(|limit| {
            if elapsed >= limit {
                Duration::ZERO
            } else {
                limit - elapsed
            }
        });
        match (remaining_worker, self.step) {
            (Some(worker), Some(step)) => Some(worker.min(step)),
            (Some(worker), None) => Some(worker),
            (None, Some(step)) => Some(step),
            (None, None) => None,
        }
    }
}

/// Whether a harness may dispatch again after `completed_attempts` terminal runs.
///
/// The original run is attempt 1. With the default budget of 1, a single
/// re-dispatch is allowed and a third run is not.
#[must_use]
pub fn can_redispatch(completed_attempts: u32, max_redispatch_per_item: u32) -> bool {
    completed_attempts < 1u32.saturating_add(max_redispatch_per_item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_defaults_match_documented_keys() {
        assert_eq!(DEFAULT_WORKER_SECS, 0);
        assert_eq!(DEFAULT_STEP_SECS, 0);
        assert_eq!(DEFAULT_IDLE_SECS, 0);
        assert_eq!(DEFAULT_ORCHESTRATOR_SECS, 0);
        assert_eq!(DEFAULT_GRACE_SECS, 5);
        assert_eq!(DEFAULT_PROGRESS_SECS, 15);
        assert_eq!(DEFAULT_MAX_REDISPATCH_PER_ITEM, 1);
        let policy = TimeoutPolicy::default();
        assert_eq!(policy.grace, Duration::from_secs(5));
        assert_eq!(policy.max_redispatch_per_item, 1);
        assert_eq!(
            policy.progress_every,
            Some(Duration::from_secs(DEFAULT_PROGRESS_SECS))
        );
    }

    #[test]
    fn redispatch_budget_allows_one_retry_by_default() {
        let max = DEFAULT_MAX_REDISPATCH_PER_ITEM;
        assert!(can_redispatch(1, max), "first failure may redispatch once");
        assert!(
            !can_redispatch(2, max),
            "second terminal run exhausts default budget"
        );
        assert!(!can_redispatch(3, max));
        assert!(!can_redispatch(1, 0), "max 0 means no retry");
        assert!(can_redispatch(2, 2));
        assert!(!can_redispatch(3, 2));
        assert!(
            can_redispatch(1, u32::MAX),
            "max budget must not overflow in 1 + max_redispatch"
        );
        assert!(!can_redispatch(u32::MAX, u32::MAX));
    }

    #[test]
    fn residual_tokens_and_fix_cap() {
        for class in [
            TimeoutClass::Hard,
            TimeoutClass::Idle,
            TimeoutClass::LostChild,
            TimeoutClass::RedispatchExhausted,
        ] {
            assert!(class.residual_blocker().starts_with("timeout:"));
            assert!(
                !class.counts_toward_fix_cap(),
                "timeout must not increment fix_count"
            );
        }
        assert_eq!(TimeoutClass::Hard.residual_blocker(), "timeout:hard");
        assert_eq!(TimeoutClass::Idle.residual_blocker(), "timeout:idle");
        assert_eq!(
            TimeoutClass::LostChild.residual_blocker(),
            "timeout:lost_child"
        );
        assert_eq!(
            TimeoutClass::RedispatchExhausted.residual_blocker(),
            "timeout:redispatch_exhausted"
        );
    }

    #[test]
    fn child_limit_is_min_of_remaining_worker_and_step() {
        let policy = TimeoutPolicy {
            worker: Some(Duration::from_secs(10)),
            step: Some(Duration::from_secs(3)),
            ..TimeoutPolicy::from_worker_timeout(None)
        };
        assert_eq!(
            policy.child_limit(Duration::from_secs(1)),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            policy.child_limit(Duration::from_secs(8)),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            policy.child_limit(Duration::from_secs(10)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn progress_line_includes_pool_and_step() {
        let snap = ProgressSnapshot {
            active: 1,
            max_parallel: 2,
            elapsed: Duration::from_secs(12),
            idle_for: Duration::from_secs(3),
            step: SupervisorStep::Running,
        };
        assert_eq!(
            snap.format_line(),
            "supervisor: active=1/2 elapsed=12s idle=3s step=running"
        );
    }
}
