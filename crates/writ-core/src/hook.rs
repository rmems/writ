//! Claude Code hook dispatcher: the production enforcement boundary.
//!
//! `writ hook` reads one JSON payload from stdin and exits 0 or 2. Policy
//! rejections on `PreToolUse` always use exit 2 so the host cannot override
//! them with a competing hook's `permissionDecision: "allow"`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, PolicyCode, Result};
use crate::git_safe::{SafeGhCommand, SafeGitCommand};
use crate::lease::{Lease, LeaseMode, LeaseStore, path_key};
use crate::paths::{derive_worktree_path, worktree_base_path};
use crate::supervisor::{is_forbidden_wrapper, normalize_program_name};
use crate::worktree::{WorktreeCreateRequest, WorktreeManager};

/// Paths an accidental agent must not rewrite: hook config and the enforcer.
const HOOK_CONFIG_SUFFIXES: &[&str] = &[".claude/settings.json", ".claude/settings.local.json"];

/// Result of dispatching one hook payload.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HookOutcome {
    pub exit_code: u8,
    pub stdout: String,
    pub stderr: String,
}

impl HookOutcome {
    #[must_use]
    pub fn success() -> Self {
        Self {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[must_use]
    pub fn allow_path(path: impl Into<String>) -> Self {
        Self {
            exit_code: 0,
            stdout: path.into(),
            stderr: String::new(),
        }
    }

    #[must_use]
    pub fn fail_closed(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            exit_code: 2,
            stdout: String::new(),
            stderr: format!("writ: hook failed closed: {reason}\n"),
        }
    }

    #[must_use]
    pub fn from_error(error: &Error) -> Self {
        Self {
            exit_code: error.exit_code(),
            stdout: String::new(),
            stderr: format!("writ: {error}\n"),
        }
    }
}

/// Claude Code's documented PreToolUse composition rule.
///
/// Exit 2 from any hook blocks the tool call. JSON `permissionDecision: "allow"`
/// from another hook cannot override it. This models the host contract so
/// tests can prove the claim without a live Claude Code session.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ComposedPreToolUse {
    Block { reason: String },
    Continue,
}

/// Apply the host composition rule to already-run hook processes.
#[must_use]
pub fn compose_pre_tool_use(hooks: &[HookOutcome]) -> ComposedPreToolUse {
    for hook in hooks {
        if hook.exit_code == 2 {
            return ComposedPreToolUse::Block {
                reason: hook.stderr.clone(),
            };
        }
    }
    ComposedPreToolUse::Continue
}

/// Runtime paths injected by the CLI or tests.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub lease_path: PathBuf,
    pub worktree_base: PathBuf,
    pub enforcer_path: PathBuf,
    pub expected_branch: Option<String>,
}

impl HookContext {
    /// Resolve context from process environment.
    pub fn from_env() -> Result<Self> {
        let lease_path = match std::env::var_os("WRIT_LEASE_PATH") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => crate::paths::StateRoot::default_root()
                .as_path()
                .join("leases.sqlite"),
        };
        let enforcer_path = match std::env::var_os("WRIT_BIN") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => std::env::current_exe().unwrap_or_else(|_| PathBuf::from("writ")),
        };
        let expected_branch = std::env::var("WRIT_EXPECTED_BRANCH")
            .ok()
            .filter(|v| !v.is_empty());
        Ok(Self {
            lease_path,
            worktree_base: worktree_base_path()?,
            enforcer_path,
            expected_branch,
        })
    }
}

