//! Claude Code hook dispatcher.
//!
//! Reads hook JSON on stdin. Exit 0 allows; exit 2 blocks and writes a reason
//! on stderr that Claude Code shows to the model. `WorktreeCreate` also prints
//! the admitted worktree path as the last stdout line.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, PolicyCode, Result};
use crate::git_safe::{SafeGhCommand, SafeGitCommand};
use crate::identity::{StartPoint, resolve_start_commit};
use crate::lease::LeaseStore;
use crate::supervisor::normalize_program_name;
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
    match dispatch_inner(input, runtime, stdout) {
        Ok(()) => 0,
        Err(err) => {
            let _ = writeln!(stderr, "writ hook: {err}");
            2
        }
    }
}

fn dispatch_inner(input: &str, runtime: &HookRuntime, stdout: &mut impl Write) -> Result<()> {
    let event: HookEvent = serde_json::from_str(input).map_err(|e| Error::Io {
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
    for invocation in extract_git_gh_invocations(command)? {
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

fn handle_worktree_create(
    event: &HookEvent,
    runtime: &HookRuntime,
    stdout: &mut impl Write,
) -> Result<()> {
    let name = event
        .name
        .as_deref()
        .ok_or_else(|| Error::PolicyViolation {
            code: PolicyCode::WorktreeResumeUnproven,
            message: "WorktreeCreate hook JSON is missing `name`".to_owned(),
        })?;
    let cwd = event.cwd.as_deref().ok_or_else(|| Error::PolicyViolation {
        code: PolicyCode::GitDirUnavailable,
        message: "WorktreeCreate hook JSON is missing `cwd`".to_owned(),
    })?;
    let repo_root = git_toplevel(Path::new(cwd))?;
    let (owner, repo_name) = origin_owner_repo(&repo_root)?;
    let start_commit = resolve_start_commit(&repo_root, StartPoint("HEAD"))?;
    let manager = manager_for_runtime(runtime)?;
    let created = manager.create_with_request(WorktreeCreateRequest {
        repo_root: &repo_root,
        owner: &owner,
        repo: &repo_name,
        job_id: name,
        branch: name,
        start_point: &start_commit,
    })?;
    writeln!(stdout, "{}", created.path.display()).map_err(|e| Error::Io {
        context: "write WorktreeCreate path",
        source: e,
    })?;
    Ok(())
}

fn handle_worktree_remove(event: &HookEvent, runtime: &HookRuntime) -> Result<()> {
    let path = event
        .worktree_path
        .as_deref()
        .ok_or_else(|| Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            message: "WorktreeRemove hook JSON is missing `worktree_path`".to_owned(),
        })?;
    let manager = manager_for_runtime(runtime)?;
    manager.remove(Path::new(path), true)?;
    Ok(())
}

fn handle_subagent_start(event: &HookEvent, runtime: &HookRuntime) -> Result<()> {
    let agent_id = event.agent_id.as_deref().unwrap_or("unknown");
    let agent_type = event.agent_type.as_deref().unwrap_or("unknown");
    open_store(runtime)?.upsert_agent(agent_id, agent_type, event.session_id.as_deref())
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
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| Error::Io {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitGhTool {
    Git,
    Gh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    tool: GitGhTool,
    args: Vec<String>,
}

fn extract_git_gh_invocations(command: &str) -> Result<Vec<Invocation>> {
    let mut found = Vec::new();
    for statement in split_shell_statements(command) {
        collect_from_statement(&statement, &mut found)?;
    }
    if found.is_empty()
        && command
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
            .any(|token| matches!(token, "git" | "gh"))
    {
        return Err(Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            message: format!("unparseable git/gh command: {command}"),
        });
    }
    Ok(found)
}

fn collect_from_statement(statement: &str, found: &mut Vec<Invocation>) -> Result<()> {
    for inner in extract_substitutions(statement) {
        collect_from_statement(&inner, found)?;
    }
    let tokens = tokenize_shell(statement);
    let Some(invocation) = invocation_from_tokens(&tokens) else {
        return Ok(());
    };
    found.push(invocation);
    Ok(())
}

fn invocation_from_tokens(tokens: &[String]) -> Option<Invocation> {
    let stripped = strip_env_assignments(tokens);
    let (program, rest) = stripped.split_first()?;
    let name = normalize_program_name(program);
    let tool = match name.as_str() {
        "git" => GitGhTool::Git,
        "gh" => GitGhTool::Gh,
        _ => return None,
    };
    let args = match tool {
        GitGhTool::Git => skip_git_global_options(rest),
        GitGhTool::Gh => rest.to_vec(),
    };
    Some(Invocation { tool, args })
}

fn strip_env_assignments(tokens: &[String]) -> &[String] {
    let mut i = 0;
    while i < tokens.len() && is_env_assignment(&tokens[i]) {
        i += 1;
    }
    &tokens[i..]
}

fn is_env_assignment(token: &str) -> bool {
    let Some((key, _)) = token.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn skip_git_global_options(args: &[String]) -> Vec<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            return args[i + 1..].to_vec();
        }
        if matches!(
            arg,
            "-C" | "-c"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--config-env"
                | "--super-prefix"
        ) {
            i += 2;
            continue;
        }
        if arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("--namespace=")
            || arg.starts_with("--config-env=")
            || arg.starts_with("--super-prefix=")
        {
            i += 1;
            continue;
        }
        break;
    }
    args.get(i..).unwrap_or(&[]).to_vec()
}

fn split_shell_statements(command: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(ch) = chars.next() {
        if ch == '\\' && !in_single {
            if let Some(next) = chars.next() {
                current.push(ch);
                current.push(next);
            }
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            current.push(ch);
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            current.push(ch);
            continue;
        }
        if !in_single && !in_double {
            if ch == ';' || ch == '\n' {
                push_statement(&mut statements, &mut current);
                continue;
            }
            if ch == '&' && chars.peek() == Some(&'&') {
                chars.next();
                push_statement(&mut statements, &mut current);
                continue;
            }
            if ch == '|' && chars.peek() == Some(&'|') {
                chars.next();
                push_statement(&mut statements, &mut current);
                continue;
            }
            if ch == '|' || ch == '&' {
                push_statement(&mut statements, &mut current);
                continue;
            }
        }
        current.push(ch);
    }
    push_statement(&mut statements, &mut current);
    statements
}

fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_owned());
    }
    current.clear();
}

fn extract_substitutions(statement: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = statement.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$'
            && bytes.get(i + 1) == Some(&b'(')
            && let Some((inner, end)) = take_balanced(&statement[i + 2..], '(', ')')
        {
            found.push(inner.to_owned());
            i += 2 + end;
            continue;
        }
        if bytes[i] == b'`'
            && let Some(end) = statement[i + 1..].find('`')
        {
            found.push(statement[i + 1..i + 1 + end].to_owned());
            i += 2 + end;
            continue;
        }
        i += 1;
    }
    found
}

fn take_balanced(input: &str, open: char, close: char) -> Option<(&str, usize)> {
    let mut depth = 1;
    for (idx, ch) in input.char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some((&input[..idx], idx + ch.len_utf8()));
            }
        }
    }
    None
}

fn tokenize_shell(statement: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = statement.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(ch) = chars.next() {
        if ch == '\\' && !in_single {
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_double {
            in_double = !in_double;
            continue;
        }
        if ch.is_whitespace() && !in_single && !in_double {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Public validation entry used by tests: policy-check a Bash command string.
pub fn validate_bash_command(command: &str) -> Result<()> {
    for invocation in extract_git_gh_invocations(command)? {
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
