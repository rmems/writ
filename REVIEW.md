# Review guide

This guide defines the review and pull-request lifecycle for `writ`. It applies to human-authored and agent-authored changes.

## Lifecycle

```text
issue -> claimed job -> isolated worktree -> focused commits -> pull request
      -> companion-skill interactive monitoring -> merge-ready report -> human merge decision
      -> human merges on GitHub (repository protection owns the merge)
```

The orchestrator, unattended runtime, installed companion `babysit-pr` skill, and worker agents may prepare or monitor a pull request and report that it is merge-ready, but they never merge the pull request or claim that an automated merge will occur. The companion skill is guidance, not an enforcement boundary. Local feature-branch integration in an assigned worktree is allowed; GitHub owns remote PR merges. Auto-merge and merge queues remain forbidden. If `main` is unprotected, report that operator gap rather than rebuilding a writ merge-permission protocol.

For stacked pull requests, review and fix the bottom PR before its children. Re-evaluate children after their base changes.

## Required checklist

### Scope and traceability

- [ ] The PR links its GitHub issue and, when present, the matching Linear `RM-*` issue.
- [ ] Changes satisfy the linked acceptance criteria without unrelated refactors.
- [ ] Session work is reflected accurately in Beads.
- [ ] Generated replies identify the agent. Review-fix replies include the pushed commit SHA. Coordination messages and no-code-change replies do not invent a SHA.
- [ ] Attribution is audited only on commits reachable from the submitted PR head and not its base, excluding synthetic review-merge/checkout and test-fixture commits. The Git author is the primary author, `Co-authored-by` credits an additional contributor, and `Agent` identifies the coding agent; the presence of one does not prove another.
- [ ] Every Codex-authored commit in that submitted range contains exact `Agent: Codex` and `Co-authored-by: Codex <noreply@openai.com>` trailers without rewriting Cursor-attributed history.
- [ ] After the first tested implementation and before final publication, one independent review matched to the change's risk was completed. Extra review was requested only for a named high-risk boundary or a reproduced finding that warranted follow-up.

### Safety

- [ ] No command, API call, prompt, or documentation path can merge a GitHub PR automatically, enqueue it, or schedule a deferred merge.
- [ ] Local `git merge` on an assigned feature branch is allowlisted; default-branch merges, dirty-WIP merges, `git mergetool`, and `gh pr merge` remain blocked.
- [ ] Force pushing accepts only `--force-with-lease`; bare `--force` and `-f` are rejected.
- [ ] Mutating operations verify the expected job branch.
- [ ] Paths are derived under the configured worktree base and reject traversal or escape.
- [ ] Agents edit only the assigned branch and isolated worktree.
- [ ] Owner allowlist is configuration-only (env/API); no hard-coded personal/org defaults in product code or docs.
- [ ] Credentials, tokens, and sensitive subprocess data are absent from logs and reports.

### Behavior and compatibility

- [ ] JSON output follows the documented envelope and keeps stdout machine-readable.
- [ ] Errors are actionable and policy rejections map to exit code 2.
- [ ] Cross-platform path and process behavior does not assume a Linux-only environment.
- [ ] New behavior has focused tests, including negative policy tests where relevant.
- [ ] Documentation and examples match the implemented command surface.
- [ ] Documentation command blocks preserve their caller's working directory when steps run sequentially.

## Language-specific review notes

`writ` is a Rust workspace. Each area has distinct review concerns:

### Rust review notes

Rust owns the hard safety boundary. Reviewers should check:

- Policy is enforced in `writ-core`, not only in `clap` argument definitions.
- Git and GitHub operations use explicit allowlists and structured argument vectors rather than shell command strings.
- `gh pr merge` and merge-oriented `gh api` requests are impossible through product runtime public interfaces. Local feature-branch `git merge` is allowlisted with default-branch and dirty-WIP guards.
- Branch verification occurs immediately before mutation to reduce time-of-check/time-of-use risk.
- Canonicalization and component checks prevent `..`, symlink, or prefix-based path escape.
- State writes for the hook lease store go through SQLite (`rusqlite` bundled). `watched.json` remains a read-only legacy path; do not add a writer for it.
- Process timeouts terminate and reap children. This is implemented in `supervisor.rs` (wall-clock timeout inclusive of permit wait, process-group kill on drop on Unix -- Windows kills only the direct child, per the `supervisor.rs` platform notes -- and wrapper/interpreter rejection); changes to it require timeout and reaping tests, not a deferral.
- Public error codes are stable enough for callers to classify without parsing prose.
- Unsafe Rust remains forbidden unless a separately reviewed design justifies it.

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```


## Review replies

Post a review-fix reply only after its commit is pushed. Include attribution on every automated thread reply, optional PR comment, and peer coordination message. After a code fix, include the pushed SHA; for intent/dependency/overlap/help/handoff messages and when no code changed, include attribution without inventing a SHA. Peers may talk before any push exists.

Render with `writ attribution format` so platforms set `agent_id` (`WRIT_AGENT_ID`) without forking reply logic. Canonical templates live in [`SKILL.md`](SKILL.md#reply-attribution). Thread reply after a successful push:

```text
Fixed the branch check and added the mismatch regression test.

---
worktrees-hives agent: fixed in abc1234
```

Replies may explain why no code change is needed. Resolve a thread only when the concern is addressed or the reviewer has accepted the explanation.

## Remote GitHub merges

`writ` does not merge pull requests. GitHub repository rules, required checks, and reviews are the intended authority. Auto-merge, merge queues, scheduled merges, and admin bypasses remain forbidden. Report a missing-protection operator gap; do not change repository settings or rebuild a writ merge-permission engine.

## Review outcome

A successful interactive-monitoring report marks the PR as **merge-ready** when required CI is green, conflicts are absent, change requests are cleared, and review threads are resolved. That status is advisory and does not authorize a merge. A human reviewer decides whether and when to merge on GitHub.
