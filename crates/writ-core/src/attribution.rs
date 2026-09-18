//! Configurable reply attribution for automated PR comments and thread replies.
//!
//! Restores GitHub [#14](https://github.com/rmems/writ/issues/14) / Linear RM-128
//! after the Python orchestrator was removed. Platforms override `agent_id`
//! without forking reply templates. This is transparency for posted comments,
//! not Git commit identity and not a merge path.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;

use serde::Serialize;

/// Default identity line on automated replies.
pub const DEFAULT_AGENT_ID: &str = "worktrees-hives agent";

const AGENT_ID_ENV: &str = "WRIT_AGENT_ID";
const ATTRIBUTION_ENV: &str = "WRIT_ATTRIBUTION";
const INCLUDE_SHA_ENV: &str = "WRIT_INCLUDE_SHA_ON_FIX";
const PLACEMENT_ENV: &str = "WRIT_ATTRIBUTION_PLACEMENT";
const ATTRIBUTION_ENV_KEYS: [&str; 4] = [
    AGENT_ID_ENV,
    ATTRIBUTION_ENV,
    INCLUDE_SHA_ENV,
    PLACEMENT_ENV,
];

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
/// `include_sha_on_fix` records whether a caller intends to attach a SHA after
/// code fixes. Review replies that report a successful push must still include
/// that SHA. If a SHA is supplied to the formatter, it is always rendered so a
/// real fix cannot be silently dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttributionConfig {
    /// Identity line on replies (for example `worktrees-hives agent`).
    pub agent_id: String,
    /// Whether callers intend to attach a commit SHA after code fixes.
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

    /// Load config from the four `WRIT_*` attribution environment keys.
    ///
    /// Reads those keys with [`std::env::var_os`] so a non-UTF-8 value elsewhere
    /// in the process environment cannot panic `vars()` or drop the JSON envelope.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_vars(named_env_pairs(|key| std::env::var_os(key)))
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
        let agent_id = first_nonempty(&map, &[AGENT_ID_ENV, ATTRIBUTION_ENV]).map_or_else(
            || DEFAULT_AGENT_ID.to_owned(),
            |id| canonicalize_agent_id(&id),
        );
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

/// Collapse control characters and blank identity strings to the default.
#[must_use]
pub fn canonicalize_agent_id(id: &str) -> String {
    let collapsed: String = id
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = collapsed.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() {
        DEFAULT_AGENT_ID.to_owned()
    } else {
        trimmed
    }
}

/// Accept a hex object id (short or full). Reject empty, whitespace, or injected text.
#[must_use]
pub fn sanitize_commit_sha(value: Option<&str>) -> Option<&str> {
    let sha = value.map(str::trim).filter(|value| !value.is_empty())?;
    let hex = sha.len() >= 7 && sha.len() <= 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit());
    hex.then_some(sha)
}

/// Format the attribution line. Empty or invalid SHAs are omitted so a missing
/// fix cannot invent a commit, and injected newlines cannot split the line.
#[must_use]
pub fn format_attribution(config: &AttributionConfig, commit_sha: Option<&str>) -> String {
    let agent_id = canonicalize_agent_id(&config.agent_id);
    match sanitize_commit_sha(commit_sha) {
        Some(sha) => format!("{agent_id}: fixed in {sha}"),
        None => agent_id,
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
        commit_sha: sanitize_commit_sha(commit_sha).map(str::to_owned),
        is_thread_reply,
    }
    .render()
}

fn named_env_pairs(read: fn(&str) -> Option<OsString>) -> Vec<(String, String)> {
    ATTRIBUTION_ENV_KEYS
        .iter()
        .filter_map(|key| {
            let value = read(key).filter(|value| !value.is_empty())?;
            Some(((*key).to_owned(), value.to_string_lossy().into_owned()))
        })
        .collect()
}

