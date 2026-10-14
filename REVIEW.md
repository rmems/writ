# Review guide

This guide defines the review and pull-request lifecycle for `writ`. It applies to human-authored and agent-authored changes.

## Lifecycle

```text
authorized task -> suitable checkout -> focused checks -> checkpoint or pull request
      -> independent review and relevant validation -> truthful readiness report
      -> authorized merge on GitHub (repository protection owns the merge)
```

Checkpoints may be published with honest focused-test status. Readiness needs relevant validation and review. GitHub owns remote permissions and merge rules; an agent acts only within operator authorization. Read-only review needs no new checkout or task record. Missing GitHub protection is an operator gap to report, not a reason to implement a second merge-permission system.

For stacked pull requests, review and fix the bottom PR before its children. Re-evaluate children after their base changes.

## Required checklist

### Scope and traceability

- [ ] The PR links its GitHub issue and, when present, the matching Linear `RM-*` issue.
- [ ] Changes satisfy the linked acceptance criteria without unrelated refactors.
- [ ] Existing task context and delivered scope are represented accurately; no tracker mirror or extra ticket is required.
- [ ] Generated replies identify the agent. Review-fix replies include the pushed commit SHA. Coordination messages and no-code-change replies do not invent a SHA.
- [ ] Attribution is audited only on commits reachable from the submitted PR head and not its base, excluding synthetic review-merge/checkout and test-fixture commits. The Git author is the primary author, `Co-authored-by` credits an additional contributor, and `Agent` identifies the coding agent; the presence of one does not prove another.
- [ ] Every Codex-authored commit in that submitted range contains exact `Agent: Codex` and `Co-authored-by: Codex <noreply@openai.com>` trailers without rewriting Cursor-attributed history.
- [ ] Readiness claims have risk-appropriate independent review and measured validation. Checkpoints identify unrun or failing checks.

### Safety

- [ ] GitHub remains the remote permission and merge authority. Runtime admission is distinct from operator authorization; this review does not itself authorize a merge.
- [ ] Assigned local integration and conflict recovery work; dirty-WIP integration and destructive checkout changes remain protected. Branch names and remote PR state add no separate approval gate.
- [ ] Force pushing accepts only `--force-with-lease`; bare `--force` and `-f` are rejected.
- [ ] Mutating operations verify the expected job branch.
- [ ] Harness-owned checkouts work at their chosen paths. Legacy managed-create path checks remain confined to deprecated lifecycle commands.
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
- [ ] CI check handling follows [`docs/ci-taxonomy.md`](docs/ci-taxonomy.md): Class A/B/C actions, no empty retrigger commits, `skipping` non-blocking, `pending` without rerun spam. Keep required-check failures distinct from advisory findings, pending, external-access/`ACTION_REQUIRED`, and unknown requiredness; a provider name is not a merge gate.

## Language-specific review notes

`writ` is a Rust workspace. Each area has distinct review concerns:

### Rust review notes

Rust owns the hard safety boundary. Reviewers should check:

- Policy is enforced in `writ-core`, not only in `clap` argument definitions.
- Git and GitHub operations use explicit allowlists and structured argument vectors rather than shell command strings.
- Remote `gh` operations are subject to GitHub authorization and explicit repository targeting, without local checkout/branch prerequisites. Local integration retains expected-branch and dirty-WIP checks.
- Branch verification occurs immediately before mutation to reduce time-of-check/time-of-use risk.
- Canonicalization and component checks prevent `..`, symlink, or prefix-based path escape.
- State writes for the hook lease store go through SQLite (`rusqlite` bundled). `watched.json` remains a read-only legacy path; do not add a writer for it.
- Process timeouts terminate and reap children. This is implemented in `supervisor.rs` (wall-clock timeout inclusive of permit wait, idle hang detection starting at spawn, SIGTERM-then-SIGKILL grace on Unix -- Windows kills only the direct child after grace -- Drop disarm after reap, bounded pipe drain, and supervised host-script execution); changes to it require timeout, idle, reaping, and recovery-policy tests, not a deferral. Recovery must not merge, bare-force-push, delete a harness-owned checkout, or emit a fake SHA.
- Public error codes are stable enough for callers to classify without parsing prose.
- Unsafe Rust remains forbidden unless a separately reviewed design justifies it.

Before a readiness claim, run relevant native gates; checkpoints may report focused checks instead:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```


## Review replies

Post a review-fix reply only after its commit is pushed. Include attribution on every automated thread reply, optional PR comment, and peer coordination message. Identify the real agent plus task, branch, and session when those exist. After a code fix, include the pushed SHA; for intent/dependency/overlap/help/handoff/conflict messages and when no code changed, include attribution without inventing a SHA. Peers may talk before any push exists.

The optional `writ attribution format` helper lets platforms set `agent_id` (`WRIT_AGENT_ID`) without forking reply logic. Canonical templates live in [`SKILL.md`](SKILL.md#reply-attribution). Thread reply after a successful push:

```text
Fixed the branch check and added the mismatch regression test.

---
writ agent: fixed in abc1234
```

Replies may explain why no code change is needed. Resolve a thread only when the concern is addressed or the reviewer has accepted the explanation.

## Remote GitHub merges

GitHub repository rules, required checks, reviews and permissions decide remote operations. Writ does not duplicate those controls. The operator decides which actions the agent is authorized to request. Report missing protection without changing settings unless requested.

## Review outcome

A successful interactive-monitoring report marks the PR as **merge-ready** when required CI is green, conflicts are absent, change requests are cleared, and review threads are resolved. That status is advisory and does not authorize a merge. The operator or established automation policy decides whether and when to request a merge on GitHub.
