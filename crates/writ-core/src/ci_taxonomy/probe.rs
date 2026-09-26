//! Lowercased search text so matchers take a domain value, not raw strings.

use super::CheckEntry;

/// A set of already-lowercase needles.
pub(super) struct Fragments<'a>(pub(super) &'a [&'a str]);

/// Owned lowercase text used for vendor and conclusion matching.
pub(super) struct Probe {
    lower: String,
}

impl Probe {
    pub(super) fn lowercased(value: &str) -> Self {
        Self {
            lower: value.to_ascii_lowercase(),
        }
    }

    pub(super) fn from_entry_text(entry: &CheckEntry) -> Self {
        Self::lowercased(&format!(
            "{} {} {} {}",
            entry.name, entry.workflow_name, entry.description, entry.details_url
        ))
    }

    pub(super) fn from_details_url(entry: &CheckEntry) -> Self {
        Self::lowercased(&entry.details_url)
    }

    pub(super) fn contains_fragment(&self, needles: &Fragments<'_>) -> bool {
        needles.0.iter().any(|needle| self.lower.contains(needle))
    }

    pub(super) fn contains_word(&self, needles: &Fragments<'_>) -> bool {
        needles.0.iter().any(|needle| {
            self.lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|part| part == *needle)
        })
    }

    pub(super) fn text(&self) -> &str {
        &self.lower
    }

    pub(super) fn equals_any(&self, needles: &Fragments<'_>) -> bool {
        needles.0.iter().any(|needle| self.lower == *needle)
    }

    /// Stable slug: lowercase alphanumerics, other runs become one `_`, max 40.
    pub(super) fn slug(&self) -> String {
        let mut out = String::new();
        for ch in self.lower.chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
            } else if !out.ends_with('_') {
                out.push('_');
            }
        }
        let trimmed = out.trim_matches('_');
        if trimmed.is_empty() {
            return "check".to_owned();
        }
        trimmed.chars().take(40).collect()
    }

    /// GitHub Actions run id from `/actions/runs/<digits>`.
    pub(super) fn actions_run_id(&self) -> Option<u64> {
        let marker = "/actions/runs/";
        let idx = self.lower.find(marker)?;
        let rest = &self.lower[idx + marker.len()..];
        let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if id.is_empty() { None } else { id.parse().ok() }
    }
}