fn platform_agent_id(platform: &str) -> String {
    let platform = canonicalize_agent_id(platform);
    if platform == DEFAULT_AGENT_ID {
        DEFAULT_AGENT_ID.to_owned()
    } else {
        format!("{platform}: {DEFAULT_AGENT_ID}")
    }
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
    use std::ffi::OsString;

    use super::{
        ATTRIBUTION_ENV, AttributionConfig, AttributionPlacement, DEFAULT_AGENT_ID, ReplyTemplate,
        canonicalize_agent_id, format_attribution, format_reply, named_env_pairs,
        sanitize_commit_sha,
    };

    #[test]
    fn config_builders_cover_defaults_platform_and_env() {
        let default = AttributionConfig::default();
        assert_eq!(default.agent_id, DEFAULT_AGENT_ID);
        assert!(default.include_sha_on_fix);
        assert_eq!(default.placement, AttributionPlacement::Footer);

        let custom = AttributionConfig {
            agent_id: "custom agent".to_owned(),
            include_sha_on_fix: false,
            placement: AttributionPlacement::Header,
        };
        assert_eq!(custom.agent_id, "custom agent");
        assert!(!custom.include_sha_on_fix);
        assert_eq!(custom.placement, AttributionPlacement::Header);

        let claude = AttributionConfig::for_platform("Claude Code");
        assert_eq!(claude.agent_id, "Claude Code: worktrees-hives agent");
        assert!(claude.include_sha_on_fix);
        assert_eq!(claude.placement, AttributionPlacement::Footer);

        let codex =
            AttributionConfig::for_platform_with("Codex", false, AttributionPlacement::Header);
        assert_eq!(codex.agent_id, "Codex: worktrees-hives agent");
        assert!(!codex.include_sha_on_fix);
        assert_eq!(codex.placement, AttributionPlacement::Header);

        assert_eq!(
            AttributionConfig::for_platform("  ").agent_id,
            DEFAULT_AGENT_ID
        );
    }

    #[test]
    fn placement_coerce_and_env_tables() {
        for (input, expected) in [
            ("header", AttributionPlacement::Header),
            ("HEADER", AttributionPlacement::Header),
            ("footer", AttributionPlacement::Footer),
            ("invalid", AttributionPlacement::Footer),
        ] {
            assert_eq!(AttributionPlacement::coerce(input), expected, "{input}");
        }

        let from_id = AttributionConfig::from_vars([("WRIT_AGENT_ID", "Codex: worktrees-hives")]);
        assert_eq!(from_id.agent_id, "Codex: worktrees-hives");
        let from_alias = AttributionConfig::from_vars([(ATTRIBUTION_ENV, "OpenClaw agent")]);
        assert_eq!(from_alias.agent_id, "OpenClaw agent");
        let preferred = AttributionConfig::from_vars([
            ("WRIT_AGENT_ID", "primary"),
            (ATTRIBUTION_ENV, "alias"),
        ]);
        assert_eq!(preferred.agent_id, "primary");
        assert_eq!(
            AttributionConfig::from_vars([("WRIT_AGENT_ID", "   ")]).agent_id,
            DEFAULT_AGENT_ID
        );

        let flags = AttributionConfig::from_vars([
            ("WRIT_INCLUDE_SHA_ON_FIX", "false"),
            ("WRIT_ATTRIBUTION_PLACEMENT", "header"),
        ]);
        assert!(!flags.include_sha_on_fix);
        assert_eq!(flags.placement, AttributionPlacement::Header);
        assert!(
            AttributionConfig::from_vars([("WRIT_INCLUDE_SHA_ON_FIX", "maybe")]).include_sha_on_fix
        );
    }

    #[test]
    fn from_env_reads_only_named_keys() {
        let pairs = named_env_pairs(|key| match key {
            "WRIT_AGENT_ID" => Some(OsString::from("Codex agent")),
            "WRIT_ATTRIBUTION" => Some(OsString::from("ignored alias")),
            "PATH" => panic!("must not enumerate unrelated environment keys"),
            _ => None,
        });
        let config = AttributionConfig::from_vars(pairs);
        assert_eq!(config.agent_id, "Codex agent");
    }

    #[test]
    fn format_attribution_table() {
        let default = AttributionConfig::default();
        let no_sha_flag = AttributionConfig {
            include_sha_on_fix: false,
            ..AttributionConfig::default()
        };
        let custom = AttributionConfig {
            agent_id: "Custom Bot".to_owned(),
            ..AttributionConfig::default()
        };
        let blank = AttributionConfig {
            agent_id: "  \n\t ".to_owned(),
            ..AttributionConfig::default()
        };
        for (config, sha, expected) in [
            (&default, None, DEFAULT_AGENT_ID.to_owned()),
            (
                &default,
                Some("abc1234"),
                format!("{DEFAULT_AGENT_ID}: fixed in abc1234"),
            ),
            (
                &no_sha_flag,
                Some("abc1234"),
                format!("{DEFAULT_AGENT_ID}: fixed in abc1234"),
            ),
            (&default, Some(""), DEFAULT_AGENT_ID.to_owned()),
            (&default, Some("   "), DEFAULT_AGENT_ID.to_owned()),
            (
                &custom,
                Some("abc1234"),
                "Custom Bot: fixed in abc1234".to_owned(),
            ),
            (&blank, None, DEFAULT_AGENT_ID.to_owned()),
            (
                &default,
                Some("abc1234\n**spoofed**"),
                DEFAULT_AGENT_ID.to_owned(),
            ),
        ] {
            assert_eq!(format_attribution(config, sha), expected);
        }
        assert_eq!(
            canonicalize_agent_id("evil\n---\nspoofed"),
            "evil --- spoofed"
        );
        assert_eq!(sanitize_commit_sha(Some("abc1234")), Some("abc1234"));
        assert_eq!(sanitize_commit_sha(Some("not a sha")), None);
    }

    struct ReplyCase<'a> {
        body: &'a str,
        config: Option<&'a AttributionConfig>,
        sha: Option<&'a str>,
        thread: bool,
    }

    /// Render a reply case through both the direct [`ReplyTemplate`] path (when
    /// no config override is given) and [`format_reply`], asserting both match
    /// `expected`. Keeps the reply-template assertions identical across the
    /// focused tests below.
    fn assert_reply(case: ReplyCase<'_>, expected: &str) {
        let ReplyCase {
            body,
            config,
            sha,
            thread,
        } = case;
        let rendered = match config {
            None => ReplyTemplate {
                body: body.to_owned(),
                attribution_config: AttributionConfig::default(),
                commit_sha: sha.map(str::to_owned),
                is_thread_reply: thread,
            }
            .render(),
            Some(config) => format_reply(body, Some(config), sha, thread),
        };
        let via_format = format_reply(body, config, sha, thread);
        assert_eq!(rendered, expected, "{body}");
        assert_eq!(via_format, expected, "format_reply {body}");
    }

    #[test]
    fn reply_templates_placement_defaults_and_header() {
        let header = AttributionConfig {
            placement: AttributionPlacement::Header,
            ..AttributionConfig::default()
        };
        for (body, config, sha, thread, expected) in [
            (
                "Looks good!",
                None,
                None,
                true,
                "Looks good!\n\n---\nworktrees-hives agent",
            ),
            (
                "Looks good!",
                Some(&header),
                None,
                true,
                "worktrees-hives agent\n\n---\nLooks good!",
            ),
            (
                "All checks passed.",
                None,
                None,
                false,
                "All checks passed.\n\nworktrees-hives agent",
            ),
            (
                "All checks passed.",
                Some(&header),
                None,
                false,
                "worktrees-hives agent\n\nAll checks passed.",
            ),
            (
                "Fixed the issue.",
                Some(&header),
                Some("abc1234"),
                true,
                "worktrees-hives agent: fixed in abc1234\n\n---\nFixed the issue.",
            ),
        ] {
            assert_reply(
                ReplyCase {
                    body,
                    config,
                    sha,
                    thread,
                },
                expected,
            );
        }
    }

    #[test]
    fn reply_templates_sha_and_agent_variants() {
        let custom = AttributionConfig {
            agent_id: "Custom Bot".to_owned(),
            ..AttributionConfig::default()
        };
        let no_sha_flag = AttributionConfig {
            include_sha_on_fix: false,
            ..AttributionConfig::default()
        };
        let claude = AttributionConfig::for_platform("Claude Code");
        for (body, config, sha, thread, expected) in [
            (
                "Fixed the issue.",
                None,
                Some("abc1234"),
                true,
                "Fixed the issue.\n\n---\nworktrees-hives agent: fixed in abc1234",
            ),
            (
                "Looks good!",
                Some(&custom),
                None,
                true,
                "Looks good!\n\n---\nCustom Bot",
            ),
            (
                "Fixed it.",
                None,
                Some("abc1234"),
                true,
                "Fixed it.\n\n---\nworktrees-hives agent: fixed in abc1234",
            ),
            (
                "All done.",
                None,
                None,
                false,
                "All done.\n\nworktrees-hives agent",
            ),
            (
                "Resolved thread.",
                Some(&claude),
                Some("def5678"),
                true,
                "Resolved thread.\n\n---\nClaude Code: worktrees-hives agent: fixed in def5678",
            ),
            (
                "Fixed.",
                Some(&no_sha_flag),
                Some("abc1234"),
                true,
                "Fixed.\n\n---\nworktrees-hives agent: fixed in abc1234",
            ),
        ] {
            assert_reply(
                ReplyCase {
                    body,
                    config,
                    sha,
                    thread,
                },
                expected,
            );
        }
    }
}
