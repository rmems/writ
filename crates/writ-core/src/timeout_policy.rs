//! Named timeout policy, stuck-class taxonomy, and redispatch budget.
//!
//! `writ` supervises **one process execution**. It does not retry, re-dispatch
//! workers, merge, or push. A timeout is a **recovery and handoff** event: it
//! must not delete a harness-owned checkout, erase WIP, or seize another
//! worker's assignment. Harness-level retry uses
//! [`DEFAULT_MAX_REDISPATCH_PER_ITEM`], [`can_redispatch`], and
//! [`RedispatchBudget`]. Operator policy: `docs/timeout-policy.md`.
//!
//! Env keys: [`ENV_TIMEOUT_SECS`], [`ENV_IDLE_SECS`] (stall alias
//! [`ENV_STALL_SECS`]), [`ENV_STEP_SECS`], [`ENV_ORCHESTRATOR_SECS`],
//! [`ENV_GRACE_SECS`], [`ENV_PROGRESS_SECS`], [`ENV_MAX_REDISPATCH`].

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

/// CLI / env: wall-clock timeout in seconds (`0` = unlimited).
pub const ENV_TIMEOUT_SECS: &str = "WRIT_SUPERVISOR_TIMEOUT_SECS";
/// CLI / env: idle (no-output) hang detector in seconds (`0` = disabled).
pub const ENV_IDLE_SECS: &str = "WRIT_SUPERVISOR_IDLE_SECS";
/// Alias of [`ENV_IDLE_SECS`] from the RM-129 comparison lane (`--stall`).
pub const ENV_STALL_SECS: &str = "WRIT_SUPERVISOR_STALL_SECS";
/// CLI / env: optional per-step cap in seconds (`0` = disabled).
pub const ENV_STEP_SECS: &str = "WRIT_SUPERVISOR_STEP_SECS";
/// CLI / env: pool-level wait budget in seconds (`0` = fall back to worker).
pub const ENV_ORCHESTRATOR_SECS: &str = "WRIT_SUPERVISOR_ORCHESTRATOR_SECS";
/// CLI / env: SIGTERM grace in seconds before SIGKILL.
pub const ENV_GRACE_SECS: &str = "WRIT_SUPERVISOR_GRACE_SECS";
/// CLI / env: progress tick interval in seconds (`0` = disabled).
pub const ENV_PROGRESS_SECS: &str = "WRIT_SUPERVISOR_PROGRESS_SECS";
/// CLI / env: host retry cap after a residual (`0` = never redispatch).
pub const ENV_MAX_REDISPATCH: &str = "WRIT_SUPERVISOR_MAX_REDISPATCH";

/// Why a worker was classified stuck (RM-15 / original GH#15).
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutClass {
    /// Elapsed wall-clock reached the worker, step, or orchestrator limit.
    Hard,
    /// Process still alive but no captured stdout/stderr for `idle`.
    Idle,
    /// Timed out while waiting for a process-local max-parallel permit (no child spawned).
    PermitWait,
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
            Self::PermitWait => "timeout:permit_wait",
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

/// Additive hang residual for `SupervisedOutput` / watchlist hosts.
///
/// Intentionally has **no** commit SHA, head, or identity fields: a timeout
/// is a handoff event and must never be posted as a fake verified-commit reply.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimeoutResidual {
    /// Stuck-detection class that fired.
    pub timeout_class: TimeoutClass,
    /// Last recovery action taken by the supervisor.
    pub recovery_stage: RecoveryStage,
    /// Always `0` from `writ supervisor run`; hosts must persist their own usage.
    pub redispatch_count: u32,
    /// Configured host cap (`max_redispatch_per_item`).
    pub max_redispatch_per_item: u32,
    /// Wall time from `run` start to residual, in milliseconds.
    pub elapsed_ms: u64,
    /// Milliseconds from spawn to the last captured child byte, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_output_ms: Option<u64>,
}

impl TimeoutResidual {
    /// True when the configured cap forbids any host retry (`max == 0`).
    ///
    /// Do **not** treat `redispatch_count` from a supervisor residual as consumed
    /// budget — that field is always `0` because writ does not retry.
    #[must_use]
    pub fn redispatch_forbidden(&self) -> bool {
        self.max_redispatch_per_item == 0
    }
}

