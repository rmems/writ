//! Atomic load/save for `watchlist.json`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::WatchlistError;
use super::schema::{WATCHLIST_VERSION, Watchlist};

const ALLOWED_OWNERS_ENV: &str = "WRIT_ALLOWED_OWNERS";
const LEGACY_ALLOWED_OWNERS_ENV: &str = "WH_ALLOWED_OWNERS";

/// Parse `WRIT_ALLOWED_OWNERS` (then `WH_ALLOWED_OWNERS`) as a comma-separated
/// owner list. Empty or unset means deny-by-default for multi-owner check-all.
#[must_use]
pub fn load_allowed_owners() -> Vec<String> {
    load_allowed_owners_from(
        std::env::var_os(ALLOWED_OWNERS_ENV),
        std::env::var_os(LEGACY_ALLOWED_OWNERS_ENV),
    )
}

/// Parse allowlist values from optional environment snapshots (test seam).
#[must_use]
pub fn load_allowed_owners_from(
    primary: Option<std::ffi::OsString>,
    legacy: Option<std::ffi::OsString>,
) -> Vec<String> {
    let raw = first_nonempty_os(primary).or_else(|| first_nonempty_os(legacy));
    let Some(raw) = raw else {
        return Vec::new();
    };
    let Ok(text) = raw.into_string() else {
        return Vec::new();
    };
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn first_nonempty_os(value: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    value.filter(|v| !v.is_empty())
}

/// True when `owner` is permitted. An empty allowlist denies every owner for
/// multi-owner operations such as `check-all`.
#[must_use]
pub fn owner_is_allowed(owner: &str, allowed: &[String]) -> bool {
    !allowed.is_empty() && allowed.iter().any(|want| want.eq_ignore_ascii_case(owner))
}

/// Load a watchlist. A missing file is an empty document (self-bootstrapping).
/// Corrupt JSON is quarantined and returned as [`WatchlistError::Corrupt`].
pub fn load_watchlist(path: &Path) -> Result<Watchlist, WatchlistError> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Watchlist::default()),
        Err(err) => {
            return Err(WatchlistError::Io {
                context: "read watchlist",
                path: path.to_path_buf(),
                source: err,
            });
        }
    };

    match serde_json::from_str::<Watchlist>(&data) {
        Ok(list) => {
            if list.version > WATCHLIST_VERSION {
                return Err(WatchlistError::UnsupportedVersion {
                    path: path.to_path_buf(),
                    version: list.version,
                });
            }
            Ok(list)
        }
        Err(err) => {
            let quarantine = quarantine_corrupt(path)?;
            Err(WatchlistError::Corrupt {
                path: path.to_path_buf(),
                quarantine,
                message: err.to_string(),
            })
        }
    }
}

/// Persist `list` with temp-file + rename. Creates the parent directory.
/// Sets POSIX mode `0o600` so titles stay local-only.
pub fn save_watchlist(path: &Path, list: &Watchlist) -> Result<(), WatchlistError> {
    let body = serde_json::to_vec_pretty(list).map_err(|err| WatchlistError::Serialize {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    atomic_write(path, &body)
}

/// Load, mutate, then save. Reloads immediately before `mutate` so a stale
/// in-memory snapshot is less likely to clobber a newer file. Do not run two
/// `check-all` writers against the same path.
pub fn mutate_watchlist<T>(
    path: &Path,
    mutate: impl FnOnce(&mut Watchlist) -> Result<T, WatchlistError>,
) -> Result<T, WatchlistError> {
    let mut list = load_watchlist(path)?;
    let result = mutate(&mut list)?;
    save_watchlist(path, &list)?;
    Ok(result)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), WatchlistError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = parent {
        fs::create_dir_all(dir).map_err(|err| WatchlistError::Io {
            context: "create watchlist directory",
            path: dir.to_path_buf(),
            source: err,
        })?;
    }
    let dir = parent.unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("watchlist.json"))
            .to_string_lossy(),
        std::process::id(),
        unix_nanos()
    ));

    let write_result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_mode(&tmp)?;
        fs::rename(&tmp, path)?;
        set_private_mode(path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result.map_err(|err| WatchlistError::Io {
        context: "atomic write watchlist",
        path: path.to_path_buf(),
        source: err,
    })
}

fn set_private_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let _ = path;
    Ok(())
}

fn quarantine_corrupt(path: &Path) -> Result<PathBuf, WatchlistError> {
    let stamp = utc_now_rfc3339().replace([':', '-'], "");
    let mut candidate = path.with_file_name(format!(
        "{}.corrupt.{stamp}",
        path.file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("watchlist.json"))
            .to_string_lossy()
    ));
    let mut n = 0_u32;
    while candidate.exists() {
        n += 1;
        candidate = path.with_file_name(format!(
            "{}.corrupt.{stamp}.{n}",
            path.file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("watchlist.json"))
                .to_string_lossy()
        ));
    }
    fs::rename(path, &candidate).map_err(|err| WatchlistError::Io {
        context: "quarantine corrupt watchlist",
        path: path.to_path_buf(),
        source: err,
    })?;
    Ok(candidate)
}

