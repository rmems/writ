//! Claude Code hook dispatcher.
//!
//! Reads hook JSON on stdin. Exit 0 allows; exit 2 blocks and writes a reason
//! on stderr that Claude Code shows to the model. The harness owns worktree
//! creation and removal: `WorktreeCreate`/`WorktreeRemove` here only update
//! coordination records and never create, move, or delete a checkout.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::bash_argv::{GitGhTool, ShellText, git_gh_invocations};
use crate::checkout::CheckoutRegistry;
use crate::error::{Error, PolicyCode, Result};
use crate::git_safe::{SafeGhCommand, SafeGitCommand};
use crate::lease::{AgentIdentity, LeaseStore};
use crate::owners::OwnerAllowlist;

/// Process-local paths for hook dispatch (tests inject temp dirs).
#[derive(Debug, Clone, Default)]
pub struct HookRuntime {
    pub worktree_base: Option<PathBuf>,
    pub lease_path: Option<PathBuf>,
    /// Optional explicit allowlist for tests; production uses `from_env`.
    pub allowed_owners: Option<OwnerAllowlist>,
}

/// Dispatch one Claude Code hook event. Returns the process exit code.
pub fn dispatch(
    input: &str,
    runtime: &HookRuntime,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    match dispatch_inner(HookJson(input), runtime, stdout) {
        Ok(()) => 0,
        Err(err) => {
            let _ = writeln!(stderr, "writ hook: [{}] {err}", err.code());
            2
        }
    }
}

struct HookJson<'a>(&'a str);

