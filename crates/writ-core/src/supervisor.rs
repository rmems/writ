//! Process supervisor with timeouts, process-group isolation, and max-parallel enforcement.
//!
//! Hang recovery lives here (RM-15 remaining supervisor contract): hard wall-clock,
//! idle (no-output) detection, lost-child classification, soft-cancel then kill,
//! and stderr progress ticks. The supervisor **never retries**, never merges, and
//! never force-pushes. Named defaults: [`crate::timeout_policy`].
//!
//! # Concurrency model
//!
//! `--max-parallel` / [`Supervisor::new`] limits concurrent supervised children **within a
//! single process**. Each `writ` CLI invocation constructs its own supervisor, so independent
//! processes do not share a global permit pool. Callers that need host-wide throttling must
//! coordinate externally (or share one long-lived `Supervisor` instance).
//!
//! # Platform notes
//!
//! On **Unix**, timeout recovery sends `SIGTERM` to the process group, waits
//! [`crate::timeout_policy::TimeoutPolicy::grace`], then `SIGKILL`
//! (`kill(-pid, SIGTERM|SIGKILL)` after spawning with `process_group(0)`).
//! `grace == 0` skips straight to `SIGKILL`. Descendants started by the child
//! are cleaned up with the supervised process.
//!
//! On **Windows**, there is no process-group SIGTERM. Recovery waits `grace`
//! then kills only the direct child (`kill_on_drop` + `child.kill()`).
//! There is **no kill-tree / job-object** yet: grandchild processes may outlive the
//! supervisor. Tracking full Windows job-object support is deferred (documented limitation).
//! Hosts whose subagent API cannot kill should stop waiting, mark a
//! `timeout:lost_child` residual, and warn the operator — they must not invent a SHA.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::Result;
use crate::owners::OwnerAllowlist;

#[path = "supervisor_cmd.rs"]
mod cmd;
use crate::timeout_policy::{
    ProgressCallback, ProgressSnapshot, RecoveryStage, SupervisorStep, TimeoutClass, TimeoutPolicy,
    TimeoutResidual,
};
pub use cmd::check_command_policy;
use cmd::{CommandRequest, prepare_supervised_command, verify_repo_branch};

/// Normalize an executable path to a basename without platform extensions.
#[must_use]
pub fn normalize_program_name(program: &str) -> String {
    // Accept both Unix and Windows separators even when running on Linux (tests / cross config).
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(program);
    let lower = base.to_ascii_lowercase();
    lower
        .strip_suffix(".exe")
        .or_else(|| lower.strip_suffix(".cmd"))
        .or_else(|| lower.strip_suffix(".bat"))
        .unwrap_or(&lower)
        .to_owned()
}

const FORBIDDEN_WRAPPERS: &[&str] = &[
    "bash",
    "bun",
    "chroot",
    "cmd",
    "curl",
    "dash",
    "deno",
    "doas",
    "env",
    "fish",
    "http",
    "httpie",
    "ipython",
    "ipython3",
    "lua",
    "nc",
    "ncat",
    "netcat",
    "nice",
    "node",
    "nodejs",
    "nohup",
    "nsenter",
    "open",
    "perl",
    "php",
    "powershell",
    "pwsh",
    "py",
    "python",
    "python2",
    "python3",
    "rscript",
    "ruby",
    "script",
    "setsid",
    "sh",
    "socat",
    "stdbuf",
    "su",
    "sudo",
    "time",
    "timeout",
    "unshare",
    "wget",
    "xargs",
    "xdg-open",
    "zsh",
];

const VERSIONED_WRAPPER_PREFIXES: &[&str] = &[
    "python", "python2", "python3", "perl", "ruby", "node", "nodejs", "php", "lua", "ipython",
];

pub(super) fn is_forbidden_wrapper(name: &str) -> bool {
    FORBIDDEN_WRAPPERS.contains(&name) || versioned_wrapper_name(name)
}

fn versioned_wrapper_name(name: &str) -> bool {
    VERSIONED_WRAPPER_PREFIXES.iter().any(|prefix| {
        let Some(rest) = name.strip_prefix(prefix) else {
            return false;
        };
        rest.is_empty()
            || rest.starts_with('.')
            || rest.chars().next().is_some_and(|c| c.is_ascii_digit())
    })
}

/// How long to wait for the child to exit after a timeout kill.
const POST_KILL_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound pipe drain when no wall-clock deadline is set, so inherited pipes from
/// background descendants cannot hang the supervisor forever.
const ORPHAN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap captured stdout/stderr per stream to avoid memory exhaustion.
const MAX_CAPTURE_BYTES: usize = 1_048_576;

