//! Configured GitHub owner allowlist.
//!
//! Repository scope is an operator-supplied owner list (`WRIT_ALLOWED_OWNERS`,
//! legacy `WH_ALLOWED_OWNERS`, or an explicit per-call list). There is no
//! built-in default organization. An empty or unset list denies owner-taking
//! operations rather than permitting them.
//!
//! Owner comparison reuses [`crate::git_safe::github_owner_name`] so
//! `Acme/Repo` and `github.com/acme/repo` cannot diverge from
//! [`crate::git_safe::github_repo_slugs_match`].

use std::collections::BTreeSet;

use crate::error::{Error, PolicyCode, Result};
use crate::git_safe::github_owner_name;

const ALLOWED_OWNERS_ENV: &str = "WRIT_ALLOWED_OWNERS";
const LEGACY_ALLOWED_OWNERS_ENV: &str = "WH_ALLOWED_OWNERS";

/// Normalized set of GitHub owners permitted at the mutation boundary.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct OwnerAllowlist {
    owners: BTreeSet<String>,
}

/// What kind of value an owner-allowlist check is enforcing. Used only to phrase
/// the "could not be parsed from …" diagnostic; it replaces a stringly-typed
/// `kind: &str` discriminator so the message wording stays fixed at the call
/// sites rather than being passed in as free-form text.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum OwnerSpecKind {
    /// A bare owner name (e.g. the worktree `owner` argument).
    Owner,
    /// A `gh -R` / `--repo` repository selector.
    RepoSelector,
}

impl OwnerSpecKind {
    /// The label used in the parse-failure diagnostic. Wording is preserved
    /// exactly from the previous `&str` discriminator.
    fn label(self) -> &'static str {
        match self {
            OwnerSpecKind::Owner => "owner",
            OwnerSpecKind::RepoSelector => "repository selector",
        }
    }
}

