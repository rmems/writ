//! Platform-aware data paths and sandbox validation.
//!
//! Worktree path derivation and escape prevention are implemented by GitHub #25.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Resolve the platform default user data directory.
///
/// - Windows: `%APPDATA%` (fallback: `%USERPROFILE%\AppData\Roaming`)
/// - macOS: `~/Library/Application Support`
/// - Unix/Linux: `$XDG_DATA_HOME` or `~/.local/share`
///
/// Empty environment values are treated as unset so resolution falls back cleanly.
#[must_use]
pub fn user_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
            return PathBuf::from(appdata);
        }
        if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
            return PathBuf::from(profile).join("AppData").join("Roaming");
        }
        return std::env::temp_dir();
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support");
        }
        return std::env::temp_dir();
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        // Empty XDG_DATA_HOME must not become a relative CWD-local root.
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(xdg);
        }
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(home).join(".local").join("share");
        }
        std::env::temp_dir()
    }
}

/// Named root for writ durable state under the user data directory.
///
/// Default layout: `{user_data_dir}/writ/`.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StateRoot {
    path: PathBuf,
}

impl StateRoot {
    /// Default state root under the platform user-data directory.
    #[must_use]
    pub fn default_root() -> Self {
        Self {
            path: user_data_dir().join("writ"),
        }
    }

    /// Construct a state root from an explicit directory.
    #[must_use]
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Absolute path to this state root.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Path to the watched-jobs store (`watched.json`) under this root.
    #[must_use]
    pub fn watched_json(&self) -> PathBuf {
        self.path.join("watched.json")
    }
}

/// Resolve the watched-jobs state path from an optional `WRIT_STATE_PATH` override.
///
/// When `writ_state_path` is `Some` and non-empty, that value is used (same as setting the
/// env var). Empty overrides are treated as unset. Non-UTF-8 paths are preserved via
/// [`OsStr`].
///
/// Otherwise defaults to [`StateRoot::default_root()`]'s `watched.json`.
#[must_use]
pub fn resolve_state_path(writ_state_path: Option<&OsStr>) -> PathBuf {
    if let Some(custom) = writ_state_path.filter(|v| !v.is_empty()) {
        return PathBuf::from(custom);
    }
    StateRoot::default_root().watched_json()
}

/// Resolve the path to the watched-jobs state file.
///
/// Honours `WRIT_STATE_PATH` if set (including non-UTF-8 values on Unix); otherwise defaults
/// to [`StateRoot::default_root()`]'s `watched.json`. Empty values are treated as unset.
#[must_use]
pub fn state_path() -> PathBuf {
    resolve_state_path(std::env::var_os("WRIT_STATE_PATH").as_deref())
}

const WORKTREE_BASE_ENV: &str = "WRIT_WORKTREE_BASE";

/// Resolve the configured worktree base path.
///
/// Uses `WRIT_WORKTREE_BASE` when set, otherwise
/// `{user_data_dir}/writ/worktrees`.
pub fn worktree_base_path() -> crate::error::Result<PathBuf> {
    if let Some(value) = std::env::var_os(WORKTREE_BASE_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    Ok(user_data_dir().join("writ").join("worktrees"))
}

/// Derive a sandboxed worktree path: `{base}/{owner}/{repo}/{job_id}`.
pub fn derive_worktree_path(
    base: &Path,
    owner: &str,
    repo: &str,
    job_id: &str,
) -> crate::error::Result<PathBuf> {
    validate_path_segment("owner", owner)?;
    validate_path_segment("repo", repo)?;
    validate_path_segment("job_id", job_id)?;
    Ok(base.join(owner).join(repo).join(job_id))
}

/// Strip Windows verbatim (`\\?\`) prefixes that [`std::fs::canonicalize`] adds.
///
/// Git for Windows rejects `\\?\C:\...` paths for `worktree add` with
/// "could not create leading directories ... Invalid argument". Keep resolved
/// paths in the non-verbatim form that external tools accept.
#[must_use]
pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::VerbatimDisk(drive) => {
                    let mut out = PathBuf::new();
                    out.push(format!("{}:", drive as char));
                    out.push(components.as_path());
                    return out;
                }
                Prefix::VerbatimUNC(server, share) => {
                    let mut out = PathBuf::from(format!(
                        r"\\{}\{}",
                        server.to_string_lossy(),
                        share.to_string_lossy()
                    ));
                    out.push(components.as_path());
                    return out;
                }
                Prefix::Verbatim(_) => {
                    // Best-effort: fall through to OsStr strip below.
                }
                _ => return path,
            },
            _ => return path,
        }
        // Fallback string strip for Verbatim and odd cases.
        let raw = path.as_os_str().to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{rest}"))
        } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
            PathBuf::from(rest)
        } else {
            path
        }
    }
    #[cfg(not(windows))]
    {
        path
    }
}

/// Canonicalize a path and strip Windows verbatim prefixes for tool consumption.
pub fn canonicalize_for_tools(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(strip_verbatim_prefix(canonical))
}

fn validate_path_segment(field: &'static str, value: &str) -> crate::error::Result<()> {
    use crate::error::Error;
    let invalid = value.is_empty()
        || value == "."
        || value == ".."
        || value.chars().any(std::path::is_separator)
        || Path::new(value).components().count() != 1
        || !matches!(
            Path::new(value).components().next(),
            Some(std::path::Component::Normal(s)) if s == value
        );
    if invalid {
        return Err(Error::InvalidSegment {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use super::{
        StateRoot, derive_worktree_path, resolve_state_path, user_data_dir, worktree_base_path,
    };

    #[test]
    fn user_data_dir_is_non_empty() {
        let dir = user_data_dir();
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn state_root_watched_json_joins_filename() {
        let root = StateRoot::from_path("/tmp/writ-state");
        assert_eq!(
            root.watched_json(),
            PathBuf::from("/tmp/writ-state/watched.json")
        );
    }

    #[test]
    fn resolve_state_path_honours_wh_state_path_override() {
        let path = resolve_state_path(Some(OsStr::new("/tmp/acme/watched.json")));
        assert_eq!(path, PathBuf::from("/tmp/acme/watched.json"));
    }

    #[test]
    fn resolve_state_path_empty_override_uses_default() {
        let path = resolve_state_path(Some(OsStr::new("")));
        assert!(path.ends_with("writ/watched.json") || path.ends_with("writ\\watched.json"));
    }

    #[test]
    fn resolve_state_path_default_uses_state_root() {
        let path = resolve_state_path(None);
        assert!(path.ends_with("writ/watched.json") || path.ends_with("writ\\watched.json"));
    }

    #[test]
    fn derive_worktree_path_joins_segments() {
        let path = derive_worktree_path(PathBuf::from("/base").as_path(), "o", "r", "j").unwrap();
        assert_eq!(path, PathBuf::from("/base/o/r/j"));
    }

    #[test]
    fn derive_worktree_path_rejects_escape() {
        assert!(derive_worktree_path(PathBuf::from("/b").as_path(), "..", "r", "j").is_err());
        assert!(derive_worktree_path(PathBuf::from("/b").as_path(), "o", "a/b", "j").is_err());
    }

    #[test]
    fn worktree_base_path_default_under_user_data() {
        // Ensure no override for this process snapshot (may already be set in CI).
        let path = worktree_base_path().unwrap();
        assert!(!path.as_os_str().is_empty());
    }
}
