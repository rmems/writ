//! Claude Code hook dispatcher.
//!
//! Reads hook JSON on stdin. Exit 0 allows; exit 2 blocks and writes a reason
//! on stderr that Claude Code shows to the model. `WorktreeCreate` also prints
//! the admitted worktree path as the last stdout line.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::bash_argv::{GitGhTool, ShellText, git_gh_invocations};
use crate::error::{Error, PolicyCode, Result};
use crate::git_safe::{SafeGhCommand, SafeGitCommand};
use crate::identity::{StartPoint, resolve_start_commit};
use crate::lease::{AgentIdentity, LeaseStore};
use crate::worktree::{WorktreeCreateRequest, WorktreeManager};

/// Process-local paths for hook dispatch (tests inject temp dirs).
#[derive(Debug, Clone, Default)]
pub struct HookRuntime {
    pub worktree_base: Option<PathBuf>,
    pub lease_path: Option<PathBuf>,
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
            let _ = writeln!(stderr, "writ hook: {err}");
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
    cwd: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<ToolInput>,
    #[serde(default)]
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

fn handle_worktree_create(
    event: &HookEvent,
    runtime: &HookRuntime,
    stdout: &mut impl Write,
) -> Result<()> {
    let inputs = create_inputs(event)?;
    let created = manager_for_runtime(runtime)?.create_with_request(inputs.request())?;
    write_created_path(stdout, &created.path)
}

struct CreateInputs {
    repo_root: PathBuf,
    owner: String,
    repo_name: String,
    name: String,
    start_commit: String,
}

impl CreateInputs {
    fn request(&self) -> WorktreeCreateRequest<'_> {
        WorktreeCreateRequest {
            repo_root: &self.repo_root,
            owner: &self.owner,
            repo: &self.repo_name,
            job_id: &self.name,
            branch: &self.name,
            start_point: &self.start_commit,
        }
    }
}

fn create_inputs(event: &HookEvent) -> Result<CreateInputs> {
    let name = hook_field(
        event.name.as_deref(),
        PolicyCode::WorktreeResumeUnproven,
        "WorktreeCreate hook JSON is missing `name`",
    )?;
    let cwd = hook_field(
        event.cwd.as_deref(),
        PolicyCode::GitDirUnavailable,
        "WorktreeCreate hook JSON is missing `cwd`",
    )?;
    let repo_root = git_toplevel(Path::new(cwd))?;
    let (owner, repo_name) = origin_owner_repo(&repo_root)?;
    let start_commit = resolve_start_commit(&repo_root, StartPoint("HEAD"))?;
    Ok(CreateInputs {
        repo_root,
        owner,
        repo_name,
        name: name.to_owned(),
        start_commit,
    })
}

fn hook_field<'a>(
    value: Option<&'a str>,
    code: PolicyCode,
    message: &'static str,
) -> Result<&'a str> {
    value.ok_or_else(|| Error::PolicyViolation {
        code,
        message: message.to_owned(),
    })
}

fn write_created_path(stdout: &mut impl Write, path: &Path) -> Result<()> {
    writeln!(stdout, "{}", path.display()).map_err(|e| Error::Io {
        context: "write WorktreeCreate path",
        source: e,
    })
}

fn handle_worktree_remove(event: &HookEvent, runtime: &HookRuntime) -> Result<()> {
    let path = hook_field(
        event.worktree_path.as_deref(),
        PolicyCode::PathNotAllowed,
        "WorktreeRemove hook JSON is missing `worktree_path`",
    )?;
    let manager = manager_for_runtime(runtime)?;
    manager.remove(Path::new(path), true)?;
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

fn manager_for_runtime(runtime: &HookRuntime) -> Result<WorktreeManager> {
    match (&runtime.worktree_base, &runtime.lease_path) {
        (Some(base), Some(lease)) => {
            WorktreeManager::with_base_and_leases(base.clone(), LeaseStore::open(lease)?)
        }
        (Some(base), None) => WorktreeManager::with_base(base.clone()),
        (None, Some(lease)) => {
            let base = crate::paths::worktree_base_path()?;
            WorktreeManager::with_base_and_leases(base, LeaseStore::open(lease)?)
        }
        (None, None) => WorktreeManager::new(),
    }
}

fn open_store(runtime: &HookRuntime) -> Result<LeaseStore> {
    match &runtime.lease_path {
        Some(path) => LeaseStore::open(path),
        None => LeaseStore::open(crate::paths::lease_store_path()),
    }
}

fn git_toplevel(cwd: &Path) -> Result<PathBuf> {
    let output =
        crate::git_cmd::git_in(cwd, &["rev-parse", "--show-toplevel"]).map_err(|e| Error::Io {
            context: "resolve hook repository root",
            source: e,
        })?;
    if !output.status.success() {
        return Err(Error::PolicyViolation {
            code: PolicyCode::GitDirUnavailable,
            message: format!(
                "WorktreeCreate cwd is not a git repository: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

fn origin_owner_repo(repo_root: &Path) -> Result<(String, String)> {
    match crate::git_safe::origin_github_slug(repo_root) {
        Ok(slug) => {
            let (owner, repo) = slug.split_once('/').ok_or_else(|| Error::PolicyViolation {
                code: PolicyCode::GitDirUnavailable,
                message: format!("origin slug `{slug}` is not owner/repo"),
            })?;
            Ok((owner.to_owned(), repo.to_owned()))
        }
        Err(_) => {
            let name = repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::InvalidSegment {
                    field: "repo",
                    value: repo_root.display().to_string(),
                })?;
            Ok(("local".to_owned(), name.to_owned()))
        }
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

    #[test]
    fn pre_tool_use_blocks_force_push_and_merge() {
        let runtime = HookRuntime::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = dispatch(
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git push --force"}}"#,
            &runtime,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2);
        assert!(String::from_utf8_lossy(&stderr).contains("BARE_FORCE_PUSH"));

        let mut stderr = Vec::new();
        let code = dispatch(
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"FOO=bar git merge feature"}}"#,
            &runtime,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2);
        assert!(String::from_utf8_lossy(&stderr).contains("MERGE_BLOCKED"));
    }

    #[test]
    fn pre_tool_use_blocks_gh_merge_and_api() {
        let runtime = HookRuntime::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = dispatch(
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"gh pr merge 1"}}"#,
            &runtime,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2);
        assert!(String::from_utf8_lossy(&stderr).contains("MERGE_BLOCKED"));

        let mut stderr = Vec::new();
        let code = dispatch(
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"gh api repos/acme/example"}}"#,
            &runtime,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2);
        assert!(String::from_utf8_lossy(&stderr).contains("GH_SUBCOMMAND_NOT_ALLOWED"));
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
