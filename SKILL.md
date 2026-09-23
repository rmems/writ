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

## Authoritative safety policy

Before any mutation, read and apply the corresponding `AGENTS.md` sections:

- [core prohibitions and deny-list](AGENTS.md#non-negotiable-safety)
- [local collaboration](AGENTS.md#local-collaboration-allowed)
- [remote GitHub merges](AGENTS.md#remote-github-merges)
- [force-with-lease allow-list](AGENTS.md#allow-list-for-force-with-lease)
- [attribution semantics](AGENTS.md#attribution-semantics)
- [team-maintainer operating model](AGENTS.md#team-maintainer-operating-model)

This skill never grants an exception to those rules. Local `git merge` / `rebase` / `cherry-pick` of peer work into the assigned feature branch is routine; conflict repair is expected. If the authoritative policy is unavailable, contradictory, or cannot be enforced by the Rust boundary -- `writ` itself, or an enforcing wrapper that routes the mutation through `writ-core`'s allowlist and branch verification -- stop the mutating flow and report the blocker.

### Branch/worktree pre-edit checklist

Before making any code change, the agent MUST verify:

1. **Worktree isolation:** `pwd` is inside the assigned checkout path (created by the harness or `git worktree add`, then joined via `writ worktree register`).
2. **Branch correctness:** `git branch --show-current` matches the assigned feature branch.
3. **Clean state:** `git status` shows no uncommitted changes from other work. A dirty or stale primary checkout is preserved and is not a reason to abort isolated work.
4. **Remote alignment:** For a newly created, unpublished assigned branch, fetch the intended remote base and prove that the branch equals that exact remote-base commit before edits; it may lack an upstream only for this creation proof. For a published assigned branch, fetch and verify its expected upstream and the expected local/remote relationship instead of comparing the branch with the base. Stop on an unexpected upstream, unexpected remote commit, behind state, or divergence.
5. **No cross-boundary edits:** No file outside the worktree is modified (no `../` paths, no absolute paths outside the worktree root).

Repair a clean bootstrap source or a newly created unpublished branch's verified-base alignment before editing. Abort and report an unsafe identity or path mismatch, a genuine ownership collision, or any other non-recoverable failure.

### Validation and final publication sequence

Run focused, task-relevant gates while working. After the first tested
implementation and before final publication, obtain one independent review
matched to the change's risk. Additional review is required only for a named
high-risk boundary or a reproduced finding that warrants follow-up.

Before first publication of an unpublished assigned branch, fetch the verified
remote base and complete final alignment or rebase. For a published branch,
fetch and reconcile it with its expected upstream; do not require equality with
the base. Then run exactly one complete native gate suite on the exact `HEAD`
that would be pushed. An issue may add focused checks; it must not replace or
reduce that final suite. Any later tree change invalidates that run and requires
restoring the applicable alignment and rerunning the suite before push.

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
- Same-host SQLite is not cross-host coordination. Linear remains the required task tracker.

See [`docs/watchlist-schema.md`](docs/watchlist-schema.md).

### CI taxonomy (Class A / B / C)

When monitoring PR checks, classify each row from `gh pr checks --json name,state,bucket,workflow,link` (and `statusCheckRollup` when `ACTION_REQUIRED` is needed) using [`docs/ci-taxonomy.md`](docs/ci-taxonomy.md) or `writ --json ci classify`. Summary:

| Class | Meaning | Do | Do not |
| --- | --- | --- | --- |
| **A** | GitHub Actions / Azure build-test | Fix source; **one** `gh run rerun` on flake | Empty “kick CI” commits |
| **B** | Codacy (and similar quality gates) | Fix real file+line findings; residual human gate on `ACTION_REQUIRED` | Empty pushes to wake the dashboard |
| **C** | Kilo, CodeRabbit, Gitar, unknown bots | Report residual (`class_c:kilo_pending`, …); continue Class A | Empty retrigger commits |

`skipping` is non-blocking. `pending` means continue other work without rerun spam. **Prefer a real fix or `gh run rerun` over noise commits.** Residual codes belong in watchlist notes and the final report.

`writ --json ci classify` emits a compact `data.collaboration` object (`ci_class`, observation counts, `residual_codes`, `blocks_unrelated_workers: false`). Copy that into existing RM-139 / RM-127 status views. Do **not** write `watched.json`, a second store, or a lease row for CI. An externally blocked service must not freeze unrelated workers.

Requiredness is **not** the Class A/B/C letter and is **not** inferred from a provider name. Pass `isRequired` from GraphQL when available. Count `required_failure` separately from advisory findings, pending results, `ACTION_REQUIRED` external-access/configuration problems, and unknown requiredness. Unknown is a report, not a writ merge gate and not a pass. Do not disable checks, fabricate success, or empty-commit to retrigger a bot.

### Platform-neutral worker prompt template

When spawning a worker subagent, include these safety instructions in the prompt:

```
SAFETY RULES (non-negotiable):
- Local `git merge` / `rebase` / `cherry-pick` of peer work into the assigned feature branch is allowed
- NEVER merge into `main`/`master` locally; refuse a merge that would lose uncommitted WIP
- NEVER use bare `git push --force` or `git push -f`
- `git push --force-with-lease` is allowed only for rebasing your own branch
- NEVER edit files outside your assigned worktree
- One writable worker per assigned worktree and branch
- Before editing, verify: worktree path, branch name, clean assigned state, and remote alignment; exact remote-base equality applies only to a newly created unpublished branch, while a published branch must match its expected upstream relationship
- Repair a clean bootstrap source or unpublished verified-base alignment; abort on unsafe identity or path mismatch
- After the first tested implementation and before publication, obtain one independent risk-matched review; add review only for a named high-risk boundary or an actual finding
- Use `writ attribution format` for automated replies. Intent/dependency/overlap/help/handoff/conflict messages need real agent/task/branch/session identity and must not invent a SHA. After a successful push, review-fix replies include that real SHA.
```

Local assigned-branch integration is allowed.

### Timeouts and hang recovery

Long-running supervised commands use `writ supervisor run` with the named policy in [`docs/timeout-policy.md`](docs/timeout-policy.md). A timeout is a recovery and handoff event: the supervisor contains the child (hard/idle/lost-child/permit-wait) and never retries, merges, bare-force-pushes, deletes a harness checkout, or invents a SHA. Harness re-dispatch is capped by `RedispatchBudget` / `max_redispatch_per_item` (default 1). Timeout residuals do not increment `fix_count`.

### Enforcement routing

This skill is portable procedure, not a security boundary. Route orchestrated
mutations through `writ`, with Rust enforcing the runtime boundary as
defined in [`AGENTS.md`](AGENTS.md#enforcement-layers). GitHub owns remote PR
integration; Linear owns task tracking.
