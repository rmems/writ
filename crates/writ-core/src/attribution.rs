//! Configurable reply attribution for automated PR comments and thread replies.
//!
//! Restores GitHub [#14](https://github.com/rmems/writ/issues/14) / Linear RM-128
//! after the Python orchestrator was removed. Platforms override `agent_id`
//! without forking reply templates. This is transparency for posted comments,
//! not Git commit identity and not a merge path.

use std::collections::HashMap;
use std::fmt;

use serde::Serialize;

/// Default identity line on automated replies.
pub const DEFAULT_AGENT_ID: &str = "worktrees-hives agent";

const AGENT_ID_ENV: &str = "WRIT_AGENT_ID";
const ATTRIBUTION_ENV: &str = "WRIT_ATTRIBUTION";
const INCLUDE_SHA_ENV: &str = "WRIT_INCLUDE_SHA_ON_FIX";
const PLACEMENT_ENV: &str = "WRIT_ATTRIBUTION_PLACEMENT";

/// Where the attribution line appears in a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AttributionPlacement {
    /// After the reply body (default).
    Footer,
    /// Before the reply body.
    Header,
}

impl AttributionPlacement {
    /// Normalize a string or already-typed value to a placement.
    ///
    /// Unknown values fall back to [`Self::Footer`] so a typo cannot drop
    /// attribution.
    #[must_use]
    pub fn coerce(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "header" => Self::Header,
            _ => Self::Footer,
        }
    }
}

impl fmt::Display for AttributionPlacement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Footer => "footer",
            Self::Header => "header",
        })
    }
}

/// Configuration for reply attribution.
///
/// `include_sha_on_fix` is caller policy: omit `commit_sha` when no fix was
/// pushed or when SHA attachment is disabled. If a SHA is supplied to the
/// formatter, it is always rendered so a real fix cannot be silently dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttributionConfig {
    /// Identity line on replies (for example `worktrees-hives agent`).
    pub agent_id: String,
    /// Whether callers should attach a commit SHA after code fixes.
    pub include_sha_on_fix: bool,
    /// Where the attribution line appears.
    pub placement: AttributionPlacement,
}

impl Default for AttributionConfig {
    fn default() -> Self {
        Self {
            agent_id: DEFAULT_AGENT_ID.to_owned(),
            include_sha_on_fix: true,
            placement: AttributionPlacement::Footer,
        }
    }
}

impl AttributionConfig {
    /// Config with a platform-specific `agent_id`.
    ///
    /// Uses `{platform}: worktrees-hives agent` so a later `: fixed in <sha>`
    /// suffix contains a single colon separator.
    #[must_use]
    pub fn for_platform(platform: &str) -> Self {
        Self::for_platform_with(platform, true, AttributionPlacement::Footer)
    }

    /// Platform factory with explicit SHA and placement overrides.
    #[must_use]
    pub fn for_platform_with(
        platform: &str,
        include_sha_on_fix: bool,
        placement: AttributionPlacement,
    ) -> Self {
        Self {
            agent_id: platform_agent_id(platform),
            include_sha_on_fix,
            placement,
        }
    }

    /// Load config from process environment (`WRIT_AGENT_ID`, `WRIT_ATTRIBUTION`,
    /// `WRIT_INCLUDE_SHA_ON_FIX`, `WRIT_ATTRIBUTION_PLACEMENT`).
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_vars(std::env::vars())
    }

    /// Load config from an explicit key/value iterator (testable without mutating
    /// process environment).
    #[must_use]
    pub fn from_vars<I, K, V>(vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let map: HashMap<String, String> = vars
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_owned(), v.as_ref().to_owned()))
            .collect();
        let agent_id = first_nonempty(&map, &[AGENT_ID_ENV, ATTRIBUTION_ENV])
            .map_or_else(|| DEFAULT_AGENT_ID.to_owned(), normalize_agent_id);
        Self {
            agent_id,
            include_sha_on_fix: parse_bool_env(map.get(INCLUDE_SHA_ENV)).unwrap_or(true),
            placement: map
                .get(PLACEMENT_ENV)
                .map_or(AttributionPlacement::Footer, |value| {
                    AttributionPlacement::coerce(value)
                }),
        }
    }
}

