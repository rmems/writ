//! Origin-remote binding: resolve a checkout's `origin` URL to GitHub identity.
//!
//! These helpers run `git config` against the checkout rather than operating on
//! strings, so they live apart from the pure parsers in `identity`.

use std::path::Path;
use std::process::Command;

use crate::error::{Error, PolicyCode, Result};

use super::identity::{
    is_supported_github_remote, normalize_github_repo_identity, normalize_github_repo_slug,
};

/// Resolve `origin` remote URL for a repo and return `owner/repo` if parseable.
///
/// Reads `remote.origin.url` from git config rather than `git remote get-url`,
/// which applies `url.*.insteadOf` rewrites. Identity must bind to the configured
/// GitHub remote (so a local fetch rewrite cannot spoof or hide the owner), and
/// filesystem `origin` URLs are still rejected by [`is_supported_github_remote`].
pub fn origin_github_slug(repo_dir: &Path) -> Result<String> {
    let url = origin_remote_url(repo_dir)?;
    normalize_github_repo_slug(&url).ok_or_else(|| Error::PolicyViolation {
        code: PolicyCode::GitDirUnavailable,
        message: format!("could not parse origin remote as GitHub owner/repo: {url}"),
    })
}

/// Resolve `origin` to a host-qualified `gh` repository selector.
///
/// Returns `HOST/OWNER/REPO` for enterprise remotes and `OWNER/REPO` for
/// github.com, so `gh --repo` targets the same host the checkout came from.
/// Returns `Ok(None)` when `origin` is missing or not a supported GitHub remote.
pub fn origin_github_repo_selector(repo_dir: &Path) -> Result<Option<String>> {
    let url = match origin_remote_url(repo_dir) {
        Ok(url) => url,
        Err(Error::Io { .. }) => return Ok(None),
        Err(err) => return Err(err),
    };
    Ok(normalize_github_repo_identity(&url).map(|(host, slug)| {
        if host == "github.com" {
            slug
        } else {
            format!("{host}/{slug}")
        }
    }))
}

fn origin_remote_url(repo_dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .map_err(|e| Error::Io {
            context: "resolve origin remote",
            source: e,
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::PolicyViolation {
            code: PolicyCode::GitDirUnavailable,
            message: format!("failed to resolve origin remote: {}", stderr.trim()),
        });
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !is_supported_github_remote(&url) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            message: format!("origin remote `{url}` is not a supported GitHub or enterprise URL"),
        });
    }
    Ok(url)
}