/// Stable supervisor failure classifications (v1, additive on the wire).
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SupervisorErrorCode {
    /// Process could not be started.
    SpawnFailed,
    /// Waiting on the child failed after spawn. Prefer [`Self::LostChild`] for
    /// hang residuals; retained so v1 JSON stays additive.
    WaitFailed,
    /// Wall-clock timeout fired (process group / child kill attempted).
    TimedOut,
    /// Idle (no-output) hang detector fired; recovery kill attempted.
    IdleTimedOut,
    /// Child PID/handle gone without a terminal status.
    LostChild,
    /// Child terminated by signal / kill without a classified timeout.
    Killed,
    /// Child exited with a non-zero status.
    NonZeroExit,
}

/// Outcome of a supervised process execution.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SupervisedOutput {
    /// Process exit code, or `None` if terminated by signal / timeout / spawn failure.
    pub exit_code: Option<i32>,
    /// Whether the process was killed due to timeout.
    pub timed_out: bool,
    /// Whether the process was killed (by timeout or signal).
    pub killed: bool,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// True when stdout hit the capture cap.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stdout_truncated: bool,
    /// True when stderr hit the capture cap.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stderr_truncated: bool,
    /// Structured failure code when the run is not a clean success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<SupervisorErrorCode>,
    /// Stuck classification when the run timed out or the child was lost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_class: Option<TimeoutClass>,
    /// Wall-clock milliseconds from `run` start through this outcome (includes permit wait).
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Always `0` from this supervisor: it never re-dispatches.
    #[serde(default)]
    pub redispatch_count: u32,
    /// Configured host redispatch cap from the active [`TimeoutPolicy`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redispatch_per_item: Option<u32>,
    /// Last recovery action when a hang residual is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_stage: Option<RecoveryStage>,
    /// Milliseconds from spawn to last captured child byte, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_output_ms: Option<u64>,
    /// Additive hang residual (no SHA / commit / head fields).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub residual: Option<TimeoutResidual>,
}

impl SupervisedOutput {
    /// True when the supervisor failed to spawn the process.
    #[must_use]
    pub fn spawn_failed(&self) -> bool {
        self.error_code == Some(SupervisorErrorCode::SpawnFailed)
    }

    /// True when the supervised command completed successfully (exit 0, not timed out/killed).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.killed && self.error_code.is_none()
    }

    fn with_error(mut self, code: SupervisorErrorCode) -> Self {
        self.error_code = Some(code);
        self
    }

    fn with_elapsed(mut self, started: Instant) -> Self {
        self.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(residual) = &mut self.residual {
            residual.elapsed_ms = self.elapsed_ms;
        }
        self
    }

    fn with_hang_residual(
        mut self,
        policy: &TimeoutPolicy,
        class: TimeoutClass,
        stage: RecoveryStage,
        last_output_ms: Option<u64>,
        started: Instant,
    ) -> Self {
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.timeout_class = Some(class);
        self.recovery_stage = Some(stage);
        self.last_output_ms = last_output_ms;
        self.max_redispatch_per_item = Some(policy.max_redispatch_per_item);
        self.elapsed_ms = elapsed_ms;
        self.residual = Some(TimeoutResidual {
            timeout_class: class,
            recovery_stage: stage,
            redispatch_count: 0,
            max_redispatch_per_item: policy.max_redispatch_per_item,
            elapsed_ms,
            last_output_ms,
        });
        self
    }
}

/// Options for a policy-checked supervised run.
#[derive(Clone, Default)]
pub struct RunOptions {
    /// When supervising `git` mutations, require this branch (verified before spawn).
    pub expected_branch: Option<String>,
    /// Repository working tree for git branch verification and `git -C` (default: `.`).
    pub repo: Option<PathBuf>,
    /// Optional progress sink (stderr diagnostics). Never writes JSON stdout.
    pub on_progress: Option<ProgressCallback>,
    /// Explicit owner allowlist. `None` reads `WRIT_ALLOWED_OWNERS` / `WH_ALLOWED_OWNERS`.
    pub allowlist: Option<OwnerAllowlist>,
}

impl std::fmt::Debug for RunOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunOptions")
            .field("expected_branch", &self.expected_branch)
            .field("repo", &self.repo)
            .field("on_progress", &self.on_progress.is_some())
            .field("allowlist", &self.allowlist.is_some())
            .finish()
    }
}

