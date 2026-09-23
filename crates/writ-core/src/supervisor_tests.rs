use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::timeout_policy::RecoveryStage;

fn platform_pair(windows: &'static str, unix: &'static str) -> &'static str {
    let pair = (windows, unix);
    if cfg!(windows) { pair.0 } else { pair.1 }
}

/// Portable shell invocation for tests (`sh -c` / `cmd /C`).
fn shell_program() -> &'static str {
    platform_pair("cmd", "sh")
}

fn shell_flag() -> &'static str {
    platform_pair("/C", "-c")
}

/// Assert the standard "supervisor killed the child on a timeout" outcome:
/// the run timed out, the child was killed, the timeout class and error code
/// match, and the supervisor never redispatched internally.
fn assert_timeout_outcome(
    output: &SupervisedOutput,
    expected_class: TimeoutClass,
    expected_code: SupervisorErrorCode,
) {
    assert!(
        output.timed_out
            && output.killed
            && output.timeout_class == Some(expected_class)
            && output.error_code == Some(expected_code)
            && output.redispatch_count == 0,
        "timeout outcome mismatch: {output:?}"
    );
}

fn hanging_script() -> &'static str {
    platform_pair("ping -n 60 127.0.0.1 >NUL", "sleep 60")
}

async fn run_hanging(timeout: Duration) -> SupervisedOutput {
    Supervisor::new(1)
        .run_unchecked(
            shell_program(),
            &[shell_flag(), hanging_script()],
            Some(timeout),
        )
        .await
}

async fn run_hanging_with_policy(policy: &TimeoutPolicy, options: &RunOptions) -> SupervisedOutput {
    Supervisor::new(1)
        .run_unchecked_with_policy(
            shell_program(),
            &[shell_flag(), hanging_script()],
            policy,
            options,
        )
        .await
}

fn assert_json_contains(json: &str, needles: &[&str]) {
    for needle in needles {
        assert!(json.contains(needle), "{json}");
    }
}

fn assert_completed_without_idle(output: &SupervisedOutput) {
    assert!(
        output.exit_code == Some(0)
            && !output.timed_out
            && output.timeout_class != Some(TimeoutClass::Idle),
        "expected completed non-idle run: {output:?}"
    );
}

fn assert_permit_wait_timeout(output: &SupervisedOutput) {
    assert_eq!(output.timeout_class, Some(TimeoutClass::PermitWait));
    assert_eq!(output.recovery_stage, Some(RecoveryStage::None));
    assert!(output.timed_out && !output.killed);
}

fn assert_permit_wait_residual(output: &SupervisedOutput) {
    let residual = output.residual.as_ref().expect("permit-wait residual");
    assert_eq!(residual.timeout_class, TimeoutClass::PermitWait);
    assert!(!residual.redispatch_forbidden());
    assert!(output.stderr.contains("max-parallel permit"));
}

fn occupy_script() -> &'static str {
    platform_pair("ping -n 8 127.0.0.1 >NUL", "sleep 4")
}

fn queued_true_cmd() -> (&'static str, &'static [&'static str]) {
    platform_pair_cmd("where.exe", &["where.exe"], "true", &[])
}

fn platform_pair_cmd(
    windows_prog: &'static str,
    windows_args: &'static [&'static str],
    unix_prog: &'static str,
    unix_args: &'static [&'static str],
) -> (&'static str, &'static [&'static str]) {
    if cfg!(windows) {
        (windows_prog, windows_args)
    } else {
        (unix_prog, unix_args)
    }
}

fn chatting_script() -> &'static str {
    platform_pair(
        "echo a & ping -n 2 127.0.0.1 >NUL & echo b",
        "echo a; sleep 0.15; echo b; sleep 0.15; echo c",
    )
}

fn chatting_idle_policy() -> TimeoutPolicy {
    #[cfg(windows)]
    {
        TimeoutPolicy {
            idle: Some(Duration::from_millis(2500)),
            worker: Some(Duration::from_secs(15)),
            grace: Duration::ZERO,
            progress_every: None,
            ..TimeoutPolicy::from_worker_timeout(None)
        }
    }
    #[cfg(not(windows))]
    {
        TimeoutPolicy {
            idle: Some(Duration::from_millis(400)),
            worker: Some(Duration::from_secs(5)),
            grace: Duration::ZERO,
            progress_every: None,
            ..TimeoutPolicy::from_worker_timeout(None)
        }
    }
}

#[tokio::test]
async fn runs_command_to_completion() {
    let supervisor = Supervisor::new(4);
    let output = supervisor
        .run_unchecked(shell_program(), &[shell_flag(), "echo hello"], None)
        .await;

    assert!(
        output.succeeded() && output.stdout.trim() == "hello",
        "expected echo hello: {output:?}"
    );
}