fn dispatch_inner(
    input: HookJson<'_>,
    runtime: &HookRuntime,
    stdout: &mut impl Write,
) -> Result<()> {
    let event: HookEvent = serde_json::from_str(input.0).map_err(|e| Error::Io {
        context: "parse hook JSON",
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;
    match event.hook_event_name.as_str() {
        "PreToolUse" => handle_pre_tool_use(&event),
        "WorktreeCreate" => handle_worktree_create(&event, runtime, stdout),
        "WorktreeRemove" => handle_worktree_remove(&event, runtime),
        "SubagentStart" => handle_subagent_start(&event, runtime),
        "SubagentStop" => handle_subagent_stop(&event, runtime),
        _ => Ok(()),
    }
}

#[derive(Debug, Deserialize)]
struct HookEvent {
    hook_event_name: String,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<ToolInput>,
    #[serde(default, alias = "worktree_name")]
    name: Option<String>,
    #[serde(default)]
    worktree_path: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolInput {
    #[serde(default)]
    command: Option<String>,
}

fn handle_pre_tool_use(event: &HookEvent) -> Result<()> {
    let tool_name = event.tool_name.as_deref().unwrap_or("");
    if !tool_name.eq_ignore_ascii_case("Bash") {
        return Ok(());
    }
    let Some(command) = event
        .tool_input
        .as_ref()
        .and_then(|input| input.command.as_deref())
    else {
        return Ok(());
    };
    admit_bash_command(ShellText(command))
}

/// Coordination-only `WorktreeCreate`: the harness performs the actual
/// creation. When the event names an already-existing checkout, register it
/// and echo the canonical path on stdout; otherwise allow native creation to
/// proceed untouched (exit 0, no claimed path).
fn handle_worktree_create(
    event: &HookEvent,
    runtime: &HookRuntime,
    stdout: &mut impl Write,
) -> Result<()> {
    let Some(path) = event
        .worktree_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Path::new)
    else {
        return Ok(());
    };
    if !path.exists() {
        return Ok(());
    }
    let job_id = event
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "checkout".to_owned());
    match open_registry(runtime)?.register(path, &job_id) {
        Ok(info) => writeln!(stdout, "{}", info.path.display()).map_err(|e| Error::Io {
            context: "write WorktreeCreate path",
            source: e,
        }),
        // A path that exists but is not a git checkout is not ours to judge;
        // let the harness continue without a registered record.
        Err(
            err @ Error::PolicyViolation {
                code: PolicyCode::GitDirUnavailable,
                ..
            },
        ) => {
            let _ = err;
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Coordination-only `WorktreeRemove`: release the lease row for the path,
/// never delete the checkout or its branch. A missing `worktree_path` is a
/// no-op rather than a block.
fn handle_worktree_remove(event: &HookEvent, runtime: &HookRuntime) -> Result<()> {
    let Some(path) = event
        .worktree_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    open_store(runtime)?.release_by_path(Path::new(path))?;
    Ok(())
}

fn handle_subagent_start(event: &HookEvent, runtime: &HookRuntime) -> Result<()> {
    let agent_id = event.agent_id.as_deref().unwrap_or("unknown");
    let agent_type = event.agent_type.as_deref().unwrap_or("unknown");
    open_store(runtime)?.upsert_agent(AgentIdentity {
        agent_id,
        agent_type,
        session_id: event.session_id.as_deref(),
    })
}

fn handle_subagent_stop(event: &HookEvent, runtime: &HookRuntime) -> Result<()> {
    let agent_id = event.agent_id.as_deref().unwrap_or("unknown");
    open_store(runtime)?.retire_agent(agent_id)
}

fn open_registry(runtime: &HookRuntime) -> Result<CheckoutRegistry> {
    CheckoutRegistry::with_store(open_store(runtime)?)
}

fn open_store(runtime: &HookRuntime) -> Result<LeaseStore> {
    match &runtime.lease_path {
        Some(path) => LeaseStore::open(path),
        None => LeaseStore::open(crate::paths::lease_store_path()),
    }
}

fn admit_bash_command(command: ShellText<'_>) -> Result<()> {
    let invocations = git_gh_invocations(command).map_err(|unparsed| Error::PolicyViolation {
        code: PolicyCode::SubcommandNotAllowed,
        message: format!("unparseable git/gh command: {}", unparsed.0.0),
    })?;
    for invocation in invocations {
        match invocation.tool {
            GitGhTool::Git => {
                SafeGitCommand::new(&invocation.args)?;
            }
            GitGhTool::Gh => {
                SafeGhCommand::new(&invocation.args)?;
            }
        }
    }
    Ok(())
}

/// Public validation entry used by tests: policy-check a Bash command string.
pub fn validate_bash_command(command: &str) -> Result<()> {
    admit_bash_command(ShellText(command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn assert_bash_hook_blocks(command: &str, needle: &str) {
        let runtime = HookRuntime::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let code = dispatch(&payload, &runtime, &mut stdout, &mut stderr);
        let stderr = String::from_utf8_lossy(&stderr);
        assert_eq!(code, 2, "{command}: {stderr}");
        assert!(stderr.contains(needle), "{command}: {stderr}");
    }

    #[test]
    fn pre_tool_use_blocks_git_and_gh_policy_violations() {
        assert_bash_hook_blocks("git push --force", "BARE_FORCE_PUSH");
        assert_bash_hook_blocks("FOO=bar git merge feature", "MERGE_BLOCKED");
        assert_bash_hook_blocks("gh pr merge 1", "MERGE_BLOCKED");
        assert_bash_hook_blocks("gh api repos/acme/example", "GH_SUBCOMMAND_NOT_ALLOWED");
    }

    #[test]
    fn pre_tool_use_allows_safe_git_and_ignores_non_git() {
        validate_bash_command("git status").unwrap();
        validate_bash_command("git push --force-with-lease origin HEAD").unwrap();
        validate_bash_command("npm test").unwrap();
        validate_bash_command("/usr/bin/git status").unwrap();
        validate_bash_command("git -C /tmp/repo status").unwrap();
        validate_bash_command("git -- status").unwrap();
        validate_bash_command("git commit -m 'a > b'").unwrap();
    }

    #[test]
    fn pre_tool_use_blocks_quoted_force_flags() {
        for command in [
            r#"git push --for"ce""#,
            r#"git push "--force""#,
            r#"git push -"f""#,
        ] {
            let err = validate_bash_command(command).unwrap_err();
            assert!(
                matches!(
                    err,
                    Error::PolicyViolation {
                        code: PolicyCode::BareForcePush,
                        ..
                    }
                ),
                "{command}: {err:?}"
            );
        }
    }

    #[test]
    fn pre_tool_use_blocks_git_config_overrides() {
        for command in [
            "git -c alias.status='!git push --force' status",
            "git --config-env alias.status=FOO status",
            "git --config-env=alias.status=FOO status",
            "git -C /tmp/repo -c core.hooksPath=/tmp/hooks status",
        ] {
            let err = validate_bash_command(command).unwrap_err();
            assert!(
                matches!(
                    err,
                    Error::PolicyViolation {
                        code: PolicyCode::SubcommandNotAllowed,
                        ..
                    }
                ),
                "{command}: {err:?}"
            );
        }
    }

    #[test]
    fn pre_tool_use_blocks_redirection_and_git_function_def() {
        let redir = validate_bash_command("git status > /tmp/out").unwrap_err();
        assert!(matches!(
            redir,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
        let shadowed = validate_bash_command("git() { :; }; git status").unwrap_err();
        assert!(matches!(
            shadowed,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
        let expanded = validate_bash_command("g${x-}it push --force").unwrap_err();
        assert!(matches!(
            expanded,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn compound_command_still_blocks_force_push() {
        let err = validate_bash_command("npm test && git push --force").unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn worktree_create_without_existing_checkout_is_a_noop_allow() {
        let runtime = HookRuntime::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = dispatch(
            r#"{"hook_event_name":"WorktreeCreate","cwd":"/tmp","name":"job-1"}"#,
            &runtime,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "no checkout may be claimed or created");

        let mut stderr = Vec::new();
        let code = dispatch(
            r#"{"hook_event_name":"WorktreeCreate","worktree_path":"/nonexistent/job-1","name":"job-1"}"#,
            &runtime,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
    }

    fn git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = crate::git_cmd::git_in(repo, args).unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn two_commit_repo(repo: &Path) -> (String, String) {
        git_stdout(repo, &["init", "-b", "main"]);
        git_stdout(repo, &["config", "user.email", "test@example.com"]);
        git_stdout(repo, &["config", "user.name", "Test"]);
        fs::write(repo.join("README"), "one\n").unwrap();
        git_stdout(repo, &["add", "README"]);
        git_stdout(repo, &["commit", "-m", "first"]);
        let first = git_stdout(repo, &["rev-parse", "HEAD"]);
        fs::write(repo.join("README"), "two\n").unwrap();
        git_stdout(repo, &["add", "README"]);
        git_stdout(repo, &["commit", "-m", "second"]);
        let second = git_stdout(repo, &["rev-parse", "HEAD"]);
        git_stdout(
            repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/test-repo.git",
            ],
        );
        (first, second)
    }

    #[test]
    fn worktree_create_registers_existing_checkout_without_mutating_it() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let (first, second) = two_commit_repo(&repo);
        assert_ne!(first, second);
        let checkout = temp.path().join("harness-checkout");
        fs::create_dir(&checkout).unwrap();
        git_stdout(&checkout, &["clone", repo.to_str().unwrap(), "."]);
        fs::write(checkout.join("wip.txt"), "keep\n").unwrap();
        let head_before = git_stdout(&checkout, &["rev-parse", "HEAD"]);

        let lease_path = temp.path().join("leases.db");
        let runtime = HookRuntime {
            worktree_base: None,
            lease_path: Some(lease_path.clone()),
            allowed_owners: None,
        };
        let payload = serde_json::json!({
            "hook_event_name": "WorktreeCreate",
            "worktree_path": checkout,
            "worktree_name": "job-src",
        })
        .to_string();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = dispatch(&payload, &runtime, &mut stdout, &mut stderr);
        assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
        assert_eq!(
            String::from_utf8_lossy(&stdout).trim(),
            crate::paths::canonicalize_for_tools(&checkout)
                .unwrap()
                .display()
                .to_string()
        );
        assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), head_before);
        assert!(checkout.join("wip.txt").exists());

        let store = LeaseStore::open(&lease_path).unwrap();
        assert_eq!(store.list_active().unwrap().len(), 1);
    }

    #[test]
    fn worktree_remove_releases_lease_without_deleting_checkout() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let _ = two_commit_repo(&repo);
        let checkout = temp.path().join("harness-checkout");
        fs::create_dir(&checkout).unwrap();
        git_stdout(&checkout, &["clone", repo.to_str().unwrap(), "."]);

        let lease_path = temp.path().join("leases.db");
        let runtime = HookRuntime {
            worktree_base: None,
            lease_path: Some(lease_path.clone()),
            allowed_owners: None,
        };
        let canonical = crate::paths::canonicalize_for_tools(&checkout).unwrap();
        open_store(&runtime)
            .unwrap()
            .grant(crate::lease::LeaseGrant {
                repo: &repo,
                owner: "acme",
                repo_name: "test-repo",
                job_id: "job-rm",
                branch: "job/rm",
                worktree_path: &canonical,
                start_commit: "abc",
            })
            .unwrap();

        let payload = serde_json::json!({
            "hook_event_name": "WorktreeRemove",
            "worktree_path": canonical,
        })
        .to_string();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = dispatch(&payload, &runtime, &mut stdout, &mut stderr);
        assert_eq!(code, 0, "stderr={}", String::from_utf8_lossy(&stderr));
        assert!(checkout.join(".git").exists(), "checkout must survive");
        assert!(
            open_store(&runtime)
                .unwrap()
                .list_active()
                .unwrap()
                .is_empty(),
            "lease released"
        );
    }

    #[test]
    fn unknown_hook_event_is_allowed() {
        let runtime = HookRuntime::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = dispatch(
            r#"{"hook_event_name":"SessionStart"}"#,
            &runtime,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }
}