/// Configuration for the process supervisor.
#[derive(Debug)]
pub struct Supervisor {
    max_parallel: usize,
    semaphore: Semaphore,
    /// Currently held permits (active supervised runs).
    active: AtomicUsize,
    /// High-water mark of concurrent supervised runs observed since construction.
    peak_active: AtomicUsize,
}

/// Guard that restores concurrency accounting if a supervised run is cancelled mid-flight.
struct ActiveGuard<'a> {
    supervisor: &'a Supervisor,
    armed: bool,
}

impl<'a> ActiveGuard<'a> {
    fn arm(supervisor: &'a Supervisor) -> Self {
        let current = supervisor.active.fetch_add(1, Ordering::SeqCst) + 1;
        supervisor.peak_active.fetch_max(current, Ordering::SeqCst);
        Self {
            supervisor,
            armed: true,
        }
    }

    fn defuse(mut self) {
        self.armed = false;
        self.supervisor.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.supervisor.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Supervisor {
    /// Create a new supervisor with the given max-parallel limit (per process).
    pub fn new(max_parallel: usize) -> Self {
        let permits = max_parallel.max(1);
        Self {
            max_parallel: permits,
            semaphore: Semaphore::new(permits),
            active: AtomicUsize::new(0),
            peak_active: AtomicUsize::new(0),
        }
    }

    /// Returns the configured max-parallel limit for this process-local supervisor.
    pub fn max_parallel(&self) -> usize {
        self.max_parallel
    }

    /// Peak concurrent supervised runs observed (for tests / diagnostics).
    pub fn peak_active(&self) -> usize {
        self.peak_active.load(Ordering::SeqCst)
    }

    /// Current concurrent supervised runs.
    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Run a command under supervision after applying safety policy.
    ///
    /// - Validates git/gh (including path-qualified names like `/usr/bin/git`) via
    ///   [`crate::git_safe::SafeGitCommand`] / [`crate::git_safe::SafeGhCommand`].
    /// - **Rejects shells and launchers** (`sh`, `bash`, `cmd`, `env`, `xargs`, …) so policy
    ///   cannot be bypassed by quoting or wrappers; invoke binaries directly.
    /// - For mutating git, requires [`RunOptions::expected_branch`], verifies the branch
    ///   **after** acquiring a permit (and immediately before spawn), and runs with
    ///   `current_dir` set to the verified repo.
    /// - Spawns with process-group isolation where the OS allows; Drop/timeout kill the group.
    /// - Enforces wall-clock timeout; kills the process group on expiry (Unix).
    /// - Acquires a permit from the **process-local** max-parallel semaphore before spawning.
    ///
    /// Equivalent to [`Self::run_with_policy`] with [`TimeoutPolicy::from_worker_timeout`]
    /// (immediate kill, no idle detector, no progress ticks).
    pub async fn run(
        &self,
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
        options: &RunOptions,
    ) -> Result<SupervisedOutput> {
        self.run_with_policy(
            program,
            args,
            &TimeoutPolicy::from_worker_timeout(timeout),
            options,
        )
        .await
    }

    /// Run a command under [`TimeoutPolicy`] (hard/idle/grace/progress).
    ///
    /// Recovery never merges, never bare-force-pushes, and never retries
    /// (`redispatch_count` is always `0`).
    pub async fn run_with_policy(
        &self,
        program: &str,
        args: &[&str],
        policy: &TimeoutPolicy,
        options: &RunOptions,
    ) -> Result<SupervisedOutput> {
        // Allowlist / structural validation only (no branch TOCTOU window before queue).
        let prepared = prepare_supervised_command(&CommandRequest {
            program,
            args,
            options,
        })?;
        let started = Instant::now();
        let _permit = match acquire_run_permit(self, policy, options, started).await {
            Ok(permit) => permit,
            Err(timeout) => return Ok(timeout),
        };
        let Some(child_policy) = child_policy_after_permit(policy, started.elapsed()) else {
            return Ok(permit_wait_timeout(started, policy));
        };

        // Branch check immediately before spawn, while holding the permit.
        // Always verify via git HEAD, for both supervised git and mutating gh.
        if let Some(ref check) = prepared.branch_check {
            verify_repo_branch(&check.repo, &check.expected_branch)?;
        }

        let guard = ActiveGuard::arm(self);
        let output = self
            .run_with_permit(
                &prepared.program,
                &prepared.args,
                prepared.cwd.as_deref(),
                &child_policy,
                options.on_progress.as_ref(),
                started,
            )
            .await;
        guard.defuse();
        Ok(output)
    }

    /// Low-level run **without** policy checks — **test-only** (not part of the public API).
    #[cfg(test)]
    async fn run_unchecked(
        &self,
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> SupervisedOutput {
        self.run_unchecked_with_policy(
            program,
            args,
            &TimeoutPolicy::from_worker_timeout(timeout),
            &RunOptions::default(),
        )
        .await
    }

    #[cfg(test)]
    async fn run_unchecked_with_policy(
        &self,
        program: &str,
        args: &[&str],
        policy: &TimeoutPolicy,
        options: &RunOptions,
    ) -> SupervisedOutput {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let started = Instant::now();
        let _permit = self
            .semaphore
            .acquire()
            .await
            .expect("supervisor semaphore closed");
        let guard = ActiveGuard::arm(self);
        let output = self
            .run_with_permit(
                program,
                &owned,
                None,
                policy,
                options.on_progress.as_ref(),
                started,
            )
            .await;
        guard.defuse();
        output
    }

    async fn run_with_permit(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&std::path::Path>,
        policy: &TimeoutPolicy,
        on_progress: Option<&ProgressCallback>,
        run_started: Instant,
    ) -> SupervisedOutput {
        let mut spawned = match spawn_supervised_child(program, args, cwd) {
            Ok(spawned) => spawned,
            Err(e) => {
                return SupervisedOutput {
                    stderr: format!("failed to spawn: {e}"),
                    error_code: Some(SupervisorErrorCode::SpawnFailed),
                    ..SupervisedOutput::default()
                }
                .with_elapsed(run_started);
            }
        };
        await_supervised_child(
            self,
            &mut spawned.child,
            spawned.stdout_handle,
            spawned.stderr_handle,
            ChildWait {
                policy,
                on_progress,
                run_started,
                spawn_at: spawned.spawn_at,
                last_activity_ms: spawned.last_activity_ms,
                pid: spawned.pid,
            },
        )
        .await
        .with_elapsed(run_started)
    }
}

struct SpawnedChild {
    child: ProcessGroupChild,
    pid: Option<u32>,
    spawn_at: Instant,
    last_activity_ms: Arc<AtomicU64>,
    stdout_handle: JoinHandle<Vec<u8>>,
    stderr_handle: JoinHandle<Vec<u8>>,
}

fn spawn_supervised_child(
    program: &str,
    args: &[String],
    cwd: Option<&std::path::Path>,
) -> std::io::Result<SpawnedChild> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    #[cfg(unix)]
    set_process_group(&mut cmd);
    let child = cmd.spawn()?;
    let pid = child.id();
    let mut child = ProcessGroupChild {
        child,
        pid,
        reaped: false,
    };
    let spawn_at = Instant::now();
    let last_activity_ms = Arc::new(AtomicU64::new(0));
    let mut child_stdout = child.child.stdout.take();
    let mut child_stderr = child.child.stderr.take();
    let stdout_last = Arc::clone(&last_activity_ms);
    let stderr_last = Arc::clone(&last_activity_ms);
    Ok(SpawnedChild {
        child,
        pid,
        spawn_at,
        last_activity_ms,
        stdout_handle: tokio::spawn(async move {
            read_pipe_probed(&mut child_stdout, spawn_at, stdout_last).await
        }),
        stderr_handle: tokio::spawn(async move {
            read_pipe_probed(&mut child_stderr, spawn_at, stderr_last).await
        }),
    })
}

async fn acquire_run_permit<'a>(
    supervisor: &'a Supervisor,
    policy: &TimeoutPolicy,
    options: &RunOptions,
    started: Instant,
) -> std::result::Result<tokio::sync::SemaphorePermit<'a>, SupervisedOutput> {
    match policy.wall_clock_limit() {
        Some(limit) => acquire_permit_with_progress(
            supervisor,
            limit,
            policy,
            options.on_progress.as_ref(),
            started,
        )
        .await
        .map_err(|()| permit_wait_timeout(started, policy)),
        None => Ok(supervisor
            .semaphore
            .acquire()
            .await
            .expect("supervisor semaphore closed")),
    }
}