/// Host-side cap that prevents infinite retry loops.
///
/// `writ supervisor run` does **not** consume this budget. A harness that
/// re-invokes the supervisor after a residual must call [`RedispatchBudget::try_acquire`]
/// once per retry.
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
        Self::new(DEFAULT_MAX_REDISPATCH_PER_ITEM)
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
    pub fn try_acquire(&mut self) -> std::result::Result<(), RedispatchExhausted> {
        if self.used >= self.max_per_item {
            return Err(RedispatchExhausted {
                max_per_item: self.max_per_item,
                used: self.used,
            });
        }
        self.used += 1;
        Ok(())
    }

    /// True when `max_per_item == 0` (hosts must not retry).
    #[must_use]
    pub fn redispatch_forbidden(&self) -> bool {
        self.max_per_item == 0
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
        let policy = TimeoutPolicy::default();
        assert_eq!(
            [
                DEFAULT_WORKER_SECS,
                DEFAULT_STEP_SECS,
                DEFAULT_IDLE_SECS,
                DEFAULT_ORCHESTRATOR_SECS
            ],
            [0, 0, 0, 0]
        );
        assert_eq!((DEFAULT_GRACE_SECS, DEFAULT_PROGRESS_SECS), (5, 15));
        assert_eq!(
            (
                policy.grace,
                policy.max_redispatch_per_item,
                policy.progress_every,
                DEFAULT_MAX_REDISPATCH_PER_ITEM
            ),
            (
                Duration::from_secs(5),
                1,
                Some(Duration::from_secs(DEFAULT_PROGRESS_SECS)),
                1
            )
        );
    }

    #[test]
    fn redispatch_budget_allows_one_retry_by_default() {
        let max = DEFAULT_MAX_REDISPATCH_PER_ITEM;
        assert_eq!(
            (
                can_redispatch(1, max),
                can_redispatch(2, max),
                can_redispatch(3, max)
            ),
            (true, false, false)
        );
        assert_eq!(
            (
                can_redispatch(1, 0),
                can_redispatch(2, 2),
                can_redispatch(3, 2)
            ),
            (false, true, false)
        );
        assert_eq!(
            (
                can_redispatch(1, u32::MAX),
                can_redispatch(u32::MAX, u32::MAX)
            ),
            (true, false)
        );
    }

    #[test]
    fn residual_tokens_and_fix_cap() {
        let classes = [
            (TimeoutClass::Hard, "timeout:hard"),
            (TimeoutClass::Idle, "timeout:idle"),
            (TimeoutClass::PermitWait, "timeout:permit_wait"),
            (TimeoutClass::LostChild, "timeout:lost_child"),
            (
                TimeoutClass::RedispatchExhausted,
                "timeout:redispatch_exhausted",
            ),
        ];
        for (class, token) in classes {
            assert_eq!(class.residual_blocker(), token);
        }
    }

    #[test]
    fn timeout_classes_do_not_count_toward_fix_cap() {
        assert!(!TimeoutClass::Hard.counts_toward_fix_cap());
        assert!(!TimeoutClass::Idle.counts_toward_fix_cap());
        assert!(!TimeoutClass::PermitWait.counts_toward_fix_cap());
    }

    #[test]
    fn lost_child_and_exhausted_skip_fix_cap() {
        assert!(!TimeoutClass::LostChild.counts_toward_fix_cap());
        assert!(!TimeoutClass::RedispatchExhausted.counts_toward_fix_cap());
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
        assert!(budget.redispatch_forbidden());
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn residual_has_no_fake_sha_fields() {
        let residual = TimeoutResidual {
            timeout_class: TimeoutClass::Hard,
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
        for stage in [
            RecoveryStage::None,
            RecoveryStage::GracefulCancel,
            RecoveryStage::Kill,
        ] {
            let token = serde_json::to_string(&stage).unwrap();
            assert!(!token.contains("merge"), "{token}");
            assert!(!token.contains("push"), "{token}");
        }
    }

    #[test]
    fn named_env_keys_are_stable() {
        let keys = [
            (ENV_TIMEOUT_SECS, "WRIT_SUPERVISOR_TIMEOUT_SECS"),
            (ENV_IDLE_SECS, "WRIT_SUPERVISOR_IDLE_SECS"),
            (ENV_STALL_SECS, "WRIT_SUPERVISOR_STALL_SECS"),
            (ENV_STEP_SECS, "WRIT_SUPERVISOR_STEP_SECS"),
            (ENV_ORCHESTRATOR_SECS, "WRIT_SUPERVISOR_ORCHESTRATOR_SECS"),
            (ENV_GRACE_SECS, "WRIT_SUPERVISOR_GRACE_SECS"),
            (ENV_PROGRESS_SECS, "WRIT_SUPERVISOR_PROGRESS_SECS"),
            (ENV_MAX_REDISPATCH, "WRIT_SUPERVISOR_MAX_REDISPATCH"),
        ];
        assert!(keys.iter().all(|(got, want)| got == want));
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
