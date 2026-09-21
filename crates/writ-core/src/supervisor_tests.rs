use std::path::PathBuf;
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
    assert_eq!(output.exit_code, Some(0), "stderr={}", output.stderr);
    assert!(!output.timed_out);
    assert_ne!(output.timeout_class, Some(TimeoutClass::Idle));
}

fn assert_permit_wait_timeout(output: &SupervisedOutput) {
    assert!(
        output.timed_out
            && !output.killed
            && output.timeout_class == Some(TimeoutClass::PermitWait)
            && output.recovery_stage == Some(RecoveryStage::None)
            && output.stderr.contains("max-parallel permit"),
        "permit-wait timeout mismatch: {output:?}"
    );
    let residual = output.residual.as_ref().expect("permit-wait residual");
    assert_eq!(residual.timeout_class, TimeoutClass::PermitWait);
    assert_eq!(residual.recovery_stage, RecoveryStage::None);
    assert!(!residual.redispatch_forbidden());
    let json = serde_json::to_value(output).unwrap();
    let obj = json.as_object().expect("object");
    for key in obj.keys() {
        let lower = key.to_ascii_lowercase();
        assert!(
            !lower.contains("sha") && !lower.contains("commit") && key != "head",
            "timeout residual must not carry identity field `{key}`"
        );
    }
}

fn assert_policy_code(err: Error, expected: PolicyCode) {
    assert!(matches!(
        err,
        Error::PolicyViolation { code, .. } if code == expected
    ));
}

fn assert_default_policy(program: &str, args: &[&str], expected: PolicyCode) {
    assert_policy_code(
        check_command_policy(program, args, &RunOptions::default()).unwrap_err(),
        expected,
    );
}

#[test]
fn normalize_strips_path_and_exe() {
    assert_eq!(normalize_program_name("/usr/bin/git"), "git");
    assert_eq!(normalize_program_name("C:\\Program Files\\git.exe"), "git");
    assert_eq!(normalize_program_name("./gh"), "gh");
    assert_eq!(normalize_program_name("GH.EXE"), "gh");
}

fn known_unsafe_policy_cases() -> &'static [(&'static str, &'static [&'static str], PolicyCode)] {
    &[
        (
            "/usr/bin/git",
            &["push", "--force"],
            PolicyCode::BareForcePush,
        ),
        ("gh", &["pr", "merge"], PolicyCode::MergeBlocked),
        ("./tools/run", &[], PolicyCode::SubcommandNotAllowed),
        (
            "sh",
            &["-c", "gh pr merge 1"],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "setsid",
            &["gh", "pr", "merge"],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "env",
            &["gh", "pr", "merge"],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "python3.11",
            &["-c", "print(1)"],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "python3",
            &[
                "-c",
                "import subprocess; subprocess.run(['gh','pr','merge','1'])",
            ],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "curl",
            &[
                "-X",
                "PUT",
                "https://api.github.com/repos/o/r/pulls/1/merge",
            ],
            PolicyCode::SubcommandNotAllowed,
        ),
        ("git", &["commit", "-m", "x"], PolicyCode::BranchMismatch),
    ]
}

#[test]
fn policy_blocks_known_unsafe_invocations() {
    for (program, args, expected) in known_unsafe_policy_cases() {
        assert_default_policy(program, args, *expected);
    }
}

/// Fixed path outside the default worktree sandbox (not `temp_dir`, which Codacy
/// flags for security-sensitive policy tests).
fn repo_outside_worktree_base_fixture() -> PathBuf {
    platform_pair(r"C:\Windows\Temp", "/tmp").into()
}

#[test]
fn mutating_git_rejects_repo_outside_default_worktree_base() {
    let repo = repo_outside_worktree_base_fixture();
    assert_policy_code(
        check_command_policy(
            "git",
            &["commit", "-m", "x"],
            &RunOptions {
                expected_branch: Some("feature".to_owned()),
                repo: Some(repo),
                ..RunOptions::default()
            },
        )
        .unwrap_err(),
        PolicyCode::PathNotAllowed,
    );
}

#[tokio::test]
async fn runs_command_to_completion() {
    let supervisor = Supervisor::new(4);
    let output = supervisor
        .run_unchecked(shell_program(), &[shell_flag(), "echo hello"], None)
        .await;

    assert_eq!(output.exit_code, Some(0), "stderr={}", output.stderr);
    assert!(!output.timed_out);
    assert!(!output.killed);
    assert!(output.succeeded());
    assert_eq!(output.stdout.trim(), "hello");
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

    assert_eq!(output.exit_code, Some(42), "stderr={}", output.stderr);
    assert!(!output.timed_out);
    assert!(!output.killed);
    assert_eq!(output.error_code, Some(SupervisorErrorCode::NonZeroExit));
    assert!(!output.succeeded());
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

    for (i, output) in results.iter().enumerate() {
        assert_eq!(
            output.exit_code,
            Some(0),
            "task {i} failed: stderr={}",
            output.stderr
        );
    }

    let peak = supervisor.peak_active();
    assert!(peak <= 2, "peak concurrency {peak} exceeded max_parallel=2");
    assert_eq!(
        peak, 2,
        "expected peak concurrency to reach 2 with 3 overlapping tasks"
    );
    assert_eq!(supervisor.active(), 0);
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
    assert!(output.spawn_failed(), "stderr={}", output.stderr);
    assert_eq!(output.error_code, Some(SupervisorErrorCode::SpawnFailed));
    assert!(output.exit_code.is_none());
}