fn child_policy_after_permit(policy: &TimeoutPolicy, elapsed: Duration) -> Option<TimeoutPolicy> {
    let child_timeout = policy.child_limit(elapsed);
    if child_timeout == Some(Duration::ZERO) {
        return None;
    }
    let mut child_policy = policy.clone();
    child_policy.worker = child_timeout;
    child_policy.orchestrator = None;
    // Step already applied inside child_limit; do not double-min.
    child_policy.step = None;
    Some(child_policy)
}

async fn acquire_permit_with_progress<'a>(
    supervisor: &'a Supervisor,
    limit: Duration,
    policy: &TimeoutPolicy,
    on_progress: Option<&ProgressCallback>,
    started: Instant,
) -> std::result::Result<tokio::sync::SemaphorePermit<'a>, ()> {
    let acquire = supervisor.semaphore.acquire();
    tokio::pin!(acquire);
    let deadline = started + limit;
    loop {
        let progress_at = on_progress
            .and(policy.progress_every)
            .map(|every| Instant::now() + every);
        tokio::select! {
            biased;
            result = &mut acquire => {
                return Ok(result.expect("supervisor semaphore closed"));
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(());
            }
            _ = sleep_until_opt(progress_at) => {
                emit_progress(
                    supervisor,
                    on_progress,
                    started,
                    Duration::ZERO,
                    SupervisorStep::PermitWait,
                );
            }
        }
    }
}