#[derive(Debug, Deserialize)]
struct HookPayload {
    hook_event_name: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<ToolInput>,
    #[serde(default)]
    cwd: Option<String>,
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
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    start_point: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolInput {
    #[serde(default)]
    command: Option<String>,
}

/// Dispatch one Claude Code hook payload.
#[must_use]
pub fn dispatch_hook(stdin: &[u8], ctx: &HookContext) -> HookOutcome {
    let payload = match parse_payload(stdin) {
        Ok(payload) => payload,
        Err(reason) => return HookOutcome::fail_closed(reason),
    };
    let Some(event) = payload.hook_event_name.as_deref() else {
        return HookOutcome::fail_closed("missing hook_event_name");
    };
    match event {
        "PreToolUse" => dispatch_pre_tool_use(&payload, ctx),
        "WorktreeCreate" => dispatch_worktree_create(&payload, ctx),
        "WorktreeRemove" => dispatch_worktree_remove(&payload, ctx),
        "SubagentStart" => dispatch_subagent_start(&payload, ctx),
        "SubagentStop" => dispatch_subagent_stop(&payload, ctx),
        _ => HookOutcome::success(),
    }
}

fn parse_payload(stdin: &[u8]) -> std::result::Result<HookPayload, String> {
    if stdin.iter().all(u8::is_ascii_whitespace) {
        return Err("empty hook payload".to_owned());
    }
    serde_json::from_slice(stdin).map_err(|err| format!("malformed hook JSON: {err}"))
}

fn dispatch_pre_tool_use(payload: &HookPayload, ctx: &HookContext) -> HookOutcome {
    let Some(tool_name) = payload.tool_name.as_deref() else {
        return HookOutcome::fail_closed("PreToolUse missing tool_name");
    };
    if !tool_name.eq_ignore_ascii_case("Bash") {
        // Edit/Write protection is Phase 5. Unknown tools must not false-positive.
        return HookOutcome::success();
    }
    let Some(command) = payload
        .tool_input
        .as_ref()
        .and_then(|input| input.command.as_deref())
    else {
        return HookOutcome::fail_closed("PreToolUse Bash missing tool_input.command");
    };
    if let Err(error) = enforce_bash_command(command, ctx, payload.cwd.as_deref()) {
        return pre_tool_use_error(&error);
    }
    HookOutcome::success()
}

fn pre_tool_use_error(error: &Error) -> HookOutcome {
    // PreToolUse only blocks on exit 2. Operational failures must not leak as
    // a non-blocking exit 1.
    HookOutcome {
        exit_code: 2,
        stdout: String::new(),
        stderr: format!("writ: {error}\n"),
    }
}

fn enforce_bash_command(command: &str, ctx: &HookContext, cwd: Option<&str>) -> Result<()> {
    let words = split_shell_words(command)?;
    let words = strip_env_assignments(&words);
    if words.is_empty() {
        return Ok(());
    }
    if looks_like_shell_control(words) {
        return Err(Error::HookFailClosed {
            reason: "compound shell command is not a single git/gh invocation".to_owned(),
        });
    }
    if extra_git_or_gh_tokens(words) {
        return Err(Error::HookFailClosed {
            reason: "multiple git/gh invocations in one Bash payload".to_owned(),
        });
    }
    if let Some(path) = protected_write_target(words, ctx) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::ProtectedPath,
            message: format!(
                "refusing to write protected path `{}`",
                path.to_string_lossy()
            ),
        });
    }
    let program = normalize_program_name(&words[0]);
    let args = &words[1..];
    if is_forbidden_wrapper(&program) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            message: format!(
                "program `{program}` can launch unreviewed commands and is not allowed at PreToolUse"
            ),
        });
    }
    match program.as_str() {
        "git" => {
            let cmd = SafeGitCommand::new(args)?;
            if let Some(expected) = expected_branch_for(ctx, cwd)?
                && cmd.requires_branch_check()
            {
                let repo = cwd.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
                cmd.verify_branch(&repo, &expected)?;
            }
            Ok(())
        }
        "gh" => {
            SafeGhCommand::new(args)?;
            Ok(())
        }
        _ => Ok(()),
    }
}

fn expected_branch_for(ctx: &HookContext, cwd: Option<&str>) -> Result<Option<String>> {
    if let Some(branch) = ctx.expected_branch.as_deref() {
        return Ok(Some(branch.to_owned()));
    }
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    let store = LeaseStore::open(&ctx.lease_path)?;
    Ok(store.get(Path::new(cwd))?.map(|lease| lease.branch))
}

