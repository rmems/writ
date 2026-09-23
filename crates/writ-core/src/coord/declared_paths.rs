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

pub(super) fn normalize_paths(paths: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for path in paths {
        let cleaned = normalize_one(path);
        if cleaned.is_empty() || normalized.iter().any(|existing| existing == &cleaned) {
            continue;
        }
        normalized.push(cleaned);
    }
    normalized
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