fn permit_wait_timeout(started: Instant, policy: &TimeoutPolicy) -> SupervisedOutput {
    SupervisedOutput {
        timed_out: true,
        stderr: "timed out waiting for max-parallel permit".to_owned(),
        error_code: Some(SupervisorErrorCode::TimedOut),
        ..SupervisedOutput::default()
    }
    .with_hang_residual(
        policy,
        TimeoutClass::PermitWait,
        RecoveryStage::None,
        None,
        started,
    )
}

fn emit_progress(
    supervisor: &Supervisor,
    on_progress: Option<&ProgressCallback>,
    started: Instant,
    idle_for: Duration,
    step: SupervisorStep,
) {
    let Some(cb) = on_progress else {
        return;
    };
    let snap = ProgressSnapshot {
        active: supervisor.active(),
        max_parallel: supervisor.max_parallel(),
        elapsed: started.elapsed(),
        idle_for,
        step,
    };
    let cb = Arc::clone(cb);
    // Host callbacks must not freeze hang recovery. Run them off the wait loop.
    tokio::task::spawn_blocking(move || cb(&snap));
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn idle_for_since(spawn_at: Instant, last_activity_ms: &AtomicU64) -> Duration {
    let last = last_activity_ms.load(Ordering::Relaxed);
    spawn_at
        .elapsed()
        .saturating_sub(Duration::from_millis(last))
}

struct ChildWait<'a> {
    policy: &'a TimeoutPolicy,
    on_progress: Option<&'a ProgressCallback>,
    run_started: Instant,
    spawn_at: Instant,
    last_activity_ms: Arc<AtomicU64>,
    pid: Option<u32>,
}

impl ChildWait<'_> {
    fn idle_for(&self) -> Duration {
        idle_for_since(self.spawn_at, &self.last_activity_ms)
    }

    fn idle_deadline(&self) -> Option<Instant> {
        self.policy.idle.map(|idle| {
            let last = self.last_activity_ms.load(Ordering::Relaxed);
            self.spawn_at + Duration::from_millis(last) + idle
        })
    }

    fn progress_deadline(&self) -> Option<Instant> {
        self.on_progress
            .and(self.policy.progress_every)
            .map(|every| Instant::now() + every)
    }

    fn idle_expired(&self) -> bool {
        self.policy.idle.is_some_and(|idle| self.idle_for() >= idle)
    }

    fn emit(&self, supervisor: &Supervisor, step: SupervisorStep) {
        emit_progress(
            supervisor,
            self.on_progress,
            self.run_started,
            self.idle_for(),
            step,
        );
    }
}

enum WaitTick {
    Recover(TimeoutClass),
    Progress,
    Continue,
    Finished(std::io::Result<std::process::ExitStatus>),
}

async fn next_wait_tick(
    child: &mut ProcessGroupChild,
    wait: &ChildWait<'_>,
    hard_deadline: Option<Instant>,
) -> WaitTick {
    tokio::select! {
        biased;
        _ = sleep_until_opt(hard_deadline) => WaitTick::Recover(TimeoutClass::Hard),
        _ = sleep_until_opt(wait.idle_deadline()) => {
            if wait.idle_expired() {
                WaitTick::Recover(TimeoutClass::Idle)
            } else {
                WaitTick::Continue
            }
        }
        _ = sleep_until_opt(wait.progress_deadline()) => WaitTick::Progress,
        status = child.wait() => WaitTick::Finished(status),
    }
}

