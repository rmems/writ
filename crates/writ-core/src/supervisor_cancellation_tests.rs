use std::sync::Arc;

use super::*;
#[cfg(any(unix, windows))]
use crate::timeout_policy::RecoveryStage;

#[cfg(unix)]
#[tokio::test]
async fn timeout_remains_active_while_draining_inherited_pipes() {
    let supervisor = Supervisor::new(1);
    let started = Instant::now();
    let output = supervisor
        .run_unchecked(
            "sh",
            &["-c", "sleep 60 &"],
            Some(Duration::from_millis(200)),
        )
        .await;

    assert!(
        output.timed_out && output.killed && started.elapsed() < Duration::from_secs(5),
        "supervisor hung or failed to kill while draining inherited pipes: {output:?}"
    );
}

#[cfg(unix)]
struct ProcessTreeFixture {
    pid_file: tempfile::NamedTempFile,
    supervisor: Arc<Supervisor>,
    run: tokio::task::JoinHandle<SupervisedOutput>,
    needs_cleanup: bool,
}

#[cfg(unix)]
impl ProcessTreeFixture {
    fn start(script: &'static str, policy: TimeoutPolicy, options: RunOptions) -> Self {
        let pid_file = tempfile::NamedTempFile::new().expect("PID fixture");
        let pid_path = pid_file
            .path()
            .to_str()
            .expect("UTF-8 fixture path")
            .to_owned();
        let supervisor = Arc::new(Supervisor::new(1));
        let run = tokio::spawn({
            let supervisor = Arc::clone(&supervisor);
            async move {
                supervisor
                    .run_unchecked_with_policy(
                        "sh",
                        &["-c", script, "sh", &pid_path],
                        &policy,
                        &options,
                    )
                    .await
            }
        });
        Self {
            pid_file,
            supervisor,
            run,
            needs_cleanup: true,
        }
    }

    fn recorded_pid(&self, index: usize) -> Option<String> {
        let value = std::fs::read_to_string(self.pid_file.path()).ok()?;
        value
            .split_whitespace()
            .nth(index)?
            .parse::<u32>()
            .ok()
            .map(|pid| pid.to_string())
    }

    async fn assert_descendant_stopped(&mut self, pid: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while descendant_is_running(pid) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !descendant_is_running(pid),
            "supervisor left descendant {pid} running"
        );
        self.needs_cleanup = false;
    }

    async fn assert_permit_released(&self) {
        assert_eq!(
            self.supervisor.active(),
            0,
            "cancelled run must release its permit"
        );
        let next = self
            .supervisor
            .run(
                "true",
                &[],
                Some(Duration::from_secs(1)),
                &RunOptions::default(),
            )
            .await
            .expect("successor run");
        assert!(
            next.succeeded(),
            "released permit must admit the next run: {next:?}"
        );
    }
}

#[cfg(unix)]
impl Drop for ProcessTreeFixture {
    fn drop(&mut self) {
        self.run.abort();
        if self.needs_cleanup
            && let Some(pid) = self.recorded_pid(0)
        {
            // Also clean up a deliberately surviving child on a failing baseline.
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid])
                .output();
        }
    }
}

