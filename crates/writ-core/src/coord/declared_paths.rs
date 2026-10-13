//! Repo-relative declared-path canonicalization for overlap checks.

use super::{Error, Result};

pub(super) fn decode_paths_row(raw: String) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(error))
    })
}

pub(super) fn encode_paths(paths: &[String]) -> Result<String> {
    serde_json::to_string(paths).map_err(|error| Error::LeaseStore {
        context: "encode declared paths",
        message: error.to_string(),
    })
}

pub(super) fn normalize_paths(paths: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for path in paths {
        let raw = path.trim().replace('\\', "/");
        if raw.is_empty() {
            continue;
        }
        if is_absolute_declared(&raw) {
            return Err(invalid_path(path));
        }
        let cleaned = normalize_one(path);
        if cleaned.is_empty() {
            return Err(invalid_path(path));
        }
        if normalized.iter().any(|existing| existing == &cleaned) {
            continue;
        }
        normalized.push(cleaned);
    }
    Ok(normalized)
}

fn is_absolute_declared(raw: &str) -> bool {
    raw.starts_with('/') || raw.contains(':')
}

fn invalid_path(path: &str) -> Error {
    Error::LeaseStore {
        context: "normalize declared paths",
        message: format!("declared path `{path}` must be repository-relative"),
    }
}

fn normalize_one(path: &str) -> String {
    collapse_dots(&slash_form(path))
}

fn slash_form(path: &str) -> String {
    let trimmed = path.trim().replace('\\', "/");
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed == "." || trimmed == "./" {
        return ".".to_owned();
    }
    let mut stripped = trimmed.trim_start_matches("./").to_owned();
    if stripped.is_empty() || stripped == "." {
        return ".".to_owned();
    }
    while stripped.contains("//") {
        stripped = stripped.replace("//", "/");
    }
    stripped
}

fn collapse_dots(stripped: &str) -> String {
    if stripped.is_empty() {
        return String::new();
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in stripped.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return String::new();
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_paths;

    #[test]
    fn blanks_are_dropped_and_relative_paths_canonicalize() {
        let paths = normalize_paths(&[
            String::from("  "),
            String::from("crates/writ-core/src/./lib/../coord.rs"),
        ])
        .unwrap();
        assert_eq!(paths, vec!["crates/writ-core/src/coord.rs"]);
    }

    #[test]
    fn absolute_and_escaping_paths_are_rejected() {
        let abs = normalize_paths(&[String::from("/crates/x")]).unwrap_err();
        assert!(abs.to_string().contains("repository-relative"));
        let escape = normalize_paths(&[String::from("src/../../x")]).unwrap_err();
        assert!(escape.to_string().contains("repository-relative"));
    }
}