async fn await_supervised_child(
    supervisor: &Supervisor,
    child: &mut ProcessGroupChild,
    stdout_handle: JoinHandle<Vec<u8>>,
    stderr_handle: JoinHandle<Vec<u8>>,
    wait: ChildWait<'_>,
) -> SupervisedOutput {
    let hard_deadline = wait.policy.worker.map(|limit| Instant::now() + limit);
    let pipes = PipePair {
        stdout: stdout_handle,
        stderr: stderr_handle,
    };
    loop {
        match next_wait_tick(child, &wait, hard_deadline).await {
            WaitTick::Recover(class) => {
                return recover_classified(
                    ChildFinish {
                        supervisor,
                        child,
                        pipes,
                        wait: &wait,
                    },
                    class,
                )
                .await;
            }
            WaitTick::Progress => wait.emit(supervisor, SupervisorStep::Running),
            WaitTick::Continue => {}
            WaitTick::Finished(status) => {
                if status.is_ok() {
                    child.disarm();
                }
                return complete_child_wait(
                    ChildFinish {
                        supervisor,
                        child,
                        pipes,
                        wait: &wait,
                    },
                    hard_deadline,
                    status,
                )
                .await;
            }
        }
    }
}

struct PipePair {
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
}

struct ChildFinish<'a> {
    supervisor: &'a Supervisor,
    child: &'a mut ProcessGroupChild,
    pipes: PipePair,
    wait: &'a ChildWait<'a>,
}

async fn recover_classified(finish: ChildFinish<'_>, class: TimeoutClass) -> SupervisedOutput {
    finish
        .wait
        .emit(finish.supervisor, SupervisorStep::Recovering);
    recover_child(
        finish.child,
        RecoverIo::from_wait(finish.pipes, finish.wait),
        class,
    )
    .await
}

async fn complete_child_wait(
    finish: ChildFinish<'_>,
    hard_deadline: Option<Instant>,
    status: std::io::Result<std::process::ExitStatus>,
) -> SupervisedOutput {
    match status {
        Ok(s) => {
            finish
                .wait
                .emit(finish.supervisor, SupervisorStep::Draining);
            drain_exited_child(
                s,
                hard_deadline,
                finish.wait.pid,
                finish.pipes,
                finish.wait.policy,
                finish.wait.run_started,
                &finish.wait.last_activity_ms,
            )
            .await
        }
        Err(e) => {
            finish
                .wait
                .emit(finish.supervisor, SupervisorStep::Recovering);
            recover_lost_child(
                finish.child,
                RecoverIo::from_wait(finish.pipes, finish.wait),
                &e,
            )
            .await
        }
    }
}

/// Drain a normally-exited child's output pipes with a bounded wait.
///
/// Drain is always capped: remaining wall-clock, or [`ORPHAN_DRAIN_TIMEOUT`].
/// If leftover group members hold the pipes, recovery kills that group and
/// records a hard residual. Captured bytes are kept. The harness checkout is
/// not deleted.
async fn drain_exited_child(
    status: std::process::ExitStatus,
    hard_deadline: Option<Instant>,
    pid: Option<u32>,
    pipes: PipePair,
    policy: &TimeoutPolicy,
    run_started: Instant,
    last_activity_ms: &AtomicU64,
) -> SupervisedOutput {
    let PipePair {
        stdout: stdout_handle,
        stderr: stderr_handle,
    } = pipes;
    let deadline_at = hard_deadline.unwrap_or_else(|| Instant::now() + ORPHAN_DRAIN_TIMEOUT);
    match drain_pipes_until(deadline_at, pid, stdout_handle, stderr_handle).await {
        Ok((stdout, stderr)) => output_to_supervised(status, &stdout, &stderr),
        Err((stdout, stderr)) => {
            // Direct child already exited; leftover group members held the pipes.
            // Kill is process containment only — the harness checkout is left intact.
            SupervisedOutput {
                timed_out: true,
                killed: true,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                stdout_truncated: stdout.len() >= MAX_CAPTURE_BYTES,
                stderr_truncated: stderr.len() >= MAX_CAPTURE_BYTES,
                error_code: Some(SupervisorErrorCode::TimedOut),
                ..SupervisedOutput::default()
            }
            .with_hang_residual(
                policy,
                TimeoutClass::Hard,
                RecoveryStage::Kill,
                nonzero_ms(last_activity_ms),
                run_started,
            )
        }
    }
}