#[tokio::test]
async fn captures_stderr() {
    let supervisor = Supervisor::new(4);
    #[cfg(windows)]
    let script = "echo err 1>&2";
    #[cfg(not(windows))]
    let script = "echo err >&2";
    let output = supervisor
        .run_unchecked(shell_program(), &[shell_flag(), script], None)
        .await;

    assert_eq!(output.exit_code, Some(0), "stderr={}", output.stderr);
    assert_eq!(output.stderr.trim(), "err");
}

#[tokio::test]
async fn timeout_kills_process_group() {
    let output = run_hanging(Duration::from_millis(200)).await;
    assert_timeout_outcome(&output, TimeoutClass::Hard, SupervisorErrorCode::TimedOut);
    assert!(output.exit_code.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_remains_active_while_draining_inherited_pipes() {
    let supervisor = Supervisor::new(1);
    let started = Instant::now();
    let output = supervisor
        .run_unchecked(
            shell_program(),
            &[shell_flag(), "sleep 60 &"],
            Some(Duration::from_millis(200)),
        )
        .await;

    assert!(output.timed_out, "output={output:?}");
    assert!(output.killed, "output={output:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "supervisor hung while draining inherited pipes"
    );
}

#[tokio::test]
async fn propagates_nonzero_exit_code() {
    let supervisor = Supervisor::new(4);
    #[cfg(windows)]
    let script = "exit /B 42";
    #[cfg(not(windows))]
    let script = "exit 42";
    let output = supervisor
        .run_unchecked(shell_program(), &[shell_flag(), script], None)
        .await;

    assert!(
        output.exit_code == Some(42)
            && !output.timed_out
            && !output.killed
            && output.error_code == Some(SupervisorErrorCode::NonZeroExit)
            && !output.succeeded(),
        "expected exit 42: {output:?}"
    );
}

#[tokio::test]
async fn max_parallel_limits_concurrency() {
    let supervisor = Arc::new(Supervisor::new(2));

    #[cfg(windows)]
    let script = "ping -n 2 127.0.0.1 >NUL";
    #[cfg(not(windows))]
    let script = "sleep 0.4";

    let mut handles = Vec::new();
    for _ in 0..3 {
        let s = Arc::clone(&supervisor);
        handles.push(tokio::spawn(async move {
            s.run_unchecked(shell_program(), &[shell_flag(), script], None)
                .await
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join task"));
    }

    let all_ok = results.iter().all(|output| output.exit_code == Some(0));
    assert!(
        all_ok && supervisor.peak_active() == 2 && supervisor.active() == 0,
        "max-parallel mismatch peak={} active={} results={results:?}",
        supervisor.peak_active(),
        supervisor.active()
    );
}

#[tokio::test]
async fn serializes_to_json_with_all_fields() {
    let output = SupervisedOutput {
        exit_code: Some(0),
        stdout: "hello\n".to_string(),
        ..SupervisedOutput::default()
    };

    let json = serde_json::to_string(&output).unwrap();
    assert_json_contains(
        &json,
        &[
            "\"exit_code\":0",
            "\"timed_out\":false",
            "\"killed\":false",
            "\"stdout\":\"hello\\n\"",
            "\"stderr\":\"\"",
        ],
    );
}

#[tokio::test]
async fn timeout_output_serializes_correctly() {
    let output = run_hanging(Duration::from_millis(200)).await;
    assert_timeout_outcome(&output, TimeoutClass::Hard, SupervisorErrorCode::TimedOut);
    let json = serde_json::to_string(&output).unwrap();
    assert_json_contains(
        &json,
        &[
            "\"timed_out\":true",
            "\"killed\":true",
            "\"exit_code\":null",
            "TIMED_OUT",
            "\"timeout_class\":\"hard\"",
        ],
    );
}

#[tokio::test]
async fn spawn_failure_is_detectable() {
    let supervisor = Supervisor::new(1);
    let output = supervisor
        .run(
            "writ-nonexistent-binary-xyz",
            &[],
            None,
            &RunOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        output.spawn_failed()
            && output.error_code == Some(SupervisorErrorCode::SpawnFailed)
            && output.exit_code.is_none(),
        "expected spawn failure: {output:?}"
    );
}

#[tokio::test]
async fn idle_timeout_kills_silent_hang() {
    let policy = TimeoutPolicy {
        idle: Some(Duration::from_millis(200)),
        grace: Duration::ZERO,
        progress_every: None,
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    let started = Instant::now();
    let output = run_hanging_with_policy(&policy, &RunOptions::default()).await;
    assert_timeout_outcome(
        &output,
        TimeoutClass::Idle,
        SupervisorErrorCode::IdleTimedOut,
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "idle detector hung instead of recovering"
    );
}

#[tokio::test]
async fn idle_timeout_does_not_fire_when_output_keeps_arriving() {
    let output = Supervisor::new(1)
        .run_unchecked_with_policy(
            shell_program(),
            &[shell_flag(), chatting_script()],
            &chatting_idle_policy(),
            &RunOptions::default(),
        )
        .await;
    assert_completed_without_idle(&output);
}

#[tokio::test]
async fn progress_ticks_while_waiting() {
    let hits = Arc::new(AtomicUsize::new(0));
    let options = RunOptions {
        on_progress: Some(Arc::new({
            let hits = Arc::clone(&hits);
            move |_| {
                hits.fetch_add(1, Ordering::SeqCst);
            }
        })),
        ..RunOptions::default()
    };
    let policy = TimeoutPolicy {
        worker: Some(Duration::from_millis(350)),
        grace: Duration::ZERO,
        progress_every: Some(Duration::from_millis(50)),
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    let output = run_hanging_with_policy(&policy, &options).await;
    assert!(output.timed_out);
    let deadline = Instant::now() + Duration::from_secs(1);
    while hits.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "expected progress ticks while waiting"
    );
}

#[tokio::test]
async fn supervisor_does_not_redispatch_on_timeout() {
    let output = run_hanging(Duration::from_millis(150)).await;
    assert_timeout_outcome(&output, TimeoutClass::Hard, SupervisorErrorCode::TimedOut);
    assert!(!TimeoutClass::Hard.counts_toward_fix_cap());
}

#[tokio::test]
async fn wall_clock_includes_permit_wait() {
    let supervisor = Arc::new(Supervisor::new(1));
    let holder = {
        let s = Arc::clone(&supervisor);
        tokio::spawn(async move {
            s.run_unchecked(shell_program(), &[shell_flag(), occupy_script()], None)
                .await
        })
    };
    let armed = Instant::now();
    while supervisor.active() == 0 && armed.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (program, args) = queued_true_cmd();
    let queued = Instant::now();
    let output = supervisor
        .run(
            program,
            args,
            Some(Duration::from_millis(250)),
            &RunOptions::default(),
        )
        .await
        .unwrap();
    assert_permit_wait_timeout(&output);
    assert_permit_wait_residual(&output);
    assert!(
        supervisor.active() == 1 && queued.elapsed() < Duration::from_secs(2),
        "queued run should time out without waiting for the holder"
    );
    let _ = holder.await;
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_cancel_then_kill_when_term_ignored() {
    let policy = TimeoutPolicy {
        worker: Some(Duration::from_millis(200)),
        grace: Duration::from_millis(200),
        progress_every: None,
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    let output = Supervisor::new(1)
        .run_unchecked_with_policy(
            shell_program(),
            &[shell_flag(), "trap '' TERM; while true; do :; done"],
            &policy,
            &RunOptions::default(),
        )
        .await;
    assert_timeout_outcome(&output, TimeoutClass::Hard, SupervisorErrorCode::TimedOut);
    assert_eq!(output.recovery_stage, Some(RecoveryStage::Kill));
}

#[cfg(unix)]
#[tokio::test]
async fn grace_period_sends_term_before_kill() {
    let policy = TimeoutPolicy {
        worker: Some(Duration::from_millis(150)),
        grace: Duration::from_millis(800),
        progress_every: None,
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    let started = Instant::now();
    let output = Supervisor::new(1)
        .run_unchecked_with_policy(
            shell_program(),
            &[shell_flag(), "trap 'exit 0' TERM; sleep 60"],
            &policy,
            &RunOptions::default(),
        )
        .await;
    assert!(
        output.timed_out
            && output.timeout_class == Some(TimeoutClass::Hard)
            && output.recovery_stage == Some(RecoveryStage::GracefulCancel)
            && !output.killed
            && started.elapsed() < Duration::from_secs(3),
        "expected SIGTERM reap during grace: {output:?}"
    );
}

#[tokio::test]
async fn idle_detects_silent_child_without_wall_clock() {
    let policy = TimeoutPolicy {
        idle: Some(Duration::from_millis(200)),
        grace: Duration::ZERO,
        progress_every: None,
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    let started = Instant::now();
    let output = run_hanging_with_policy(&policy, &RunOptions::default()).await;
    assert_timeout_outcome(
        &output,
        TimeoutClass::Idle,
        SupervisorErrorCode::IdleTimedOut,
    );
    assert!(
        policy.worker.is_none()
            && output.recovery_stage == Some(RecoveryStage::Kill)
            && started.elapsed() < Duration::from_secs(5)
    );
}

#[tokio::test]
async fn hang_residual_has_no_fake_sha_fields() {
    let output = run_hanging(Duration::from_millis(150)).await;
    let json = serde_json::to_value(&output).unwrap();
    let residual = json.get("residual").expect("residual");
    let obj = residual.as_object().expect("object");
    for key in obj.keys() {
        let lower = key.to_ascii_lowercase();
        assert!(
            !lower.contains("sha") && !lower.contains("commit") && !lower.contains("head"),
            "timeout residual must not carry identity field `{key}`"
        );
    }
}