/// Format the attribution line. Empty or whitespace-only SHAs are omitted so a
/// missing fix cannot invent a commit.
#[must_use]
pub fn format_attribution(config: &AttributionConfig, commit_sha: Option<&str>) -> String {
    match nonempty(commit_sha) {
        Some(sha) => format!("{}: fixed in {sha}", config.agent_id),
        None => config.agent_id.clone(),
    }
}

/// Template for an automated thread reply or PR-level summary comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyTemplate {
    /// Main reply content. Must still be substantive; attribution is not a body.
    pub body: String,
    /// Attribution configuration.
    pub attribution_config: AttributionConfig,
    /// Commit SHA after a successful push; `None` when no code change landed.
    pub commit_sha: Option<String>,
    /// Thread replies use a `---` separator; PR comments use a blank line.
    pub is_thread_reply: bool,
}

impl ReplyTemplate {
    /// Render the full reply with attribution placed according to config.
    #[must_use]
    pub fn render(&self) -> String {
        let attribution = format_attribution(&self.attribution_config, self.commit_sha.as_deref());
        let separator = if self.is_thread_reply {
            "\n---\n"
        } else {
            "\n"
        };
        match self.attribution_config.placement {
            AttributionPlacement::Header => {
                format!("{attribution}\n{separator}{}", self.body)
            }
            AttributionPlacement::Footer => {
                format!("{}\n{separator}{attribution}", self.body)
            }
        }
    }
}

/// Format a reply with attribution, using defaults when `config` is omitted.
#[must_use]
pub fn format_reply(
    body: impl Into<String>,
    config: Option<&AttributionConfig>,
    commit_sha: Option<&str>,
    is_thread_reply: bool,
) -> String {
    ReplyTemplate {
        body: body.into(),
        attribution_config: config.cloned().unwrap_or_default(),
        commit_sha: nonempty(commit_sha).map(str::to_owned),
        is_thread_reply,
    }
    .render()
}

fn platform_agent_id(platform: &str) -> String {
    let platform = platform.trim();
    if platform.is_empty() {
        DEFAULT_AGENT_ID.to_owned()
    } else {
        format!("{platform}: {DEFAULT_AGENT_ID}")
    }
}

