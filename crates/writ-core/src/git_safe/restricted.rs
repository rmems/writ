//! Restricted git spawn used by PR-head import.

use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

use super::{GitOutput, SafeGitCommand};

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

fn restrict_git_environment(command: &mut Command) {
    let inherited: Vec<_> = std::env::vars_os().collect();
    for (key, value) in inherited {
        let Some(name) = key.to_str() else {
            command.env_remove(&key);
            continue;
        };
        if matches!(name, "GIT_SSL_CAINFO" | "GIT_SSL_CAPATH") {
            command.env(&key, value);
            continue;
        }
        if name.starts_with("GIT_") {
            command.env_remove(&key);
        }
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_PROTOCOL_FROM_USER", "0");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_CONFIG_GLOBAL", null_device());
    command.env("GIT_ASKPASS", "");
    command.env("GIT_PAGER", "cat");
}

/// Spawn an allowlisted git argv with helper/hook/config overrides disabled.
///
/// Global `-c` flags are applied *before* the subcommand so they cannot be
/// smuggled as fetch operands. Inherited `GIT_*` helper and namespace
/// overrides are stripped except for SSL certificate location.
pub(crate) fn run_allowlisted_git_restricted(
    repo_dir: &Path,
    args: &[String],
) -> Result<GitOutput> {
    run_allowlisted_git_restricted_with_file(repo_dir, args, false)
}

/// Same as [`run_allowlisted_git_restricted`], optionally allowing the `file`
/// transport for an already-validated local `origin` path.
pub(crate) fn run_allowlisted_git_restricted_with_file(
    repo_dir: &Path,
    args: &[String],
    allow_file_protocol: bool,
) -> Result<GitOutput> {
    let cmd = SafeGitCommand::new(args)?;
    let mut command = Command::new("git");
    command.arg("-C").arg(repo_dir);
    command.arg("-c").arg("protocol.ext.allow=never");
    command
        .arg("-c")
        .arg(format!("core.hooksPath={}", null_device()));
    command.arg("-c").arg("core.fsmonitor=");
    command.arg("-c").arg("fetch.fsckObjects=true");
    command.arg("-c").arg("transfer.fsckObjects=true");
    if allow_file_protocol {
        command.arg("-c").arg("protocol.file.allow=always");
    }
    command.args(cmd.args());
    restrict_git_environment(&mut command);
    let output = command.output().map_err(|e| Error::Io {
        context: "spawn restricted git",
        source: e,
    })?;
    Ok(GitOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(1),
    })
}
