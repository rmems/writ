//! Idempotent writer for the Claude Code hook block in `.claude/settings.json`.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{Error, Result};

/// Stable argv for the hook dispatcher.
pub const HOOK_ARGS: [&str; 1] = ["hook"];

/// Path or name of the `writ` executable written into hook settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WritCommand<'a>(pub &'a str);

impl<'a> WritCommand<'a> {
    #[must_use]
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

#[derive(Clone, Copy)]
struct HookEventName(&'static str);

impl HookEventName {
    const PRE_TOOL_USE: Self = Self("PreToolUse");
    const WORKTREE_CREATE: Self = Self("WorktreeCreate");
    const WORKTREE_REMOVE: Self = Self("WorktreeRemove");
    const SUBAGENT_START: Self = Self("SubagentStart");
    const SUBAGENT_STOP: Self = Self("SubagentStop");

    const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HookIf(&'static str);

impl HookIf {
    const BASH_GIT: Self = Self("Bash(git *)");
    const BASH_GH: Self = Self("Bash(gh *)");

    const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Result of an install pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    pub path: std::path::PathBuf,
    pub changed: bool,
}

/// Merge the `writ hook` block into `settings_path` without dropping other hooks.
pub fn install_settings(
    settings_path: &Path,
    writ_command: WritCommand<'_>,
) -> Result<InstallResult> {
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::Io {
            context: "create Claude Code settings directory",
            source: e,
        })?;
    }
    let _lock = SettingsLock::acquire(settings_path)?;
    let existing = read_settings_object(settings_path)?;
    let (merged, changed) = merge_hook_block(existing, writ_command);
    if changed {
        write_settings(settings_path, &merged)?;
    }

    Ok(InstallResult {
        path: settings_path.to_path_buf(),
        changed,
    })
}

/// Exclusive lock for the settings read/merge/write so concurrent installers
/// cannot clobber each other's hook blocks.
struct SettingsLock {
    _file: File,
}

impl SettingsLock {
    fn acquire(settings_path: &Path) -> Result<Self> {
        let lock_path = settings_lock_path(settings_path);
        let file = open_lock_file(&lock_path).map_err(|e| Error::Io {
            context: "open Claude Code settings lock",
            source: e,
        })?;
        lock_file_exclusive(&file).map_err(|e| Error::Io {
            context: "lock Claude Code settings",
            source: e,
        })?;
        Ok(Self { _file: file })
    }
}

fn settings_lock_path(settings_path: &Path) -> PathBuf {
    let mut lock_name = settings_path
        .file_name()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| OsString::from("settings.json"));
    lock_name.push(".lock");
    match settings_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(lock_name),
        _ => PathBuf::from(lock_name),
    }
}

#[cfg(windows)]
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
}