fn dispatch_worktree_create(payload: &HookPayload, ctx: &HookContext) -> HookOutcome {
    match create_and_lease(payload, ctx) {
        Ok(path) => HookOutcome::allow_path(format!("{}\n", path.display())),
        Err(error) => HookOutcome::from_error(&error),
    }
}

fn create_and_lease(payload: &HookPayload, ctx: &HookContext) -> Result<PathBuf> {
    let owner = required_identity(payload.owner.as_deref(), "owner")?;
    let repo = required_identity(payload.repo.as_deref(), "repo")?;
    let job_id = payload
        .job_id
        .as_deref()
        .or(payload.name.as_deref())
        .ok_or_else(|| Error::HookFailClosed {
            reason: "WorktreeCreate missing job_id/name".to_owned(),
        })?;
    let branch = required_identity(payload.branch.as_deref(), "branch")?;
    let start_point = payload
        .start_point
        .as_deref()
        .ok_or(Error::StartPointRequired)?;
    let repo_root = payload
        .cwd
        .as_deref()
        .ok_or_else(|| Error::HookFailClosed {
            reason: "WorktreeCreate missing cwd".to_owned(),
        })?;
    let repo_root = Path::new(repo_root);
    let derived = derive_worktree_path(&ctx.worktree_base, owner, repo, job_id)?;
    reject_protected_lease_scope(&derived, ctx)?;
    let manager = WorktreeManager::with_base(ctx.worktree_base.clone())?;
    let request = WorktreeCreateRequest {
        repo_root,
        owner,
        repo,
        job_id,
        branch,
        start_point,
    };
    let worktree = manager.create_with_request(request)?;
    let stored = path_key(&worktree.path);
    let store = LeaseStore::open(&ctx.lease_path)?;
    store.grant(&Lease {
        worktree_path: stored.clone(),
        repo: repo.to_owned(),
        branch: branch.to_owned(),
        owner: owner.to_owned(),
        mode: LeaseMode::WriterLocked,
        ttl: None,
        heartbeat: None,
    })?;
    Ok(PathBuf::from(stored))
}

fn dispatch_worktree_remove(payload: &HookPayload, ctx: &HookContext) -> HookOutcome {
    let Some(path) = payload.worktree_path.as_deref() else {
        return HookOutcome::fail_closed("WorktreeRemove missing worktree_path");
    };
    match LeaseStore::open(&ctx.lease_path).and_then(|store| store.release(Path::new(path))) {
        Ok(()) => HookOutcome::success(),
        Err(error) => HookOutcome::from_error(&error),
    }
}

fn dispatch_subagent_start(payload: &HookPayload, ctx: &HookContext) -> HookOutcome {
    let Some(agent_id) = payload.agent_id.as_deref() else {
        return HookOutcome::success();
    };
    match LeaseStore::open(&ctx.lease_path).and_then(|store| {
        store.upsert_agent(
            agent_id,
            payload.agent_type.as_deref(),
            payload.session_id.as_deref(),
        )
    }) {
        Ok(()) => HookOutcome::success(),
        Err(error) => HookOutcome::from_error(&error),
    }
}

fn dispatch_subagent_stop(payload: &HookPayload, ctx: &HookContext) -> HookOutcome {
    let Some(agent_id) = payload.agent_id.as_deref() else {
        return HookOutcome::success();
    };
    match LeaseStore::open(&ctx.lease_path).and_then(|store| store.retire_agent(agent_id)) {
        Ok(()) => HookOutcome::success(),
        Err(error) => HookOutcome::from_error(&error),
    }
}

fn required_identity<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    value
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::HookFailClosed {
            reason: format!("WorktreeCreate missing {field}"),
        })
}

fn reject_protected_lease_scope(worktree_path: &Path, ctx: &HookContext) -> Result<()> {
    if is_protected_path(worktree_path, ctx) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::ProtectedPath,
            message: format!(
                "lease scope cannot include protected path `{}`",
                worktree_path.display()
            ),
        });
    }
    Ok(())
}

