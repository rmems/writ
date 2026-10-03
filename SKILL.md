---
name: writ
description: Use when discovering GitHub or Linear work, spawning isolated worker agents, running Safe Issue → Verified Commit or PR handoff, or applying writ safety rules (assigned-worktree local integration, force-with-lease only). Portable procedure for the writ Rust coordination core; not a security boundary.
---

# writ Skill

Installable agent skill for the `writ` Rust coordination core. Directory name and frontmatter `name` are both `writ`. Install with [`scripts/install-skill.sh`](scripts/install-skill.sh); see [`docs/install.md`](docs/install.md).

[`AGENTS.md`](AGENTS.md) is the authoritative repository contribution and autonomy contract. This portable skill supplies platform-neutral procedures and must not broaden or relax that policy.

## When to use

Use this skill when:
- Discovering work from GitHub or Linear issues
- Spawning worker subagents for code changes
- Integrating compatible peer work into an assigned feature-branch worktree
- Running [Safe Issue → Verified Commit](docs/workflows/safe-issue-verified-commit.md) then [Safe Verified Commit → PR](docs/workflows/safe-verified-commit-to-pr.md)
- Formatting attributed PR review replies and peer coordination messages (`writ attribution format`; [Reply attribution](#reply-attribution))
- Handing a pull request to the installed companion `babysit-pr` skill when interactive monitoring is needed
- Reporting results back to the operator

## Collaboration policy

Read [`AGENTS.md`](AGENTS.md) for ownership, WIP protection, attribution and authorized publication. Reuse suitable harness-owned checkouts and existing task context. Independent writers need separate checkouts; read-only reviewers do not. No tracker installation, exact-base reset or writ binary is a prerequisite for working with the host's authorized tools.

### Branch/worktree pre-edit checklist

Verify the assigned repository, checkout and branch. Inspect existing changes and peer ownership. Preserve WIP, negotiate overlapping work, and use merge/rebase/cherry-pick for normal integration. Register the checkout when using the shared lease store. The harness chooses its path; a managed writ root is unnecessary.

### Validation and publication

Run focused tests while implementing. Publish useful checkpoints or drafts with their real test status. Before claiming readiness, review the change independently and run the relevant native gates on the submitted tree. Inspect current GitHub checks and report failures, pending work and unavailable platform coverage. A checkpoint is not merge readiness; no extra tracker or metadata gate is needed to save it.

GitHub owns remote merge permissions. Writ helpers admit remote operations without requiring a local branch; the operator's authorization still determines whether an agent should perform them. Local checkout-changing actions retain ownership safeguards. This skill does not authorize a merge merely because a PR is ready.

### Final status guidance

When handing off a pull request, report:

- **PR status:** Open / Ready for review / Blocked
- **Residual issues:** List of unresolved CI failures, review comments, or blockers
- **Agent attribution:** Every automated PR comment and thread reply uses the templates below

A worker or companion-skill monitoring agent reports PR status truthfully and does not take credit for merges performed by another actor. Do not invent a SHA for a message that did not land code.

### Reply attribution

Automated review-thread replies, optional PR-level summary comments, and peer coordination messages must identify **which automation stack** responded. When discussing committed or pushed work, they must also identify **which commit** with an actual SHA. Attribution is transparency, not GitHub App impersonation and not a change to Git `user.name` / `user.email`.

Do not merge, approve, or auto-resolve a thread as a side effect of posting a reply. Intent, dependency, overlap/help, and handoff messages may be posted before any code or pushed SHA exists. Post a review-fix reply only after a successful push, and include that pushed SHA. A local HEAD SHA after a failed or rejected push is not a completion SHA. Never fabricate a SHA.

#### Configuration

Platforms set identity without forking these templates. Empty values fall back to the default so attribution cannot become blank noise.

| Key | Env | Type | Default | Purpose |
| --- | --- | --- | --- | --- |
| `agent_id` / `attribution` | `WRIT_AGENT_ID`, else `WRIT_ATTRIBUTION` | string | `writ agent` | Identity line on replies |
| `task_id` | `WRIT_TASK_ID`, else `--task` | string | omitted | Linear or issue id when one exists |
| `branch` | `WRIT_BRANCH`, else `--branch` | string | omitted | Assigned branch when one exists |
| `session_id` | `WRIT_SESSION_ID`, else `--session` | string | omitted | Agent session id when one exists |
| `include_sha_on_fix` | `WRIT_INCLUDE_SHA_ON_FIX` | bool | `true` | Callers intend to attach a SHA after code fixes. Review-fix replies after a successful push still include that SHA. Coordination messages omit it. |
| `attribution_placement` | `WRIT_ATTRIBUTION_PLACEMENT` | `footer` \| `header` | `footer` | Where the line goes |

Override `agent_id` with `WRIT_AGENT_ID` (or `writ attribution format --agent-id ...`). Do not copy this skill to change the label.

| Platform | Example `agent_id` |
| --- | --- |
| Generic default | `writ agent` |
| Claude Code | `Claude Code: writ agent` |
| Codex | `Codex: writ agent` |
| OpenClaw | `OpenClaw: writ agent` |

The `{platform}: writ agent` shape keeps a single colon when the formatter appends `: fixed in <sha>`.

#### Templates

Render with `writ attribution format` so platforms do not fork reply logic. Human mode prints the body to post. `--json` wraps it in the v1 envelope (`command`: `attribution.format`).

Thread reply after a successful push (default footer):

```bash
writ attribution format --body "Fixed the branch check and added the mismatch regression test." --commit-sha abc1234
```

```text
Fixed the branch check and added the mismatch regression test.

---
writ agent: fixed in abc1234
```

Thread reply when no code change landed — omit `--commit-sha`; do not invent a SHA:

```bash
writ attribution format --body "No code change: the check already covers this path."
```

```text
No code change: the check already covers this path.

---
writ agent
```

Peer coordination (intent, dependency, overlap/help, handoff, conflict) — include real task/branch/session identity; omit `--commit-sha`; do not wait for a push:

```bash
writ attribution format --body "Overlap: I own SKILL.md Reply attribution templates; RM-145 owns the rest of SKILL.md." --task RM-128 --branch cursor/reply-attribution-config-6e46 --session bc-fa8ed877
```

```text
Overlap: I own SKILL.md Reply attribution templates; RM-145 owns the rest of SKILL.md.

---
writ agent | task RM-128 | branch cursor/reply-attribution-config-6e46 | session bc-fa8ed877
```

Optional PR-level summary comment (`--pr-comment` uses a blank line instead of `---`):

```bash
writ attribution format --pr-comment --body "Ready for review." --commit-sha abc1234
```

```text
Ready for review.

writ agent: fixed in abc1234
```

`--placement header` puts the identity line above the body. `--agent-id` overrides the env default for one reply.

SHA policy: include `--commit-sha` only when referring to committed or pushed work, and only with a real object id. Review replies that report a code fix must use the SHA from a successful push; do not omit it after that push, and do not invent one. Coordination messages and replies with no code change omit `--commit-sha`. If a SHA is passed to the formatter, it is always rendered so a real fix cannot be dropped. Peers do not need a push before they can talk.

### Collaboration watchlist (view only)

`writ watchlist list|check|check-all` reads the shared lease store (`WRIT_LEASE_PATH`, default `{user_data}/writ/leases.db`). It does **not** write `watchlist.json`, `watched.json`, or `pr-babysit` state.

- **Source of ownership:** lease rows from `writ worktree register` (and later RM-825 `coord_claims` / `coord_messages` when those tables exist).
- **Local collab status:** `running`, `waiting`, `paused`, `conflicted`, `ready_for_integration`.
- **Recovery:** `live`, `released`, `stale_heartbeat`, `missing_checkout`.
- **GitHub:** `check` / `check-all` probe PRs live (`gh pr list --head`). That overlay is not a merge gate. `--repo` / `--owner` filters are optional; probes still honour `WRIT_ALLOWED_OWNERS`.
- **`add` / `remove`:** do not persist. Register or unregister a checkout instead.
- Same-host SQLite is not cross-host coordination. Task tracking follows the operator; no tracker is required by the product.

See [`docs/watchlist-schema.md`](docs/watchlist-schema.md).

### Platform-neutral worker prompt template

Give each worker the task, owned files, checkout, branch, tests, dependencies and integration owner. A compact prompt is enough:

```text
Work only in your assigned checkout and scope; preserve existing WIP.
Coordinate overlap and handoffs. Integrate compatible peer commits locally.
Keep direct local mutations bound to the intended branch; never bare-force-push.
Use focused tests, and report observed results and residuals truthfully.
Publish checkpoints when authorized; distinguish them from reviewed readiness.
Identify your agent and use the real pushed SHA when reporting a pushed fix.
```

### Timeouts and hang recovery

Long-running supervised commands use `writ supervisor run` with the named policy in [`docs/timeout-policy.md`](docs/timeout-policy.md). A timeout is a recovery and handoff event: the supervisor contains the child (hard/idle/lost-child/permit-wait) and never retries, merges, bare-force-pushes, deletes a harness checkout, or invents a SHA. Harness re-dispatch is capped by `RedispatchBudget` / `max_redispatch_per_item` (default 1). Timeout residuals do not increment `fix_count`.

### Optional helpers

Use registration/status for shared ownership visibility, supervision for bounded execution, and git/gh helpers for their documented local integrity and repository-target checks. Host-native tools remain valid. Scripts and opaque API payloads are not recursively inspected. See [`AGENTS.md`](AGENTS.md#enforcement-layers).
