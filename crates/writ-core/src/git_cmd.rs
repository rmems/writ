//! Shared `git -C <path> …` spawn used by hook admission and worktree identity.

use std::path::Path;
use std::process::{Command, Output};

pub(crate) fn git_in(repo: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git").arg("-C").arg(repo).args(args).output()
}