#[tokio::test]
async fn run_rejects_merge_before_spawn() {
    let supervisor = Supervisor::new(1);
    let err = supervisor
        .run("gh", &["pr", "merge"], None, &RunOptions::default())
        .await
        .unwrap_err();
    assert_policy_code(err, PolicyCode::MergeBlocked);
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
    let supervisor = Supervisor::new(1);
    // The idle/worker margins are platform-specific because the two shells
    // produce output at different cadences. On Unix the script emits a line
    // every 150ms, so a tight 400ms idle timeout still proves that a steady
    // stream suppresses the idle detector. On Windows, cmd.exe treats `&` as
    // a *sequential* separator (not background), and `ping -n 2` waits ~1000ms
    // between its two requests, so there is a ~1s gap with no captured output
    // between `echo a` and `echo b`. A 400ms idle timeout would fire during
    // that gap, so Windows uses a 2500ms idle (comfortably above the ~1s gap)
    // and a 15s worker timeout (well above the ~1s total runtime). Both
    // platforms therefore verify the same property: steady output must not
    // trip the idle timeout, and the process runs to completion.
    #[cfg(windows)]
    let policy = TimeoutPolicy {
        idle: Some(Duration::from_millis(2500)),
        worker: Some(Duration::from_secs(15)),
        grace: Duration::ZERO,
        progress_every: None,
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    #[cfg(not(windows))]
    let policy = TimeoutPolicy {
        idle: Some(Duration::from_millis(400)),
        worker: Some(Duration::from_secs(5)),
        grace: Duration::ZERO,
        progress_every: None,
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    #[cfg(windows)]
    let script = "echo a & ping -n 2 127.0.0.1 >NUL & echo b";
    #[cfg(not(windows))]
    let script = "echo a; sleep 0.15; echo b; sleep 0.15; echo c";
    let output = supervisor
        .run_unchecked_with_policy(
            shell_program(),
            &[shell_flag(), script],
            &policy,
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
async fn recovery_still_rejects_merge_and_bare_force_push() {
    let supervisor = Supervisor::new(1);
    let policy = TimeoutPolicy {
        idle: Some(Duration::from_millis(50)),
        grace: Duration::from_millis(20),
        ..TimeoutPolicy::default()
    };
    let merge = supervisor
        .run_with_policy("gh", &["pr", "merge"], &policy, &RunOptions::default())
        .await
        .unwrap_err();
    assert_policy_code(merge, PolicyCode::MergeBlocked);
    let force = supervisor
        .run_with_policy("git", &["push", "--force"], &policy, &RunOptions::default())
        .await
        .unwrap_err();
    assert_policy_code(force, PolicyCode::BareForcePush);
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
            s.run_unchecked(
                shell_program(),
                &[shell_flag(), {
                    #[cfg(windows)]
                    {
                        "ping -n 8 127.0.0.1 >NUL"
                    }
                    #[cfg(not(windows))]
                    {
                        "sleep 4"
                    }
                }],
                None,
            )
            .await
        })
    };
    let armed = Instant::now();
    while supervisor.active() == 0 && armed.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(supervisor.active(), 1, "holder should own the only permit");

    #[cfg(windows)]
    let (program, args): (&str, &[&str]) = ("where.exe", &["where.exe"]);
    #[cfg(not(windows))]
    let (program, args): (&str, &[&str]) = ("true", &[]);
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
    assert!(
        queued.elapsed() < Duration::from_secs(2),
        "queued run should time out without waiting for the holder"
    );
    let _ = holder.await;
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
    assert!(output.timed_out, "stderr={}", output.stderr);
    assert_eq!(output.timeout_class, Some(TimeoutClass::Hard));
    assert_eq!(output.recovery_stage, Some(RecoveryStage::GracefulCancel));
    assert!(!output.killed, "SIGTERM reap should not report killed");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "SIGTERM during grace should reap without waiting the full sleep"
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
    assert!(policy.worker.is_none());
    let started = Instant::now();
    let output = run_hanging_with_policy(&policy, &RunOptions::default()).await;
    assert_timeout_outcome(
        &output,
        TimeoutClass::Idle,
        SupervisorErrorCode::IdleTimedOut,
    );
    assert_eq!(output.recovery_stage, Some(RecoveryStage::Kill));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_cancel_then_kill_when_term_ignored() {
    let policy = TimeoutPolicy {
        worker: Some(Duration::from_millis(150)),
        grace: Duration::from_millis(200),
        progress_every: None,
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    let started = Instant::now();
    let output = Supervisor::new(1)
        .run_unchecked_with_policy(
            shell_program(),
            &[shell_flag(), "trap '' TERM; sleep 60"],
            &policy,
            &RunOptions::default(),
        )
        .await;
    assert_timeout_outcome(&output, TimeoutClass::Hard, SupervisorErrorCode::TimedOut);
    assert_eq!(output.recovery_stage, Some(RecoveryStage::Kill));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "SIGKILL after ignored SIGTERM should recover promptly"
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