#[cfg(not(windows))]
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn lock_file_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` is an open descriptor; LOCK_EX is the documented
    // advisory exclusive flock and is released when the File is dropped.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_file_exclusive(_file: &File) -> std::io::Result<()> {
    Ok(())
}

fn read_settings_object(settings_path: &Path) -> Result<Value> {
    if !settings_path.exists() {
        return Ok(json!({}));
    }
    let text = fs::read_to_string(settings_path).map_err(|e| Error::Io {
        context: "read Claude Code settings",
        source: e,
    })?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|e| Error::Io {
        context: "parse Claude Code settings",
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })
}

fn write_settings(settings_path: &Path, merged: &Value) -> Result<()> {
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(merged).map_err(|e| {
            Error::Io {
                context: "serialize Claude Code settings",
                source: std::io::Error::other(e),
            }
        })?
    );
    fs::write(settings_path, body).map_err(|e| Error::Io {
        context: "write Claude Code settings",
        source: e,
    })
}

fn merge_hook_block(mut root: Value, writ_command: WritCommand<'_>) -> (Value, bool) {
    let hooks = hooks_object(&mut root);
    let mut changed = ensure_pre_tool_use(hooks, writ_command);
    for event in [
        HookEventName::WORKTREE_CREATE,
        HookEventName::WORKTREE_REMOVE,
        HookEventName::SUBAGENT_START,
        HookEventName::SUBAGENT_STOP,
    ] {
        changed |= ensure_unmatched_event(hooks, event, writ_command);
    }
    (root, changed)
}

fn hooks_object(root: &mut Value) -> &mut Value {
    coerce_object(root);
    let hooks = root
        .as_object_mut()
        .expect("root object")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    coerce_object(hooks);
    hooks
}

fn coerce_object(value: &mut Value) {
    if !value.is_object() {
        *value = json!({});
    }
}

fn ensure_pre_tool_use(hooks: &mut Value, writ_command: WritCommand<'_>) -> bool {
    let groups = event_groups(hooks, HookEventName::PRE_TOOL_USE);
    let Some(bash_group) = groups.iter_mut().find(|group| matcher_is_bash(group)) else {
        groups.push(json!({
            "matcher": "Bash",
            "hooks": [
                hook_handler(writ_command, Some(HookIf::BASH_GIT)),
                hook_handler(writ_command, Some(HookIf::BASH_GH)),
            ]
        }));
        return true;
    };
    let handlers = hook_list(bash_group);
    let mut changed = false;
    for condition in [HookIf::BASH_GIT, HookIf::BASH_GH] {
        if let Some(existing) = handlers
            .iter_mut()
            .find(|h| handler_if(h) == Some(condition.as_str()))
        {
            changed |= update_handler_command(existing, writ_command);
        } else {
            handlers.push(hook_handler(writ_command, Some(condition)));
            changed = true;
        }
    }
    changed
}

fn ensure_unmatched_event(
    hooks: &mut Value,
    event: HookEventName,
    writ_command: WritCommand<'_>,
) -> bool {
    let groups = event_groups(hooks, event);
    if let Some(existing) = groups
        .iter_mut()
        .flat_map(hook_list)
        .find(|h| is_writ_hook(h))
    {
        return update_handler_command(existing, writ_command);
    }
    groups.push(json!({
        "hooks": [hook_handler(writ_command, None)]
    }));
    true
}

fn event_groups(hooks: &mut Value, event: HookEventName) -> &mut Vec<Value> {
    ensure_array(hooks, JsonField(event.as_str()))
}

fn hook_list(group: &mut Value) -> &mut Vec<Value> {
    ensure_array(group, JsonField("hooks"))
}

#[derive(Clone, Copy)]
struct JsonField(&'static str);

fn ensure_array(parent: &mut Value, key: JsonField) -> &mut Vec<Value> {
    coerce_object(parent);
    let entry = parent
        .as_object_mut()
        .expect("json object")
        .entry(key.0)
        .or_insert_with(|| json!([]));
    if !entry.is_array() {
        *entry = json!([]);
    }
    entry.as_array_mut().expect("json array")
}

fn matcher_is_bash(group: &Value) -> bool {
    group.get("matcher").and_then(Value::as_str) == Some("Bash")
}

fn handler_if(handler: &Value) -> Option<&str> {
    handler.get("if").and_then(Value::as_str)
}

fn is_writ_hook(handler: &Value) -> bool {
    let args_ok = handler
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|args| args.iter().filter_map(Value::as_str).collect::<Vec<_>>() == ["hook"]);
    let command_ok = handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(command_looks_like_writ);
    args_ok && command_ok
}

fn command_looks_like_writ(command: &str) -> bool {
    let name = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_end_matches(".exe");
    name == "writ"
}

fn update_handler_command(handler: &mut Value, writ_command: WritCommand<'_>) -> bool {
    let mut changed = false;
    if handler.get("type").and_then(Value::as_str) != Some("command") {
        handler["type"] = json!("command");
        changed = true;
    }
    if handler.get("command").and_then(Value::as_str) != Some(writ_command.as_str()) {
        handler["command"] = json!(writ_command.as_str());
        changed = true;
    }
    let expected_args = json!(HOOK_ARGS);
    if handler.get("args") != Some(&expected_args) {
        handler["args"] = expected_args;
        changed = true;
    }
    changed
}

fn hook_handler(writ_command: WritCommand<'_>, condition: Option<HookIf>) -> Value {
    let mut handler = json!({
        "type": "command",
        "command": writ_command.as_str(),
        "args": HOOK_ARGS,
    });
    if let Some(condition) = condition {
        handler["if"] = json!(condition.as_str());
    }
    handler
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn install_is_idempotent_and_preserves_existing_hooks() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
  "hooks": {
    "SessionStart": [
      { "matcher": "", "hooks": [{ "type": "command", "command": "bd prime" }] }
    ]
  }
}
"#,
        )
        .unwrap();

        let first = install_settings(&path, WritCommand("/opt/writ")).unwrap();
        assert!(first.changed);
        let second = install_settings(&path, WritCommand("/opt/writ")).unwrap();
        assert!(!second.changed);

        let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            parsed["hooks"]["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("bd prime")
        );
        assert_eq!(
            parsed["hooks"]["PreToolUse"][0]["hooks"][0]["if"],
            "Bash(git *)"
        );
        assert_eq!(
            parsed["hooks"]["PreToolUse"][0]["hooks"][1]["if"],
            "Bash(gh *)"
        );
        assert_eq!(
            parsed["hooks"]["WorktreeCreate"][0]["hooks"][0]["args"],
            json!(["hook"])
        );
    }

    #[test]
    fn settings_lock_sits_beside_settings() {
        assert_eq!(
            settings_lock_path(Path::new(".claude/settings.json")),
            PathBuf::from(".claude/settings.json.lock")
        );
    }
}