#[cfg(unix)]
fn process_state(pid: &str) -> String {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .expect("inspect descendant state");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[cfg(unix)]
fn descendant_is_running(pid: &str) -> bool {
    let state = process_state(pid);
    // An orphan may briefly remain a zombie until the host init reaps it.
    !state.is_empty() && !state.starts_with('Z')
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_during_inherited_pipe_drain_stops_descendants() {
    let draining = Arc::new(tokio::sync::Notify::new());
    let options = RunOptions {
        on_progress: Some(Arc::new({
            let draining = Arc::clone(&draining);
            move |snapshot| {
                if snapshot.step == SupervisorStep::Draining {
                    draining.notify_one();
                }
            }
        })),
        ..RunOptions::default()
    };
    let mut fixture = ProcessTreeFixture::start(
        "sleep 60 & printf '%s %s\n' \"$!\" \"$$\" > \"$1\"",
        TimeoutPolicy::from_worker_timeout(None),
        options,
    );
    tokio::time::timeout(Duration::from_secs(5), draining.notified())
        .await
        .expect("direct child exited and inherited-pipe drain started");
    let pid = fixture.recorded_pid(0).expect("descendant PID recorded");
    assert!(
        descendant_is_running(&pid),
        "fixture child must still be alive"
    );
    let leader = fixture.recorded_pid(1).expect("group leader PID recorded");
    assert!(
        process_state(&leader).starts_with('Z'),
        "exited leader must remain unreaped to reserve its PID during drain"
    );

    fixture.run.abort();
    assert!(
        (&mut fixture.run)
            .await
            .expect_err("run was cancelled")
            .is_cancelled(),
        "run task should be aborted"
    );
    fixture.assert_descendant_stopped(&pid).await;
    fixture.assert_permit_released().await;
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_leader_exit_reports_kill_for_term_resistant_descendant() {
    let mut fixture = ProcessTreeFixture::start(
        r#"trap 'echo term-handled; exit 0' TERM
sh -c 'trap "" TERM; printf "%s %s\n" "$$" "$1" > "$2"; exec sleep 60' sh "$$" "$1" &
wait"#,
        TimeoutPolicy {
            worker: Some(Duration::from_secs(1)),
            grace: Duration::from_secs(1),
            progress_every: None,
            ..TimeoutPolicy::from_worker_timeout(None)
        },
        RunOptions::default(),
    );
    let output = tokio::time::timeout(Duration::from_secs(5), &mut fixture.run)
        .await
        .expect("bounded recovery")
        .expect("supervised run");
    let pid = fixture
        .recorded_pid(0)
        .expect("TERM-resistant descendant recorded");
    fixture.assert_descendant_stopped(&pid).await;
    assert_eq!(
        output.stdout.trim(),
        "term-handled",
        "leader must exit during grace"
    );
    assert_eq!(
        output.recovery_stage,
        Some(RecoveryStage::Kill),
        "SIGKILL containment must be reported even when the leader exits in grace: {output:?}"
    );
    assert!(
        output.timed_out && output.killed && output.timeout_class == Some(TimeoutClass::Hard),
        "group kill must be visible in the recovery outcome: {output:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn signal_fallback_observes_exit_without_reaping_the_leader() {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new("sh")
        .args(["-c", "read line; exit 23"])
        .stdin(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn exit fixture");
    let observation = wait_for_child_signal(child.id());
    tokio::pin!(observation);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut observation)
            .await
            .is_err(),
        "fallback must keep waiting while the child is alive"
    );
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"exit\n")
        .await
        .expect("release child exit");
    tokio::time::timeout(Duration::from_secs(2), &mut observation)
        .await
        .expect("fallback must observe exit even when SIGCHLD is masked")
        .expect("observe child exit");
    // Reaping here must still succeed and preserve the original exit status.
    assert_eq!(
        child.wait().await.expect("reap observed child").code(),
        Some(23)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn observing_a_reaped_child_releases_group_ownership() {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "exit 23"]).kill_on_drop(true);
    set_process_group(&mut command);
    let child = command.spawn().expect("spawn ownership fixture");
    let mut child = ProcessGroupChild {
        pid: child.id(),
        child,
        reaped: false,
    };
    // Consume the real OS status without going through the group's wait wrapper.
    // Tokio retains the status, so its own kill-on-drop guard is safely disarmed.
    child.child.wait().await.expect("reap before observing");
    let observed = tokio::time::timeout(Duration::from_secs(2), child.wait_for_exit()).await;
    let retained_pid = child.pid;
    let reaped = child.reaped;
    // Keep failure cleanup safe even if ownership-release logic regresses.
    child.disarm();

    let error = observed
        .expect("lost ownership must not wait for an unrelated process")
        .expect_err("reaped status is no longer observable");
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    assert!(
        reaped && retained_pid.is_none(),
        "lost-child observation must prevent later group signalling"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exit_observation_without_child_identity_fails_closed() {
    let error = tokio::time::timeout(Duration::from_secs(1), wait_for_unreaped_exit(None))
        .await
        .expect("missing identity must not wait for any child")
        .expect_err("missing identity cannot prove an owned child exited");
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
}

#[cfg(windows)]
#[tokio::test]
async fn direct_child_exit_during_grace_reports_no_kill() {
    let policy = TimeoutPolicy {
        worker: Some(Duration::from_millis(150)),
        grace: Duration::from_secs(5),
        progress_every: None,
        ..TimeoutPolicy::from_worker_timeout(None)
    };
    let output = Supervisor::new(1)
        .run_unchecked_with_policy(
            "cmd",
            &["/C", "ping -n 2 127.0.0.1 >NUL & exit /B 0"],
            &policy,
            &RunOptions::default(),
        )
        .await;
    assert!(output.timed_out && !output.killed, "{output:?}");
    assert_eq!(output.recovery_stage, Some(RecoveryStage::GracefulCancel));
}

#[tokio::test]
async fn cancelled_drain_closes_both_inherited_pipe_readers() {
    use tokio::io::AsyncWriteExt;

    let (stdout_reader, mut stdout_writer) = tokio::io::duplex(64);
    let (stderr_reader, mut stderr_writer) = tokio::io::duplex(64);
    let activity = Arc::new(AtomicU64::new(0));
    let spawn_at = Instant::now();
    let pipes = PipePair {
        stdout: tokio::spawn({
            let activity = Arc::clone(&activity);
            async move { read_pipe_probed(&mut Some(stdout_reader), spawn_at, activity).await }
        }),
        stderr: tokio::spawn(async move {
            read_pipe_probed(&mut Some(stderr_reader), spawn_at, activity).await
        }),
    };
    let drain = tokio::spawn(drain_pipes_until(
        Instant::now() + Duration::from_secs(60),
        None,
        pipes,
    ));
    tokio::task::yield_now().await;
    drain.abort();
    assert!(drain.await.expect_err("drain cancelled").is_cancelled());
    for writer in [&mut stdout_writer, &mut stderr_writer] {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if writer.write_all(b"still open").await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelling drain must close inherited pipe readers");
    }
}

/// Both paths contain the Unix group; a TERM handler still gets its grace window.
#[cfg(unix)]
#[tokio::test]
async fn graceful_cancel_follows_term_sensitivity() {
    let cases = [
        ("trap '' TERM; while true; do :; done", ""),
        (
            "trap 'echo term-handled; exit 0' TERM; sleep 60",
            "term-handled",
        ),
    ];
    for (script, expected_stdout) in cases {
        let policy = TimeoutPolicy {
            worker: Some(Duration::from_millis(200)),
            grace: Duration::from_millis(800),
            progress_every: None,
            ..TimeoutPolicy::from_worker_timeout(None)
        };
        let started = Instant::now();
        let supervisor = Supervisor::new(1);
        let output = tokio::time::timeout(
            Duration::from_secs(3),
            supervisor.run_unchecked_with_policy(
                "sh",
                &["-c", script],
                &policy,
                &RunOptions::default(),
            ),
        )
        .await
        .expect("TERM recovery must finish within three seconds");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "TERM recovery exceeded its original completion bound for {script}"
        );
        assert_eq!(
            output.stdout.trim(),
            expected_stdout,
            "TERM response for {script}"
        );
        assert!(
            output.timed_out
                && output.killed
                && output.timeout_class == Some(TimeoutClass::Hard)
                && output.recovery_stage == Some(RecoveryStage::Kill)
                && output.error_code == Some(SupervisorErrorCode::TimedOut)
                && output.redispatch_count == 0,
            "group containment mismatch for {script}: {output:?}"
        );
    }
}