impl OwnerAllowlist {
    /// Parse a comma-separated owner list (the env-var / `--allowed-owners` form).
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let owners = raw.split(',').filter_map(github_owner_name).collect();
        Self { owners }
    }

    /// Build an allowlist from explicit per-call owner names or selectors.
    #[must_use]
    pub fn from_owners<I, S>(owners: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let owners = owners
            .into_iter()
            .filter_map(|owner| github_owner_name(owner.as_ref()))
            .collect();
        Self { owners }
    }

    /// Read `WRIT_ALLOWED_OWNERS`, falling back to legacy `WH_ALLOWED_OWNERS`.
    ///
    /// Empty values are treated as unset. Unset or empty configuration yields an
    /// empty allowlist (deny-by-default).
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_values(
            nonempty_env(ALLOWED_OWNERS_ENV).as_deref(),
            nonempty_env(LEGACY_ALLOWED_OWNERS_ENV).as_deref(),
        )
    }

    /// Prefer an explicit CLI/API string when present; otherwise read the env.
    ///
    /// Passing `Some("")` is an explicit empty list and still denies, matching
    /// the documented alternative to the environment variable.
    #[must_use]
    pub fn from_cli_or_env(cli_owners: Option<&str>) -> Self {
        match cli_owners {
            Some(raw) => Self::parse(raw),
            None => Self::from_env(),
        }
    }

    #[must_use]
    fn from_env_values(primary: Option<&str>, legacy: Option<&str>) -> Self {
        if let Some(raw) = primary.filter(|value| !value.trim().is_empty()) {
            return Self::parse(raw);
        }
        if let Some(raw) = legacy.filter(|value| !value.trim().is_empty()) {
            return Self::parse(raw);
        }
        Self::default()
    }

    /// Whether the allowlist contains no owners.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }

    /// Reject `owner` unless it normalizes to an allowlisted owner.
    pub fn enforce_owner(&self, owner: &str) -> Result<()> {
        self.enforce_spec(OwnerSpecKind::Owner, owner)
    }

    /// Reject a `gh -R` / `--repo` selector unless its owner is allowlisted.
    pub fn enforce_repo_selector(&self, selector: &str) -> Result<()> {
        self.enforce_spec(OwnerSpecKind::RepoSelector, selector)
    }

    fn enforce_spec(&self, kind: OwnerSpecKind, spec: &str) -> Result<()> {
        if self.owners.is_empty() {
            return Err(Error::PolicyViolation {
                code: PolicyCode::OwnerNotAllowed,
                message:
                    "owner allowlist is empty; set WRIT_ALLOWED_OWNERS or pass --allowed-owners"
                        .to_owned(),
            });
        }
        let Some(normalized) = github_owner_name(spec) else {
            let kind = kind.label();
            return Err(Error::PolicyViolation {
                code: PolicyCode::OwnerNotAllowed,
                message: format!("GitHub owner could not be parsed from {kind} `{spec}`"),
            });
        };
        if self.owners.contains(&normalized) {
            return Ok(());
        }
        Err(Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            message: format!("owner `{normalized}` is not on the configured allowlist"),
        })
    }
}

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_owner_not_allowed(result: Result<()>) {
        assert!(matches!(
            result,
            Err(Error::PolicyViolation {
                code: PolicyCode::OwnerNotAllowed,
                ..
            })
        ));
    }

    #[test]
    fn empty_allowlist_denies_owner_and_selector() {
        let allowlist = OwnerAllowlist::default();
        assert!(allowlist.is_empty());
        assert_owner_not_allowed(allowlist.enforce_owner("acme"));
        assert_owner_not_allowed(allowlist.enforce_repo_selector("acme/repo"));
        assert_owner_not_allowed(OwnerAllowlist::parse("").enforce_owner("acme"));
        assert_owner_not_allowed(OwnerAllowlist::from_cli_or_env(Some("")).enforce_owner("acme"));
        assert_owner_not_allowed(OwnerAllowlist::from_env_values(None, None).enforce_owner("acme"));
        assert_owner_not_allowed(
            OwnerAllowlist::from_env_values(Some("   "), Some("")).enforce_owner("acme"),
        );
    }

    #[test]
    fn explicit_owners_are_honored_with_case_and_host_form() {
        let allowlist = OwnerAllowlist::from_owners(["Acme", "example-org"]);
        allowlist.enforce_owner("acme").unwrap();
        allowlist.enforce_owner("ACME").unwrap();
        allowlist.enforce_repo_selector("Acme/Repo").unwrap();
        allowlist
            .enforce_repo_selector("github.com/acme/repo")
            .unwrap();
        allowlist
            .enforce_repo_selector("https://github.com/Example-Org/tools.git")
            .unwrap();
        assert_owner_not_allowed(allowlist.enforce_owner("other"));
        assert_owner_not_allowed(allowlist.enforce_repo_selector("other/repo"));
    }

    #[test]
    fn slug_and_host_forms_cannot_diverge_from_github_repo_slugs_match() {
        assert!(crate::git_safe::github_repo_slugs_match(
            "Acme/Repo",
            "github.com/acme/repo"
        ));
        assert_eq!(
            github_owner_name("Acme/Repo"),
            github_owner_name("github.com/acme/repo")
        );
        let allowlist = OwnerAllowlist::parse("acme");
        allowlist.enforce_repo_selector("Acme/Repo").unwrap();
        allowlist
            .enforce_repo_selector("github.com/acme/repo")
            .unwrap();
        let denied = OwnerAllowlist::parse("other");
        assert_owner_not_allowed(denied.enforce_repo_selector("Acme/Repo"));
        assert_owner_not_allowed(denied.enforce_repo_selector("github.com/acme/repo"));
    }

    #[test]
    fn writ_env_wins_over_legacy_and_legacy_fills_when_writ_unset() {
        let prefer_writ = OwnerAllowlist::from_env_values(Some("acme"), Some("other"));
        prefer_writ.enforce_owner("acme").unwrap();
        assert_owner_not_allowed(prefer_writ.enforce_owner("other"));

        let legacy = OwnerAllowlist::from_env_values(None, Some("example-org"));
        legacy.enforce_owner("example-org").unwrap();
        assert_owner_not_allowed(legacy.enforce_owner("acme"));
    }

    #[test]
    fn unparseable_selector_is_denied() {
        let allowlist = OwnerAllowlist::parse("acme");
        assert_owner_not_allowed(allowlist.enforce_repo_selector(":::"));
        assert_owner_not_allowed(allowlist.enforce_owner(""));
    }
}
