//! GitHub repository identity and origin binding.

use std::path::Path;
use std::process::Command;

use crate::error::{Error, PolicyCode, Result};

pub fn normalize_github_repo_identity(spec: &str) -> Option<(String, String)> {
    let mut s = spec.trim().trim_end_matches('/').to_owned();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_suffix(".git") {
        s = rest.to_owned();
    }
    let mut host = String::from("github.com");
    // scp-like: git@host:owner/repo
    if let Some(at) = s.find('@')
        && let Some(rel) = s[at..].find(':')
    {
        let colon = at + rel;
        let host_part = &s[at + 1..colon];
        let after = &s[colon + 1..];
        if after.contains('/') && !after.contains("://") {
            if !host_part.is_empty() {
                host = host_part.to_ascii_lowercase();
            }
            s = after.to_owned();
        }
    }
    for prefix in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_owned();
            break;
        }
    }
    // Drop userinfo in URL path form user@host/owner/repo
    if let Some(at) = s.find('@')
        && !s[..at].contains('/')
    {
        s = s[at + 1..].to_owned();
    }
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    let (owner, repo) = match parts.as_slice() {
        [owner, repo] => (*owner, *repo),
        [h, owner, repo] => {
            // host/owner/repo or HOST/OWNER/REPO from -R
            if h.contains('.') || *h == "github.com" || h.contains(':') {
                host = h.split(':').next().unwrap_or(h).to_ascii_lowercase();
            }
            (*owner, *repo)
        }
        [h, _extra, owner, repo] => {
            host = h.split(':').next().unwrap_or(h).to_ascii_lowercase();
            (*owner, *repo)
        }
        _ => return None,
    };
    let owner = owner.split(':').next().unwrap_or(owner);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((
        host,
        format!(
            "{}/{}",
            owner.to_ascii_lowercase(),
            repo.to_ascii_lowercase()
        ),
    ))
}

/// Normalize to `owner/repo` (lowercase), dropping host. Prefer
/// [`normalize_github_repo_identity`] when host must be compared.
#[must_use]
pub fn normalize_github_repo_slug(spec: &str) -> Option<String> {
    normalize_github_repo_identity(spec).map(|(_, slug)| slug)
}

/// Whether two GitHub repo selectors refer to the same host + owner/repo.
#[must_use]
pub fn github_repo_slugs_match(a: &str, b: &str) -> bool {
    match (
        normalize_github_repo_identity(a),
        normalize_github_repo_identity(b),
    ) {
        (Some((ha, sa)), Some((hb, sb))) => ha == hb && sa == sb,
        _ => false,
    }
}

/// Extract a lowercase GitHub owner from a bare owner name or repo selector.
///
/// Uses the same identity parser as [`normalize_github_repo_identity`] so
/// `Acme/Repo` and `github.com/acme/repo` yield the same owner (`acme`). Bare
/// owner names that cannot be parsed as `owner/repo` slugs are lowercased as-is
/// when they do not look like a host or URL.
#[must_use]
pub fn github_owner_name(spec: &str) -> Option<String> {
    if let Some((_, slug)) = normalize_github_repo_identity(spec) {
        return slug.split('/').next().map(str::to_owned);
    }
    let owner = spec.trim().trim_matches('/');
    if owner.is_empty() {
        return None;
    }
    if owner.contains(['/', '@', ':']) || owner.contains('.') {
        return None;
    }
    Some(owner.to_ascii_lowercase())
}

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

/// True for HTTPS/SSH/git remotes and SCP-style `git@host:owner/repo` remotes.
///
/// Filesystem paths (`/tmp/acme/repo`, `file://…`) must not be trusted as a
/// GitHub owner identity: the permissive slug parser would otherwise treat a
/// three-component local path as `acme/repo`.
#[must_use]
pub fn is_supported_github_remote(url: &str) -> bool {
    let s = url.trim();
    if s.is_empty() {
        return false;
    }
    if s.starts_with("file:") || s.starts_with('/') || s.starts_with('\\') {
        return false;
    }
    if s.contains("://") {
        return s.starts_with("https://")
            || s.starts_with("http://")
            || s.starts_with("ssh://")
            || s.starts_with("git://");
    }
    // scp-like: [user@]host:owner/repo — not Windows drive letters (`C:…`).
    let Some((_, after)) = s.split_once(':') else {
        return false;
    };
    !after.starts_with('\\') && after.contains('/')
}
