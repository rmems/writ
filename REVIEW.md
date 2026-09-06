# Review guide

This guide defines the review and pull-request lifecycle for `writ`. It applies to human-authored and agent-authored changes.

## Lifecycle

```text
issue -> claimed job -> isolated worktree -> focused commits -> pull request
      -> companion-skill interactive monitoring -> merge-ready report -> human merge decision
      -> human merge or explicitly authorized one-shot primary-agent merge
```

The orchestrator, unattended runtime, installed companion `babysit-pr` skill, and worker agents may prepare or monitor a pull request and report that it is merge-ready, but they never merge it or claim that an automated merge will occur. The companion skill is guidance, not an enforcement boundary. A primary interactive agent may execute a human's explicit one-shot merge request only by completing the [protocol in `AGENTS.md`](AGENTS.md#human-authorized-one-shot-merge-protocol). Auto-merge and merge queues remain forbidden.

For stacked pull requests, review and fix the bottom PR before its children. Re-evaluate children after their base changes.

## Required checklist

### Scope and traceability

- [ ] The PR links its GitHub issue and, when present, the matching Linear `RM-*` issue.
- [ ] Changes satisfy the linked acceptance criteria without unrelated refactors.
- [ ] Session work is reflected accurately in Beads.
- [ ] Generated replies identify the agent and, after a fix, include the pushed commit SHA.
- [ ] Attribution is audited only on commits reachable from the submitted PR head and not its base, excluding synthetic review-merge/checkout and test-fixture commits. The Git author is the primary author, `Co-authored-by` credits an additional contributor, and `Agent` identifies the coding agent; the presence of one does not prove another.
- [ ] Every Codex-authored commit in that submitted range contains exact `Agent: Codex` and `Co-authored-by: Codex <noreply@openai.com>` trailers without rewriting Cursor-attributed history.
- [ ] After the first tested implementation and before final publication, one independent review matched to the change's risk was completed. Extra review was requested only for a named high-risk boundary or a reproduced finding that warranted follow-up.

### Safety

- [ ] No command, API call, prompt, or documentation path can merge a PR automatically, enqueue it, or schedule a deferred merge.
- [ ] Product runtime and worker interfaces expose no merge path; any interactive one-shot merge is outside the unattended runtime and satisfies the complete human-authorization protocol.
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
- `gh pr merge` and merge-oriented `gh api` requests are impossible through product runtime public interfaces.
- Branch verification occurs immediately before mutation to reduce time-of-check/time-of-use risk.
- Canonicalization and component checks prevent `..`, symlink, or prefix-based path escape.
- State writes use a temporary file and atomic replacement without leaving partial JSON. **Nothing writes state today** -- `state.rs` is a read path only, so there is currently no code for a reviewer to check this against. The requirement stands for whatever gains a write path next, which is expected to be the lease store (#124), not `watched.json`.
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

Post a fix reply only after its commit is pushed. Include the short or full SHA and attribution, for example:

```text
Grok Build agent: fixed the branch check in abc1234 and added the mismatch regression test.
```

Replies may explain why no code change is needed. Resolve a thread only when the concern is addressed or the reviewer has accepted the explanation.

## Human-authorized one-shot merge review

Before a primary interactive agent executes a requested merge, verify all of the following:

- [ ] The human's current message unambiguously identifies and affirmatively requests the exact PR, either directly or by reference to the single current PR; no standing permission or inferred intent is being reused.
- [ ] The expected repository, PR number, base, head SHA, and merge method were stated. The method is the human's choice, or squash when the request omitted a method.
- [ ] A fresh GitHub MCP preflight confirms the PR is open, non-draft, conflict-free, mergeable, still at that head SHA, and has all required checks terminal and successful without a protection bypass.
- [ ] The current review decision, every paginated review thread, and trusted-bot comments were inspected.
- [ ] Any unresolved findings were disclosed before the final human decision. Every explicitly deferred actionable finding has a linked open GitHub issue; duplicate, obsolete, or non-actionable findings have a documented disposition.
- [ ] No new blocker appeared and authorization did not expire because the target/head changed, the method became ambiguous, or the session ended.
- [ ] The operation is an immediate one-shot merge through GitHub MCP when available. Auto-merge, merge queues, scheduled merges, and admin bypasses are disabled.
- [ ] The post-mutation read confirms the merged state and captures the method and resulting merge commit SHA before the agent claims success.

## Review outcome

A successful interactive-monitoring report marks the PR as **merge-ready** when required CI is green, conflicts are absent, change requests are cleared, and review threads are resolved. That status is advisory and does not authorize a merge. A human reviewer decides whether and when to merge, then either merges personally or explicitly requests a one-shot primary-agent merge under the checklist above.
