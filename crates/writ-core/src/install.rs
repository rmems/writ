//! Idempotent writer for the Claude Code hook block in `.claude/settings.json`.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::error::{Error, Result};

/// Stable argv for the hook dispatcher.
pub const HOOK_ARGS: [&str; 1] = ["hook"];

/// Result of an install pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    pub path: std::path::PathBuf,
    pub changed: bool,
}

/// Merge the `writ hook` block into `settings_path` without dropping other hooks.
pub fn install_settings(settings_path: &Path, writ_command: &str) -> Result<InstallResult> {
    let existing = if settings_path.exists() {
        let text = fs::read_to_string(settings_path).map_err(|e| Error::Io {
            context: "read Claude Code settings",
            source: e,
        })?;
        if text.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&text).map_err(|e| Error::Io {
                context: "parse Claude Code settings",
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            })?
        }
    } else {
        json!({})
    };

    let (merged, changed) = merge_hook_block(existing, writ_command);
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::Io {
            context: "create Claude Code settings directory",
            source: e,
        })?;
    }
    if changed {
        let body = format!(
            "{}\n",
            serde_json::to_string_pretty(&merged).map_err(|e| {
                Error::Io {
                    context: "serialize Claude Code settings",
                    source: std::io::Error::other(e),
                }
            })?
        );
        fs::write(settings_path, body).map_err(|e| Error::Io {
            context: "write Claude Code settings",
            source: e,
        })?;
    }

    Ok(InstallResult {
        path: settings_path.to_path_buf(),
        changed,
    })
}

fn merge_hook_block(mut root: Value, writ_command: &str) -> (Value, bool) {
    if !root.is_object() {
        root = json!({});
    }
    let hooks = root
        .as_object_mut()
        .expect("root object")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }

    let mut changed = false;
    changed |= ensure_pre_tool_use(hooks, writ_command);
    for event in [
        "WorktreeCreate",
        "WorktreeRemove",
        "SubagentStart",
        "SubagentStop",
    ] {
        changed |= ensure_unmatched_event(hooks, event, writ_command);
    }
    (root, changed)
}

fn ensure_pre_tool_use(hooks: &mut Value, writ_command: &str) -> bool {
    let groups = event_groups(hooks, "PreToolUse");
    let Some(bash_group) = groups.iter_mut().find(|group| matcher_is_bash(group)) else {
        groups.push(json!({
            "matcher": "Bash",
            "hooks": [
                hook_handler(writ_command, Some("Bash(git *)")),
                hook_handler(writ_command, Some("Bash(gh *)")),
            ]
        }));
        return true;
    };
    let handlers = hook_list(bash_group);
    let mut changed = false;
    for condition in ["Bash(git *)", "Bash(gh *)"] {
        if let Some(existing) = handlers
            .iter_mut()
            .find(|h| handler_if(h) == Some(condition))
        {
            changed |= update_handler_command(existing, writ_command);
        } else {
            handlers.push(hook_handler(writ_command, Some(condition)));
            changed = true;
        }
    }
    changed
}

fn ensure_unmatched_event(hooks: &mut Value, event: &str, writ_command: &str) -> bool {
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

fn event_groups<'a>(hooks: &'a mut Value, event: &str) -> &'a mut Vec<Value> {
    let entry = hooks
        .as_object_mut()
        .expect("hooks object")
        .entry(event)
        .or_insert_with(|| json!([]));
    if !entry.is_array() {
        *entry = json!([]);
    }
    entry.as_array_mut().expect("hook event array")
}

fn hook_list(group: &mut Value) -> &mut Vec<Value> {
    if !group.is_object() {
        *group = json!({});
    }
    let hooks = group
        .as_object_mut()
        .expect("hook group object")
        .entry("hooks")
        .or_insert_with(|| json!([]));
    if !hooks.is_array() {
        *hooks = json!([]);
    }
    group
        .get_mut("hooks")
        .and_then(Value::as_array_mut)
        .expect("hooks array")
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

fn update_handler_command(handler: &mut Value, writ_command: &str) -> bool {
    let mut changed = false;
    if handler.get("type").and_then(Value::as_str) != Some("command") {
        handler["type"] = json!("command");
        changed = true;
    }
    if handler.get("command").and_then(Value::as_str) != Some(writ_command) {
        handler["command"] = json!(writ_command);
        changed = true;
    }
    let expected_args = json!(HOOK_ARGS);
    if handler.get("args") != Some(&expected_args) {
        handler["args"] = expected_args;
        changed = true;
    }
    changed
}

fn hook_handler(writ_command: &str, condition: Option<&str>) -> Value {
    let mut handler = json!({
        "type": "command",
        "command": writ_command,
        "args": HOOK_ARGS,
    });
    if let Some(condition) = condition {
        handler["if"] = json!(condition);
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

        let first = install_settings(&path, "/opt/writ").unwrap();
        assert!(first.changed);
        let second = install_settings(&path, "/opt/writ").unwrap();
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
}