/// Recover a child whose `wait()` failed (a lost/errored process), attaching the
/// process error to stderr when no captured stderr is available.
struct RecoverIo<'a> {
    stdout_handle: JoinHandle<Vec<u8>>,
    stderr_handle: JoinHandle<Vec<u8>>,
    policy: &'a TimeoutPolicy,
    pid: Option<u32>,
    run_started: Instant,
    last_activity_ms: Arc<AtomicU64>,
}

impl<'a> RecoverIo<'a> {
    fn from_wait(pipes: PipePair, wait: &'a ChildWait<'a>) -> Self {
        Self {
            stdout_handle: pipes.stdout,
            stderr_handle: pipes.stderr,
            policy: wait.policy,
            pid: wait.pid,
            run_started: wait.run_started,
            last_activity_ms: Arc::clone(&wait.last_activity_ms),
        }
    }
}

async fn recover_lost_child(
    child: &mut ProcessGroupChild,
    io: RecoverIo<'_>,
    error: &std::io::Error,
) -> SupervisedOutput {
    let mut recovered = recover_child(child, io, TimeoutClass::LostChild).await;
    if recovered.stderr.is_empty() {
        recovered.stderr = format!("process error: {error}");
    }
    recovered
}

async fn recover_child(
    child: &mut ProcessGroupChild,
    io: RecoverIo<'_>,
    class: TimeoutClass,
) -> SupervisedOutput {
    let RecoverIo {
        stdout_handle,
        stderr_handle,
        policy,
        pid,
        run_started,
        last_activity_ms,
    } = io;
    let error_code = match class {
        TimeoutClass::Idle => SupervisorErrorCode::IdleTimedOut,
        TimeoutClass::LostChild => SupervisorErrorCode::LostChild,
        TimeoutClass::Hard | TimeoutClass::PermitWait | TimeoutClass::RedispatchExhausted => {
            SupervisorErrorCode::TimedOut
        }
    };

    let stage = cancel_supervised_child(child, pid, policy.grace).await;
    child.disarm();

    let stdout = join_with_timeout(stdout_handle).await;
    let stderr = join_with_timeout(stderr_handle).await;
    let last_output_ms = nonzero_ms(&last_activity_ms);
    let timed_out = class != TimeoutClass::LostChild;
    let killed = stage == RecoveryStage::Kill;
    SupervisedOutput {
        timed_out,
        killed,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated: stdout.len() >= MAX_CAPTURE_BYTES,
        stderr_truncated: stderr.len() >= MAX_CAPTURE_BYTES,
        error_code: Some(error_code),
        ..SupervisedOutput::default()
    }
    .with_hang_residual(policy, class, stage, last_output_ms, run_started)
}

async fn cancel_supervised_child(
    child: &mut ProcessGroupChild,
    pid: Option<u32>,
    grace: Duration,
) -> RecoveryStage {
    if grace == Duration::ZERO {
        let _ = child.kill().await;
        let _ = tokio::time::timeout(POST_KILL_JOIN_TIMEOUT, child.wait()).await;
        return RecoveryStage::Kill;
    }
    terminate_process_group(pid);
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(_)) => {
            // Parent exited after SIGTERM; SIGKILL remaining group members
            // while the original pgid is still this pid, then disarm so Drop
            // cannot SIGKILL a reused PID.
            kill_process_group(pid);
            RecoveryStage::GracefulCancel
        }
        Ok(Err(_)) | Err(_) => {
            let _ = child.kill().await;
            let _ = tokio::time::timeout(POST_KILL_JOIN_TIMEOUT, child.wait()).await;
            RecoveryStage::Kill
        }
    }
}

/// Child that kills its Unix process group on Drop (in addition to tokio kill_on_drop).
struct ProcessGroupChild {
    child: tokio::process::Child,
    pid: Option<u32>,
    reaped: bool,
}

impl ProcessGroupChild {
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.child.wait().await?;
        self.disarm();
        Ok(status)
    }

    fn disarm(&mut self) {
        self.reaped = true;
        self.pid = None;
    }

    async fn kill(&mut self) -> std::io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        kill_process_group(self.pid);
        self.child.kill().await
    }
}

impl Drop for ProcessGroupChild {
    fn drop(&mut self) {
        if !self.reaped {
            kill_process_group(self.pid);
            let _ = self.child.start_kill();
        }
    }
}

fn nonzero_ms(stamp: &AtomicU64) -> Option<u64> {
    let v = stamp.load(Ordering::Relaxed);
    (v > 0).then_some(v)
}