/// RFC3339 UTC timestamp for `added_at` / `last_checked`.
#[must_use]
pub fn utc_now_rfc3339() -> String {
    format_rfc3339(SystemTime::now())
}

#[must_use]
pub(crate) fn format_rfc3339(now: SystemTime) -> String {
    let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    let (year, month, day) = civil_ymd((secs / 86_400) as i64);
    let tod = secs % 86_400;
    let hour = tod / 3600;
    let min = (tod % 3600) / 60;
    let sec = tod % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Civil YYYY-MM-DD from days since Unix epoch (Howard Hinnant).
fn civil_ymd(unix_days: i64) -> (i32, u8, u8) {
    let z = unix_days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = u32::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u8, d as u8)
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watchlist::schema::{WatchEntry, WatchStatus};

    fn unique_path(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}.json",
            std::process::id(),
            unix_nanos()
        ))
    }

    fn sample_entry() -> WatchEntry {
        WatchEntry {
            repo: "acme/widgets".to_owned(),
            number: 7,
            branch: "fix/x".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 0,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: Some("main".to_owned()),
            title: Some("Fix x".to_owned()),
            added_at: Some("2026-01-01T00:00:00Z".to_owned()),
            check_count: Some(0),
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn missing_file_is_empty() {
        let path = unique_path("watchlist-missing");
        let _ = fs::remove_file(&path);
        let list = load_watchlist(&path).unwrap();
        assert!(list.prs.is_empty());
        assert_eq!(list.version, WATCHLIST_VERSION);
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = unique_path("watchlist-roundtrip");
        let _ = fs::remove_file(&path);
        let mut list = Watchlist::default();
        list.prs.push(sample_entry());
        save_watchlist(&path, &list).unwrap();
        let loaded = load_watchlist(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.prs.len(), 1);
        assert_eq!(loaded.prs[0].repo, "acme/widgets");
        assert_eq!(loaded.prs[0].number, 7);
    }

    #[test]
    fn save_creates_parent_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!(
            "watchlist-nested-{}-{}",
            std::process::id(),
            unix_nanos()
        ));
        let path = dir.join("nested").join("watchlist.json");
        save_watchlist(&path, &Watchlist::default()).unwrap();
        assert!(path.exists());
        let temps: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp-"))
            .collect();
        let _ = fs::remove_dir_all(&dir);
        assert!(temps.is_empty());
    }

    #[test]
    fn corrupt_file_is_quarantined() {
        let path = unique_path("watchlist-corrupt");
        fs::write(&path, "{not json").unwrap();
        let err = load_watchlist(&path).unwrap_err();
        match err {
            WatchlistError::Corrupt { quarantine, .. } => {
                assert!(quarantine.exists());
                assert!(!path.exists());
                let _ = fs::remove_file(quarantine);
            }
            other => panic!("expected corrupt, got {other}"),
        }
    }

    #[test]
    fn unsupported_version_is_not_silently_loaded() {
        let path = unique_path("watchlist-version");
        fs::write(&path, r#"{"version": 99, "prs": [], "groups": {}}"#).unwrap();
        let err = load_watchlist(&path).unwrap_err();
        let _ = fs::remove_file(&path);
        assert!(matches!(
            err,
            WatchlistError::UnsupportedVersion { version: 99, .. }
        ));
    }

    #[test]
    fn rfc3339_known_instants() {
        assert_eq!(format_rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_rfc3339(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
            "2023-11-14T22:13:20Z"
        );
    }

    #[test]
    fn allowlist_empty_denies() {
        assert!(!owner_is_allowed("acme", &[]));
        assert!(owner_is_allowed(
            "Acme",
            &["acme".to_owned(), "example-org".to_owned()]
        ));
        assert!(!owner_is_allowed("other", &["acme".to_owned()]));
    }

    #[test]
    fn allowlist_from_env_values() {
        let owners =
            load_allowed_owners_from(Some(std::ffi::OsString::from("acme, example-org")), None);
        assert_eq!(owners, vec!["acme".to_owned(), "example-org".to_owned()]);
        let owners = load_allowed_owners_from(None, Some(std::ffi::OsString::from("legacy")));
        assert_eq!(owners, vec!["legacy".to_owned()]);
        let owners = load_allowed_owners_from(Some(std::ffi::OsString::from("")), None);
        assert!(owners.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_mode_600() {
        use std::os::unix::fs::PermissionsExt;
        let path = unique_path("watchlist-mode");
        save_watchlist(&path, &Watchlist::default()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = fs::remove_file(&path);
        assert_eq!(mode, 0o600);
    }
}
