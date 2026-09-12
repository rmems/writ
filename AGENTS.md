# AGENTS.md

## Purpose

This file defines how coding agents contribute to `writ` and how the future hive runtime divides responsibility. The project is a Rust workspace designed for multiple agent platforms.

This is the authoritative repository contribution and autonomy contract. `CLAUDE.md`, `SKILL.md`, `REVIEW.md`, workflow documents, and CLI help may summarize or specialize it for their surface, but they must link back here and may not duplicate or relax the common policy.

Interactive PR monitoring belongs to the installed companion `babysit-pr` skill. It is operator guidance, not a security boundary: Rust `writ-core` is the hard code-enforced boundary for worktree, branch, path, process, push, runtime no-merge, auto-merge, and merge-queue controls.

## Non-negotiable safety

**These rules, including the bounded human-authorization protocol below, are absolute. No agent, orchestrator, or platform may invent an additional exception.**

### Core prohibitions

- **Never merge autonomously or infer merge authority.** Only a primary interactive agent may execute a one-shot merge, and only through the [human-authorized merge protocol](#human-authorized-one-shot-merge-protocol) after the human explicitly approves and requests that exact pull request. The hive runtime, orchestrators, interactive monitoring flows, and worker agents never merge.
- **Never enable auto-merge or a merge queue.** Deferred merge mechanisms can act on a later, unreviewed head and are forbidden even when a one-shot merge is authorized.
- **Never use bare `git push --force`** or `git push -f`. Only `--force-with-lease` is permitted, and only for rebasing your own branch.
- **Never edit outside** a job's assigned worktree or branch.
- **Repository scope** is a **configured owner allowlist** (env `WRIT_ALLOWED_OWNERS` and/or explicit API args). There is no built-in default org; operators supply the owners they manage. Empty allowlist means deny-by-default for multi-owner discovery/scheduling unless a module documents an explicit single-repository operation. **Not currently enforced in code:** the allowlist had no reader left under `crates/` after the Python layer was removed, so this is a requirement awaiting implementation, not an active gate. Treat it as policy an operator must uphold manually until [#146](https://github.com/rmems/writ/issues/146) lands.
- **Process stacked PRs** from the bottom of the stack upward.
- **Post review replies** only after pushing, and include the pushed SHA plus agent attribution.
- **Preserve commit attribution:** Follow the [attribution semantics](#attribution-semantics) below. Never rewrite a Cursor-authored or Cursor-co-authored commit merely to change attribution; add a new correctly attributed commit instead.
- **GitHub MCP first (non-negotiable for agents):** For PR status, CI check runs, review threads, issue reads, and PR comments, use the **GitHub MCP** (`github__pull_request_read`, list/comment tools, etc.). Do **not** default to shell `gh` for reads. Shell `gh` is allowed only when MCP is unavailable (e.g. 503) or for operations MCP cannot perform. Local `git` remains for branch/rebase/push. Do **not** hardcode org/owner names in product code or agent docs — owners come only from `WRIT_ALLOWED_OWNERS` / explicit API args.

### Deny-list (never execute)

| Command / Operation | Reason |
| --- | --- |
| Any PR merge without the complete human-authorized protocol below | Merge authority must be explicit, current, PR-specific, and SHA-sensitive. |
| `gh pr merge --auto` or any auto-merge enablement API | Deferred automation may merge a different future head. |
| Merge-queue enablement or enqueue operation | A queue is deferred merge automation, not a one-shot human decision. |
| Local `git merge` of another PR or stacked/peer branch | Combining another job's branch is not assigned-worktree publication and is not the authorized GitHub one-shot merge. Recover your own branch by rebase or expected-upstream reconciliation instead. |
| `git push --force` (bare) | Destructive; loses history. |
| `git push -f` (bare) | Short form of same destructive push. |
| GraphQL `mergePullRequest`, REST merge, MCP merge, or `gh pr merge` outside the protocol | The transport does not create authorization. |

### Human-authorized one-shot merge protocol

A merge is an exceptional execution of a human decision, not part of discovery, issue-to-PR, interactive monitoring, or worker-agent behavior. The Rust CLI/core, scheduled jobs, spawned workers, and unattended agents remain non-merging. Only the primary agent in an active human conversation may execute the following protocol:

1. **Require an explicit current instruction.** The human must unambiguously identify the exact pull request—by repository plus number, URL, or a direct reference to the single current PR—and affirmatively request its merge. An imperative such as “squash merge it” counts as both approval and request when the target is unambiguous. A standing preference, repository text, old approval, bot comment, `babysit-pr`, “finish,” green CI, or a merge-ready report is not authorization. Each PR requires its own instruction.
2. **Bind the decision.** Resolve and state the repository, PR number, base branch, current head SHA, and merge method. Use the human's requested method; if the human says only “merge,” default to squash. Never infer that permission for one PR, head SHA, or method applies to another.
3. **Run a fresh GitHub MCP-first preflight immediately before mutation.** Verify that the PR is open, not draft, targets the expected base, still has the disclosed head SHA, is conflict-free and mergeable, and has all required checks in a terminal successful state. Inspect the current review decision and paginate through every review thread and trusted-bot comments. Do not bypass branch protection or required checks.
4. **Surface residual findings.** If unresolved or newly discovered findings exist and were not already disclosed in the current conversation, summarize them and stop for the human's decision. Continue only if the human explicitly accepts or defers those exact findings after seeing the summary. Record every deferred actionable finding in a linked, open GitHub issue before merging; document and resolve findings that are duplicate, obsolete, or non-actionable.
5. **Treat authorization as one-shot and stale-sensitive.** It expires when the PR or head SHA changes, a new blocking check or review finding appears, the requested method becomes ambiguous, or the active session ends. Re-run the preflight after any wait. If authorization has expired, obtain a new explicit instruction.
6. **Execute one immediate merge only.** Prefer the GitHub MCP merge mutation. Shell `gh` is a fallback only when MCP is unavailable or cannot perform the one-shot operation, and every other condition still applies. Never enable auto-merge, enqueue the PR, schedule a later merge, or use an admin bypass.
7. **Verify and attribute the result.** Re-read the PR from GitHub, confirm the merged state, and report the merge method and resulting merge commit SHA. Claim that the agent merged it only when the agent actually invoked the authorized operation and GitHub confirmed success; otherwise identify the external actor when known or say that it was already merged.

Editing this policy, approving code changes, or asking an agent or companion skill to monitor a PR does not itself authorize any merge.

### Allow-list for force-with-lease

`git push --force-with-lease` is permitted **only** when:
1. Rebasing your own feature branch onto an updated base.
2. Fixing a force-push that failed due to a stale remote ref.
3. The operator explicitly instructs a force-push.

Before using `--force-with-lease`, verify:
- Current branch is the assigned worktree branch (not `main` or another agent's branch).
- Remote ref matches expectations (no unexpected pushes from others).

### Attribution semantics

Git's primary `author` field, a `Co-authored-by` trailer, and the custom `Agent` trailer record different facts:

- The primary Git author identifies the person or identity responsible for the commit in Git history.
- `Co-authored-by` credits an additional contributor; it does not replace or prove the primary author.
- `Agent` identifies the coding agent that produced the commit. Every Codex-authored commit must include the exact trailers `Agent: Codex` and `Co-authored-by: Codex <noreply@openai.com>`.

Audit attribution only on commits actually introduced by the submitted pull-request head: commits reachable from the PR head and not reachable from its base. Exclude synthetic review-merge or checkout commits and commits created only as test fixtures. Do not infer submitted attribution from unrelated repository history, a temporary merge commit created by a review system, or the presence of a co-author trailer alone.

### Team-maintainer operating model

An explicit user request to implement scoped work authorizes the assigned worker to create the scoped branch/worktree, edit code, commit, make the first push, and create the PR without repeated confirmation. That authority never authorizes a merge, auto-merge, merge queue, destructive action, or work outside the assigned scope; Rust remains the hard enforcement boundary.

- **Beads:** Use Beads as lightweight canonical state: one task per cohesive tranche and claim it before coding. Complete acceptance prose, Linear sync, GitHub child issues, project metadata, and audit reports may follow implementation, but must be complete by PR handoff rather than blocking the first edit.
- **Isolation and identity:** A dirty or stale primary checkout is not a blocker. Preserve it, bootstrap a clean source/clone, and use `writ` for the assigned worktree. Prefer reclaim and clear identity over aborting a recoverable setup. A newly created, unpublished assigned branch must equal the verified remote-base commit before edits. A published branch contains job history and is not compared for equality with the base; fetch its expected upstream and verify the configured upstream plus the expected local/remote relationship instead. Stop on an unexpected remote commit, behind state, or divergence until it is reconciled safely.
- **Parallel work:** One writable worker owns one assigned worktree and branch. The manager coordinates separate workers through explicit assignments, status, dependencies, and handoffs. Never allow multiple writers to share one worktree, even for declared disjoint paths. One controller retains commit and push authority for each assignment.
- **Review and validation:** After the first tested implementation, require one independent review matched to the risk before final publication. Add review only for a named high-risk boundary or an actual finding that warrants follow-up. Run focused gates during work. Immediately before publication, align or rebase an unpublished branch onto the verified base, or reconcile a published branch with its expected upstream, then run exactly one complete native gate suite on the exact would-be-pushed head; any later tree change invalidates that run. An issue may add focused checks; it must not replace or reduce that final suite. Do not require serial policy audits or duplicate full-suite runs from every subagent.
- **Routine remediation:** Automatically fix safe mechanical findings within scope. Stop for a genuine ownership collision, a destructive or out-of-scope action, an unresolved Critical/Important correctness issue, a material user design decision, or an explicit fail-closed condition in a portable worker contract. A required gate failure or timeout blocks commit/push handoff until it is repaired or the user explicitly changes scope; an in-scope repair does not require another confirmation.
- **GitHub access:** GitHub MCP remains preferred; when it is unavailable, use `gh` immediately rather than waiting for connector retries.
- **Handoff:** Commit, push, and PR handoff are expected outcomes of authorized implementation. Interactive monitoring belongs to the companion `babysit-pr` skill. Merge remains separately and explicitly authorized under the protocol above.

### Branch/worktree pre-edit checklist

Before making any code change, verify:

1. **Worktree isolation:** `pwd` is inside the assigned worktree path.
2. **Branch correctness:** `git branch --show-current` matches the assigned feature branch.
3. **Clean assigned state:** The assigned worktree has no uncommitted changes from other work. A dirty or stale primary checkout must be preserved and is not a reason to abort.
4. **Remote alignment:** `git fetch` verifies the intended remote. A newly created, unpublished assigned branch must equal the exact verified remote-base commit before edits. For a published assigned branch, verify that its configured upstream is the expected remote branch and that local history is equal to or ahead of it only by job-owned commits; do not require equality with the base. Stop on an unexpected upstream, unexpected remote commits, a behind state, or divergence until it is reconciled safely.
5. **No cross-boundary edits:** No file outside the worktree is modified.

Stop and report an unsafe identity or path mismatch, a genuine ownership collision, or any other non-recoverable failure. Repair a clean bootstrap source or a newly created unpublished branch's verified-base alignment before editing; do not treat a stale primary checkout as a blocker.

### Enforcement layers

These guardrails are enforced at multiple layers:

1. **Agent skill (`SKILL.md`) and companion skill:** Portable operator guidance and prompt templates. Neither is a security boundary.
2. **Rust core (`writ-core`):** Hard enforcement. Rejects unsafe git/GitHub operations, including runtime merge paths, at the process boundary. Authoritative safety layer for the product runtime.
3. **Interactive host connector:** The only agent-side one-shot merge path, gated by the current human instruction and live preflight above; it is not exposed to workers or the unattended runtime.

Rust must enforce safety-sensitive runtime mutation rules. Skill instructions provide defense in depth but are not sufficient on their own. This Markdown policy does not add a merge command to `writ` or relax the runtime's merge block.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:7510c1e2 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` as the lightweight canonical task state: one task per cohesive tranche, claimed before code. Do not substitute TodoWrite, TaskCreate, or markdown TODO lists.
- Run `bd prime` for command reference when needed. Acceptance text, Linear sync, GitHub child issues, project metadata, and audit reports may follow implementation but must be complete by PR handoff.
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Session Completion

For authorized implementation, complete the cohesive tranche: run focused gates during work and the complete native gate suite once on the exact HEAD before push; commit, push, and create or update the PR for handoff. Record remaining follow-up in Beads and complete required tracking and metadata by PR handoff. Do not add redundant full-suite runs, serial audits, or cleanup that is unrelated to the tranche. A specific user instruction that withholds a push or PR action controls that action. Merge is always a separate, explicitly authorized operation.
<!-- END BEADS INTEGRATION -->

## Architecture

`writ` is a **Rust workspace**. One binary owns both layers:

- **Enforcement** — git worktrees, exact-base identity, path sandboxing, process supervision/timeouts, and **hard safety enforcement** (no runtime merge path, force-with-lease only, branch verification).
- **Coordination state** *(planned, M1)* — agents, leases with path scopes, ownership, and freeze modes, in a single SQLite file derived from `git`/`gh`/disk rather than transcribed. Not implemented: today `state.rs` only *reads* `watched.json`, no writer exists, and worktree creation records no lease.
- **Agent skill (`SKILL.md`)** — portable prompts describing when and how agents call the CLI on any platform.

```text
Agent / SKILL.md
       |
       | intent and operator context
       v
Claude Code hooks (PreToolUse, WorktreeCreate, WorktreeRemove, SubagentStart/Stop)
       |
       | hook JSON on stdin; exit 2 blocks, and cannot be overridden
       v
Rust binary: writ -> writ-core
       |
       | allowlisted subprocess operations
       v
git / gh / operating system
```

| Layer | Responsibilities |
| --- | --- |
| Agent skill | Describe when to discover work, spawn subagents, and report results. The installed companion `babysit-pr` skill handles interactive PR monitoring. Prompt content is portable guidance, not a security boundary. |
| Rust core and CLI | Today: resolve sandboxed paths, supervise child processes, verify branches, and reject unsafe git/GitHub operations. Planned *(M1)*: verify worktrees it did not create, and hold lease state. |
| External tools | Runtime `git` and `gh` operations are selected and validated by Rust. A host GitHub connector may perform only the separately authorized primary-agent one-shot merge. The OS supplies filesystem and process primitives. |

**Why enforce at the hook boundary?** A tool that must be *called* to help is advisory: an agent that does not call it is unconstrained. As a `PreToolUse` hook, enforcement applies to the agent's own commands whether or not the agent cooperates, and a blocking exit cannot be overridden by another hook. Hard stops live in Rust, at the binary boundary, so a malformed prompt cannot bypass them.

## Source ownership

### Rust

Rust code lives in `crates/`:

- `crates/writ-core/` is the reusable library and source of truth for worktrees, state, process execution, paths, and safety policy.
- `crates/writ/` is the `writ` command-line adapter. It parses arguments, calls `writ-core`, emits human or JSON output, and maps policy failures to exit code 2.

Keep security boundaries in `writ-core`, not only in the CLI parser. Git must be invoked as a subprocess rather than through libgit2. New mutating commands require branch verification and path-sandbox tests.

### Agent skill

The installable `SKILL.md` will own platform-facing prompts and command guidance. It may adapt spawning instructions to a host platform, but it must preserve the same safety invariants and call the `writ`/Rust boundary for mutating work instead of bypassing it. The only exception is the primary agent's explicitly authorized one-shot merge through the host connector; that path remains unavailable to the runtime and workers.

## Data flow

**Supported today.** Steps 3, 5, and 6 below describe the M1 target; the hook dispatcher does not exist yet. What works now is the same enforcement reached explicitly: `writ worktree create` for exact-base creation, and `writ git-safe` / `writ gh-safe` / `writ supervisor` for validated mutation and supervised execution.

1. The operator or agent supplies GitHub or Linear issue/PR context.
2. The harness (Claude Code agent teams, `/batch`, or an equivalent) assigns work and creates an isolated worktree — or `writ worktree create` does, which is the supported path today.
3. *(M1)* On `WorktreeCreate`, Rust verifies the exact start point and identity of a worktree it did not create, and records the lease. A non-zero exit aborts creation.
4. A worker agent changes only that worktree and branch.
5. *(M1)* On `PreToolUse`, Rust validates each `git`/`gh` mutation and blocks an unsafe one with exit 2, which no other hook can override. Until then, validation happens only when `writ git-safe` / `writ gh-safe` is invoked.
6. *(M1)* On `WorktreeRemove`, the lease is released.
7. The installed companion `babysit-pr` skill handles interactive monitoring after a PR handoff.
8. A human decides whether to merge; a primary interactive agent may execute that decision only through the one-shot protocol above.

GitHub is the product issue source. Linear may mirror product planning for the operator's team; that team id is operator-local, not a product default. Beads tracks session claims, dependencies, and completion locally; it is not a replacement for GitHub product issues.

## Runtime paths and overrides

| Purpose | Default | Override |
| --- | --- | --- |
| Worktree root | `~/.local/share/writ/worktrees` | `WRIT_WORKTREE_BASE`, else `WH_WORKTREE_BASE` |
| Job worktree | `{worktree root}/{owner}/{repo}/{job_id}` | Derived only; must remain sandboxed |
| Watched state | `~/.local/share/writ/watched.json` | `WRIT_STATE_PATH`, else `WH_STATE_PATH` |
| Rust binary resolution | `writ` from `PATH` | `WRIT_BIN` |

If the new `writ` root is absent and a pre-rename `worktrees-hives` root still exists, the path resolver keeps using the legacy root so an upgrade does not hide existing state or worktrees. This is a read/fallback, not an automatic directory move. `WH_STATE_PATH` and `WH_WORKTREE_BASE` are honoured when the corresponding `WRIT_*` variable is unset. The supervisor still has its own `WRIT_WORKTREE_BASE` resolver until [#152](https://github.com/rmems/writ/issues/152).

Use platform-aware XDG/user-data resolution in implementation. Never assume a Linux-only home-directory layout when an OS API is available.

## JSON and process boundary

Version 1 responses use this envelope shape:

```json
{"ok":true,"schema_version":1,"command":"cli.bootstrap","data":{},"error":null}
```

- Standard output is machine-readable JSON when `--json` is selected.
- Diagnostics belong on standard error.
- Additive fields are compatible within v1; removals or semantic renames require a schema-version change.
- Supervised execution is `writ supervisor run --timeout <secs>`; it is implemented, not reserved. Do not improvise a second timeout path in the CLI.

Response envelopes and error codes are illustrated by the fixtures in `docs/examples/`.


## Contribution workflow

Follow the portable worker contracts. They apply to every agent platform.

1. **[Safe Issue → Verified Commit](docs/workflows/safe-issue-verified-commit.md)** ([#84](https://github.com/rmems/writ/issues/84), isolation [#6](https://github.com/rmems/writ/issues/6)): read the issue and repo docs, isolate a worktree/branch, implement, run README gates, commit, push, comment on the issue with SHA. Never edit `main`.
2. **[Safe Verified Commit → PR](docs/workflows/safe-verified-commit-to-pr.md)** ([#8](https://github.com/rmems/writ/issues/8) / [RM-123](https://linear.app/rpd-34/issue/RM-123/issue-pr-workflow-never-auto-merge)): open or update a PR that links the issue, hand off URL + SHA, and never merge during that workflow. Review checklist: [`REVIEW.md`](REVIEW.md). The installed companion `babysit-pr` skill handles any interactive monitoring after handoff; an authorized one-shot merge remains a separate primary-agent action.

## Review expectations

Use [`REVIEW.md`](REVIEW.md) for the shared checklist. Reviewers should verify behavior at both the soft-policy and hard-enforcement layers, with particular attention to runtime merge prohibition and interactive merge authorization/preflight, force-push parsing, expected-branch checks, path traversal, JSON compatibility, cross-platform path handling, alternate or interactive interpreter routes, platform-specific wrapper operands and command-lookup overrides, nested environment resets and unsets, runtime configuration, lab child commands, ambient Git pagers and fsmonitor hooks, optional index-lock suppression, partial-clone lazy fetches, exact case-sensitive Git built-ins, positional config actions, named, peeled, sorted, and ref-format signature settings, alternate-ref options across revision consumers, clustered patch flags, and Git reads or mutations that can launch nested helpers, hooks, filters, viewers, transports, signature tools, credential helpers, diff tools, aliases, or archive formatters without preserving their capability requirements.

## Related planning

- Product epic: GitHub #1
- Current phase (hook enforcement): GitHub #124
- Threat model and boundary tests: GitHub #22, #81
- Linear project: <https://linear.app/rpd-34/project/worktrees-hives-e3052de4caa3>