async fn drain_pipes_until(
    deadline_at: Instant,
    pid: Option<u32>,
    stdout_handle: JoinHandle<Vec<u8>>,
    stderr_handle: JoinHandle<Vec<u8>>,
) -> std::result::Result<(Vec<u8>, Vec<u8>), (Vec<u8>, Vec<u8>)> {
    // Keep the original deadline active while draining: descendants that inherit
    // stdout/stderr (e.g. `sh -c 'sleep 60 &'`) must not hang the supervisor forever.
    let drain = async {
        let stdout = stdout_handle.await.unwrap_or_default();
        let stderr = stderr_handle.await.unwrap_or_default();
        (stdout, stderr)
    };
    tokio::pin!(drain);
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline_at) => {
            kill_process_group(pid);
            // Do not abort pipe readers: after the kill, pipes close and readers finish.
            // Preserve any bytes already captured (Codex: empty output on drain timeout).
            match tokio::time::timeout(POST_KILL_JOIN_TIMEOUT, &mut drain).await {
                Ok(out) => Err(out),
                Err(_) => Err((Vec::new(), Vec::new())),
            }
        }
        out = &mut drain => Ok(out),
    }
}

async fn join_with_timeout(handle: JoinHandle<Vec<u8>>) -> Vec<u8> {
    match tokio::time::timeout(POST_KILL_JOIN_TIMEOUT, handle).await {
        Ok(Ok(buf)) => buf,
        Ok(Err(_)) => Vec::new(),
        Err(_) => Vec::new(),
    }
}

async fn read_pipe_probed<R: AsyncReadExt + Unpin>(
    pipe: &mut Option<R>,
    spawn_at: Instant,
    last_activity_ms: Arc<AtomicU64>,
) -> Vec<u8> {
    match pipe.as_mut() {
        Some(reader) => {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut capped = false;
            loop {
                match reader.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let elapsed =
                            u64::try_from(spawn_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                        last_activity_ms.store(elapsed, Ordering::Relaxed);
                        if !capped {
                            let room = MAX_CAPTURE_BYTES.saturating_sub(buf.len());
                            if room > 0 {
                                buf.extend_from_slice(&chunk[..n.min(room)]);
                            }
                            if n > room || buf.len() >= MAX_CAPTURE_BYTES {
                                capped = true;
                            }
                        }
                        // When capped, keep draining so the child does not get SIGPIPE/EPIPE.
                    }
                    Err(_) => break,
                }
            }
            buf
        }
        None => Vec::new(),
    }
}

#[cfg(unix)]
fn set_process_group(cmd: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt as _;
    cmd.as_std_mut().process_group(0);
}

fn output_to_supervised(
    status: std::process::ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> SupervisedOutput {
    let exit_code = status.code();

    #[cfg(unix)]
    let killed = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().is_some()
    };
    #[cfg(not(unix))]
    let killed = false;

    let mut out = SupervisedOutput {
        exit_code,
        timed_out: false,
        killed,
        stdout: String::from_utf8_lossy(stdout).into_owned(),
        stderr: String::from_utf8_lossy(stderr).into_owned(),
        stdout_truncated: stdout.len() >= MAX_CAPTURE_BYTES,
        stderr_truncated: stderr.len() >= MAX_CAPTURE_BYTES,
        error_code: None,
        timeout_class: None,
        elapsed_ms: 0,
        redispatch_count: 0,
        max_redispatch_per_item: None,
        recovery_stage: None,
        last_output_ms: None,
        residual: None,
    };
    if killed {
        out = out.with_error(SupervisorErrorCode::Killed);
    } else if exit_code != Some(0) {
        out = out.with_error(SupervisorErrorCode::NonZeroExit);
    }
    out
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn signal_process_group(pid: Option<u32>, signal: i32) {
    if let Some(pid) = pid {
        // SAFETY: kill(2) is async-signal-safe and only sends a signal to the group.
        unsafe {
            libc::kill(-(pid as i32), signal);
        }
    }
}

#[cfg(unix)]
fn terminate_process_group(pid: Option<u32>) {
    signal_process_group(pid, libc::SIGTERM);
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: Option<u32>) {
    // Windows: no SIGTERM process-group. recover_child waits grace then child.kill().
}

#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    signal_process_group(pid, libc::SIGKILL);
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {
    // Windows: only the direct child is killed via kill_on_drop / child.kill().
    // Grandchildren are not reaped (no kill-tree / job object yet).
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "supervisor_policy_tests.rs"]
mod policy_tests;