fn normalize_agent_id(id: String) -> String {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        DEFAULT_AGENT_ID.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn first_nonempty(map: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn parse_bool_env(value: Option<&String>) -> Option<bool> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ATTRIBUTION_ENV, AttributionConfig, AttributionPlacement, DEFAULT_AGENT_ID, ReplyTemplate,
        format_attribution, format_reply,
    };

    #[test]
    fn default_values() {
        let config = AttributionConfig::default();
        assert_eq!(config.agent_id, DEFAULT_AGENT_ID);
        assert!(config.include_sha_on_fix);
        assert_eq!(config.placement, AttributionPlacement::Footer);
    }

    #[test]
    fn custom_values() {
        let config = AttributionConfig {
            agent_id: "custom agent".to_owned(),
            include_sha_on_fix: false,
            placement: AttributionPlacement::Header,
        };
        assert_eq!(config.agent_id, "custom agent");
        assert!(!config.include_sha_on_fix);
        assert_eq!(config.placement, AttributionPlacement::Header);
    }

    #[test]
    fn for_platform() {
        let config = AttributionConfig::for_platform("Claude Code");
        assert_eq!(config.agent_id, "Claude Code: worktrees-hives agent");
        assert!(config.include_sha_on_fix);
        assert_eq!(config.placement, AttributionPlacement::Footer);
    }

    #[test]
    fn for_platform_with_overrides() {
        let config =
            AttributionConfig::for_platform_with("Codex", false, AttributionPlacement::Header);
        assert_eq!(config.agent_id, "Codex: worktrees-hives agent");
        assert!(!config.include_sha_on_fix);
        assert_eq!(config.placement, AttributionPlacement::Header);
    }

    #[test]
    fn for_platform_empty_falls_back_to_default() {
        let config = AttributionConfig::for_platform("  ");
        assert_eq!(config.agent_id, DEFAULT_AGENT_ID);
    }

    #[test]
    fn coerce_string_header() {
        assert_eq!(
            AttributionPlacement::coerce("header"),
            AttributionPlacement::Header
        );
        assert_eq!(
            AttributionPlacement::coerce("HEADER"),
            AttributionPlacement::Header
        );
    }

    #[test]
    fn coerce_string_footer() {
        assert_eq!(
            AttributionPlacement::coerce("footer"),
            AttributionPlacement::Footer
        );
    }

    #[test]
    fn coerce_invalid_string() {
        assert_eq!(
            AttributionPlacement::coerce("invalid"),
            AttributionPlacement::Footer
        );
    }

    #[test]
    fn format_without_sha() {
        let config = AttributionConfig::default();
        assert_eq!(format_attribution(&config, None), DEFAULT_AGENT_ID);
    }

    #[test]
    fn format_with_sha() {
        let config = AttributionConfig::default();
        assert_eq!(
            format_attribution(&config, Some("abc1234")),
            format!("{DEFAULT_AGENT_ID}: fixed in abc1234")
        );
    }

    #[test]
    fn format_with_sha_always_included() {
        let config = AttributionConfig {
            include_sha_on_fix: false,
            ..AttributionConfig::default()
        };
        assert_eq!(
            format_attribution(&config, Some("abc1234")),
            format!("{DEFAULT_AGENT_ID}: fixed in abc1234")
        );
    }

    #[test]
    fn format_with_empty_or_whitespace_sha_does_not_invent() {
        let config = AttributionConfig::default();
        assert_eq!(format_attribution(&config, Some("")), DEFAULT_AGENT_ID);
        assert_eq!(format_attribution(&config, Some("   ")), DEFAULT_AGENT_ID);
    }

    #[test]
    fn format_custom_agent_id() {
        let config = AttributionConfig {
            agent_id: "Custom Bot".to_owned(),
            ..AttributionConfig::default()
        };
        assert_eq!(
            format_attribution(&config, Some("abc1234")),
            "Custom Bot: fixed in abc1234"
        );
    }

    #[test]
    fn thread_reply_footer() {
        let template = ReplyTemplate {
            body: "Looks good!".to_owned(),
            attribution_config: AttributionConfig::default(),
            commit_sha: None,
            is_thread_reply: true,
        };
        assert_eq!(
            template.render(),
            "Looks good!\n\n---\nworktrees-hives agent"
        );
    }

    #[test]
    fn thread_reply_header() {
        let template = ReplyTemplate {
            body: "Looks good!".to_owned(),
            attribution_config: AttributionConfig {
                placement: AttributionPlacement::Header,
                ..AttributionConfig::default()
            },
            commit_sha: None,
            is_thread_reply: true,
        };
        assert_eq!(
            template.render(),
            "worktrees-hives agent\n\n---\nLooks good!"
        );
    }

    #[test]
    fn pr_comment_footer() {
        let template = ReplyTemplate {
            body: "All checks passed.".to_owned(),
            attribution_config: AttributionConfig::default(),
            commit_sha: None,
            is_thread_reply: false,
        };
        assert_eq!(
            template.render(),
            "All checks passed.\n\nworktrees-hives agent"
        );
    }

    #[test]
    fn pr_comment_header() {
        let template = ReplyTemplate {
            body: "All checks passed.".to_owned(),
            attribution_config: AttributionConfig {
                placement: AttributionPlacement::Header,
                ..AttributionConfig::default()
            },
            commit_sha: None,
            is_thread_reply: false,
        };
        assert_eq!(
            template.render(),
            "worktrees-hives agent\n\nAll checks passed."
        );
    }

    #[test]
    fn render_with_commit_sha() {
        let template = ReplyTemplate {
            body: "Fixed the issue.".to_owned(),
            attribution_config: AttributionConfig::default(),
            commit_sha: Some("abc1234".to_owned()),
            is_thread_reply: true,
        };
        assert_eq!(
            template.render(),
            "Fixed the issue.\n\n---\nworktrees-hives agent: fixed in abc1234"
        );
    }

    #[test]
    fn render_with_commit_sha_header() {
        let template = ReplyTemplate {
            body: "Fixed the issue.".to_owned(),
            attribution_config: AttributionConfig {
                placement: AttributionPlacement::Header,
                ..AttributionConfig::default()
            },
            commit_sha: Some("abc1234".to_owned()),
            is_thread_reply: true,
        };
        assert_eq!(
            template.render(),
            "worktrees-hives agent: fixed in abc1234\n\n---\nFixed the issue."
        );
    }

    #[test]
    fn format_reply_defaults() {
        assert_eq!(
            format_reply("Looks good!", None, None, true),
            "Looks good!\n\n---\nworktrees-hives agent"
        );
    }

    #[test]
    fn format_reply_with_config() {
        let config = AttributionConfig {
            agent_id: "Custom Bot".to_owned(),
            ..AttributionConfig::default()
        };
        assert_eq!(
            format_reply("Looks good!", Some(&config), None, true),
            "Looks good!\n\n---\nCustom Bot"
        );
    }

    #[test]
    fn format_reply_with_commit_sha() {
        assert_eq!(
            format_reply("Fixed it.", None, Some("abc1234"), true),
            "Fixed it.\n\n---\nworktrees-hives agent: fixed in abc1234"
        );
    }

    #[test]
    fn format_reply_pr_comment() {
        assert_eq!(
            format_reply("All done.", None, None, false),
            "All done.\n\nworktrees-hives agent"
        );
    }

    #[test]
    fn format_reply_full_platform_scenario() {
        let config = AttributionConfig::for_platform("Claude Code");
        let result = format_reply("Resolved thread.", Some(&config), Some("def5678"), true);
        assert_eq!(
            result,
            "Resolved thread.\n\n---\nClaude Code: worktrees-hives agent: fixed in def5678"
        );
    }

    #[test]
    fn format_reply_sha_always_included_when_provided() {
        let config = AttributionConfig {
            include_sha_on_fix: false,
            ..AttributionConfig::default()
        };
        assert_eq!(
            format_reply("Fixed.", Some(&config), Some("abc1234"), true),
            "Fixed.\n\n---\nworktrees-hives agent: fixed in abc1234"
        );
    }

    #[test]
    fn from_vars_reads_agent_id_and_alias() {
        let from_id = AttributionConfig::from_vars([("WRIT_AGENT_ID", "Codex: worktrees-hives")]);
        assert_eq!(from_id.agent_id, "Codex: worktrees-hives");

        let from_alias = AttributionConfig::from_vars([(ATTRIBUTION_ENV, "OpenClaw agent")]);
        assert_eq!(from_alias.agent_id, "OpenClaw agent");
    }

    #[test]
    fn from_vars_prefers_agent_id_over_alias() {
        let config = AttributionConfig::from_vars([
            ("WRIT_AGENT_ID", "primary"),
            (ATTRIBUTION_ENV, "alias"),
        ]);
        assert_eq!(config.agent_id, "primary");
    }

    #[test]
    fn from_vars_empty_agent_id_is_not_empty_noise() {
        let config = AttributionConfig::from_vars([("WRIT_AGENT_ID", "   ")]);
        assert_eq!(config.agent_id, DEFAULT_AGENT_ID);
    }

    #[test]
    fn from_vars_bool_and_placement() {
        let config = AttributionConfig::from_vars([
            ("WRIT_INCLUDE_SHA_ON_FIX", "false"),
            ("WRIT_ATTRIBUTION_PLACEMENT", "header"),
        ]);
        assert!(!config.include_sha_on_fix);
        assert_eq!(config.placement, AttributionPlacement::Header);
    }

    #[test]
    fn from_vars_invalid_bool_keeps_default() {
        let config = AttributionConfig::from_vars([("WRIT_INCLUDE_SHA_ON_FIX", "maybe")]);
        assert!(config.include_sha_on_fix);
    }
}
