//! Shared `git -C <path> …` spawn used by hook admission and worktree identity.
//!
//! Repository-configured command launchers (`core.fsmonitor`, `core.hooksPath`)
//! are disabled so read-only inspection cannot execute checkout-supplied code.

use std::path::Path;
use std::process::{Command, Output};

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

pub(crate) fn git_in(repo: &Path, args: &[&str]) -> std::io::Result<Output> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .arg("-c")
        .arg("core.fsmonitor=")
        .arg("-c")
        .arg(format!("core.hooksPath={}", null_device()))
        .args(args);
    command.output()
}