/// True when `path` is hook configuration or the enforcer binary.
#[must_use]
pub fn is_protected_path(path: &Path, ctx: &HookContext) -> bool {
    let rendered = path.to_string_lossy().replace('\\', "/");
    if HOOK_CONFIG_SUFFIXES
        .iter()
        .any(|suffix| rendered.ends_with(suffix))
    {
        return true;
    }
    path_eq(path, &ctx.enforcer_path)
}

fn path_eq(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => left == right,
    }
}

fn protected_write_target(words: &[String], ctx: &HookContext) -> Option<PathBuf> {
    let program = normalize_program_name(&words[0]);
    if matches!(
        program.as_str(),
        "tee" | "cp" | "mv" | "install" | "touch" | "dd"
    ) {
        for word in &words[1..] {
            if word.starts_with('-') {
                continue;
            }
            if is_protected_path(Path::new(word), ctx) {
                return Some(PathBuf::from(word));
            }
        }
    }
    for dest in redirect_destinations(words) {
        if is_protected_path(Path::new(dest), ctx) {
            return Some(PathBuf::from(dest));
        }
    }
    None
}

fn redirect_destinations(words: &[String]) -> Vec<&str> {
    let mut dests = Vec::new();
    let mut pending = false;
    for word in words {
        if pending {
            dests.push(word.as_str());
            pending = false;
            continue;
        }
        if matches!(word.as_str(), ">" | ">>" | "2>" | "2>>") {
            pending = true;
            continue;
        }
        for prefix in [">>", ">", "2>>", "2>"] {
            if let Some(rest) = word.strip_prefix(prefix)
                && !rest.is_empty()
            {
                dests.push(rest);
                break;
            }
        }
    }
    dests
}

fn extra_git_or_gh_tokens(words: &[String]) -> bool {
    words
        .iter()
        .skip(1)
        .any(|word| matches!(normalize_program_name(word).as_str(), "git" | "gh"))
}

fn looks_like_shell_control(words: &[String]) -> bool {
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "&&" | "||" | "|" | ";" | "&" | ">" | ">>" | "<" | ">&" | ">(" | "<("
        )
    })
}

fn strip_env_assignments(words: &[String]) -> &[String] {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        if is_env_assignment(word) {
            index += 1;
        } else {
            break;
        }
    }
    &words[index..]
}

fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn split_shell_words(command: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(ch) = chars.next() {
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        if in_double {
            if ch == '"' {
                in_double = false;
            } else if ch == '\\' {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\n' | '\r' => {
                return Err(Error::HookFailClosed {
                    reason: "unquoted newline is a command separator".to_owned(),
                });
            }
            ';' => {
                return Err(Error::HookFailClosed {
                    reason: "unquoted semicolon is a command separator".to_owned(),
                });
            }
            '\'' => in_single = true,
            '"' => in_double = true,
            '`' => {
                return Err(Error::HookFailClosed {
                    reason: "backtick substitution is not classified".to_owned(),
                });
            }
            '$' if matches!(chars.peek(), Some('(' | '{')) => {
                return Err(Error::HookFailClosed {
                    reason: "command or parameter substitution is not classified".to_owned(),
                });
            }
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if in_single || in_double {
        return Err(Error::HookFailClosed {
            reason: "unterminated quote in hook command".to_owned(),
        });
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

/// Ordinary developer commands that must not be blocked (false-positive corpus).
#[must_use]
pub fn false_positive_corpus() -> &'static [&'static str] {
    &[
        "git status",
        "git log",
        "git log --oneline -n 5",
        "git commit -m \"fix typo\"",
        "git push origin feature/assigned",
        "git push --force-with-lease origin feature/assigned",
        "git rebase origin/main",
        "git status .claude/settings.json 2>/dev/null",
        "gh pr view 1",
        "gh pr list",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn ctx(dir: &Path) -> HookContext {
        HookContext {
            lease_path: dir.join("leases.sqlite"),
            worktree_base: dir.join("worktrees"),
            enforcer_path: dir.join("writ-bin"),
            expected_branch: None,
        }
    }

    #[test]
    fn malformed_json_fails_closed() {
        let dir = tempdir().unwrap();
        let outcome = dispatch_hook(b"{not json", &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 2);
        assert!(outcome.stderr.contains("malformed hook JSON"));
    }

    #[test]
    fn empty_payload_fails_closed() {
        let dir = tempdir().unwrap();
        let outcome = dispatch_hook(b"   \n", &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 2);
    }

    #[test]
    fn unrecognized_event_exits_zero_without_decision() {
        let dir = tempdir().unwrap();
        let outcome = dispatch_hook(br#"{"hook_event_name":"SessionStart"}"#, &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.stdout.is_empty());
        assert!(outcome.stderr.is_empty());
    }

    #[test]
    fn pre_tool_use_blocks_merge_with_stderr_reason() {
        let dir = tempdir().unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "git merge feature"}
        });
        let outcome = dispatch_hook(payload.to_string().as_bytes(), &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 2);
        assert!(outcome.stderr.contains("MERGE_BLOCKED"));
        assert!(outcome.stdout.is_empty());
    }

    #[test]
    fn competing_allow_json_does_not_override_exit_2() {
        let dir = tempdir().unwrap();
        let blocked = dispatch_hook(
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": {"command": "git push --force"}
            })
            .to_string()
            .as_bytes(),
            &ctx(dir.path()),
        );
        let competitor = HookOutcome {
            exit_code: 0,
            stdout: r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#.into(),
            stderr: String::new(),
        };
        match compose_pre_tool_use(&[blocked, competitor]) {
            ComposedPreToolUse::Block { reason } => {
                assert!(reason.contains("BARE_FORCE_PUSH"));
            }
            ComposedPreToolUse::Continue => panic!("exit 2 must win over JSON allow"),
        }
    }

    #[test]
    fn false_positive_corpus_is_not_blocked() {
        let dir = tempdir().unwrap();
        let context = ctx(dir.path());
        for command in false_positive_corpus() {
            let payload = serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": {"command": command}
            });
            let outcome = dispatch_hook(payload.to_string().as_bytes(), &context);
            assert_eq!(
                outcome.exit_code, 0,
                "false positive on {command}: {}",
                outcome.stderr
            );
        }
    }

    #[test]
    fn protected_hook_config_write_is_blocked() {
        let dir = tempdir().unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "tee .claude/settings.json"}
        });
        let outcome = dispatch_hook(payload.to_string().as_bytes(), &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 2);
        assert!(outcome.stderr.contains("PROTECTED_PATH"));
    }

    #[test]
    fn newline_separated_merge_fails_closed() {
        let dir = tempdir().unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "git status\ngit merge feature"}
        });
        let outcome = dispatch_hook(payload.to_string().as_bytes(), &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 2, "{}", outcome.stderr);
        assert!(
            outcome.stderr.contains("newline") || outcome.stderr.contains("failed closed"),
            "{}",
            outcome.stderr
        );
    }

    #[test]
    fn cp_to_hook_config_is_blocked() {
        let dir = tempdir().unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "cp foo .claude/settings.json"}
        });
        let outcome = dispatch_hook(payload.to_string().as_bytes(), &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 2);
        assert!(outcome.stderr.contains("PROTECTED_PATH"));
    }

    #[test]
    fn git_status_redirecting_stderr_is_not_a_false_positive() {
        let dir = tempdir().unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "git status .claude/settings.json 2>/dev/null"}
        });
        let outcome = dispatch_hook(payload.to_string().as_bytes(), &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
    }

    #[test]
    fn wrapper_launching_git_is_blocked() {
        let dir = tempdir().unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "sh -c \"git merge feature\""}
        });
        let outcome = dispatch_hook(payload.to_string().as_bytes(), &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 2, "{}", outcome.stderr);
    }

    #[test]
    fn non_bash_tool_is_not_asserted() {
        let dir = tempdir().unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": "src/lib.rs"}
        });
        let outcome = dispatch_hook(payload.to_string().as_bytes(), &ctx(dir.path()));
        assert_eq!(outcome.exit_code, 0);
    }
}
