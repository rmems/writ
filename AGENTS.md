# AGENTS.md

## Purpose

This file defines how coding agents contribute to `writ` and how the future hive runtime divides responsibility. The project is a Rust workspace designed for multiple agent platforms.

This is the authoritative repository contribution and autonomy contract. `CLAUDE.md`, `SKILL.md`, `REVIEW.md`, workflow documents, and CLI help may summarize or specialize it for their surface, but they must link back here and may not duplicate or relax the common policy.

Interactive PR monitoring belongs to the installed companion `babysit-pr` skill. It is operator guidance, not a security boundary: Rust `writ-core` is the hard code-enforced boundary for worktree, branch, path, process, push, auto-merge, merge-queue, and default-branch protection. Local feature-branch integration is allowlisted; GitHub owns remote PR merges.

## Non-negotiable safety

**These rules are absolute. No agent, orchestrator, or platform may invent an additional exception.** They do **not** ban routine local integration in an assigned worktree.

### Core prohibitions

- **Never enable auto-merge or a merge queue** through `writ`, `gh-safe`, or GitHub APIs. Deferred automation can act on a later, unreviewed head.
- **Never use bare `git push --force`** or `git push -f`. Only `--force-with-lease` is permitted, and only for rebasing your own assigned branch.
- **Never edit outside** a job's assigned worktree or branch. Do not seize another worker's live worktree or discard their uncommitted work.
- **Never merge a GitHub pull request** through `gh pr merge`, GraphQL `mergePullRequest`, REST merge, MCP merge, or an unattended runtime path. GitHub repository rules, required checks, and reviews own protected-branch integration. `writ` does **not** implement a merge-permission protocol and must not grow one.
- **Repository scope** is a **configured owner allowlist** (env `WRIT_ALLOWED_OWNERS` and/or explicit API args). There is no built-in default org; operators supply the owners they manage. Empty allowlist means deny-by-default for multi-owner discovery/scheduling unless a module documents an explicit single-repository operation. `writ-core` rejects worktree creation and `gh` repository selectors (`-R` / `--repo` in any pflag spelling, plus `GH_REPO`) whose owner is missing from that list.
- **Process stacked PRs** from the bottom of the stack upward.
- **Post review replies** only after pushing, and include the pushed SHA plus agent attribution. Coordination messages that do not land code must not invent a SHA.
- **Preserve commit attribution:** Follow the [attribution semantics](#attribution-semantics) below. Never rewrite a Cursor-authored or Cursor-co-authored commit merely to change attribution; add a new correctly attributed commit instead.
- **GitHub MCP first (non-negotiable for agents):** For PR status, CI check runs, review threads, issue reads, and PR comments, use the **GitHub MCP** (`github__pull_request_read`, list/comment tools, etc.). Do **not** default to shell `gh` for reads. Shell `gh` is allowed only when MCP is unavailable (e.g. 503) or for operations MCP cannot perform. Local `git` remains for branch/rebase/merge/push. Do **not** hardcode org/owner names in product code or agent docs — owners come only from `WRIT_ALLOWED_OWNERS` / explicit API args.

### Local collaboration (allowed)

In an **assigned feature-branch worktree**, workers may integrate compatible peer work:

- `git merge`, `git rebase`, and `git cherry-pick` of another job's branch into the assigned branch are routine. Conflict repair (`merge --continue` / `--abort` / `--quit`, editing contended files) is expected.
- Negotiate overlapping writes: identify the current owner and hand off; do not silently diverge. Shared visibility lives in the existing lease store and watchlist — do **not** invent another database.
- Refuse a local merge that would overwrite uncommitted WIP; commit, stash, or abort first.
- Do not merge into `main`/`master` locally. Default-branch writes belong to GitHub policy.

### Deny-list (never execute)

| Command / Operation | Reason |
| --- | --- |
| `gh pr merge`, GraphQL `mergePullRequest`, REST merge, MCP merge | GitHub owns remote PR integration; the runtime must not provide an unattended merge path. |
| `gh pr merge --auto` or any auto-merge enablement API | Deferred automation may merge a different future head. |
| Merge-queue enablement or enqueue operation | A queue is deferred merge automation. |
| `git merge` / `git pull` on `main` or `master` | Default-branch integration is not assigned-worktree collaboration. |
| `git push --force` (bare) | Destructive; loses history. |
| `git push -f` (bare) | Short form of same destructive push. |

### Remote GitHub merges

GitHub repository rules, permissions, required checks, and reviews are the intended authority for merging a pull request into a protected branch. `writ` must not duplicate that with a seven-step one-shot merge ritual.

If protection is missing (for example `main.protected=false` and no rulesets), **report that operator gap**. Do not change repository settings, enable unattended remote merging, or treat the gap as a reason to rebuild a writ merge-permission engine.

This document never authorizes merging a specific pull request.

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

An explicit user request to implement scoped work authorizes the assigned worker to create the scoped branch/worktree, edit code, commit, make the first push, and create the PR without repeated confirmation. That authority never authorizes a GitHub pull-request merge, auto-merge, merge queue, destructive action, or work outside the assigned scope. Local integration of peer work into the assigned feature branch is allowed. Rust remains the hard enforcement boundary.

- **Beads:** Use Beads as lightweight canonical state: one task per cohesive tranche and claim it before coding. Complete acceptance prose, Linear sync, GitHub child issues, project metadata, and audit reports may follow implementation, but must be complete by PR handoff rather than blocking the first edit.
- **Isolation and identity:** A dirty or stale primary checkout is not a blocker. Preserve it, bootstrap a clean source/clone, and use `writ` for the assigned worktree. Prefer reclaim and clear identity over aborting a recoverable setup. A newly created, unpublished assigned branch must equal the verified remote-base commit before edits. A published branch contains job history and is not compared for equality with the base; fetch its expected upstream and verify the configured upstream plus the expected local/remote relationship instead. Stop on an unexpected remote commit, behind state, or divergence until it is reconciled safely.
- **Parallel work:** One writable worker owns one assigned worktree and branch. The manager coordinates separate workers through explicit assignments, status, dependencies, and handoffs. Never allow multiple writers to share one worktree, even for declared disjoint paths. One controller retains commit and push authority for each assignment.
- **Review and validation:** After the first tested implementation, require one independent review matched to the risk before final publication. Add review only for a named high-risk boundary or an actual finding that warrants follow-up. Run focused gates during work. Immediately before publication, align or rebase an unpublished branch onto the verified base, or reconcile a published branch with its expected upstream, then run exactly one complete native gate suite on the exact would-be-pushed head; any later tree change invalidates that run. An issue may add focused checks; it must not replace or reduce that final suite. Do not require serial policy audits or duplicate full-suite runs from every subagent.
- **Routine remediation:** Automatically fix safe mechanical findings within scope. Stop for a genuine ownership collision, a destructive or out-of-scope action, an unresolved Critical/Important correctness issue, a material user design decision, or an explicit fail-closed condition in a portable worker contract. A required gate failure or timeout blocks commit/push handoff until it is repaired or the user explicitly changes scope; an in-scope repair does not require another confirmation.
- **GitHub access:** GitHub MCP remains preferred; when it is unavailable, use `gh` immediately rather than waiting for connector retries.
- **Handoff:** Commit, push, and PR handoff are expected outcomes of authorized implementation. Interactive monitoring belongs to the companion `babysit-pr` skill. GitHub owns remote PR merges; this document does not authorize merging a specific pull request.

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
2. **Rust core (`writ-core`):** Hard enforcement. Rejects unsafe git/GitHub operations — including `gh pr merge`, auto-merge, merge-queue, default-branch local merge, dirty-WIP merge, and bare force-push — at the process boundary. Local feature-branch `git merge` is allowlisted. Authoritative safety layer for the product runtime.
3. **GitHub repository protection:** Intended authority for merging a pull request into a protected branch. `writ` does not implement a merge-permission protocol. If protection is missing, report the operator gap; do not rebuild a writ merge engine.

Rust must enforce safety-sensitive runtime mutation rules. Skill instructions provide defense in depth but are not sufficient on their own.

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

For authorized implementation, complete the cohesive tranche: run focused gates during work and the complete native gate suite once on the exact HEAD before push; commit, push, and create or update the PR for handoff. Record remaining follow-up in Beads and complete required tracking and metadata by PR handoff. Do not add redundant full-suite runs, serial audits, or cleanup that is unrelated to the tranche. A specific user instruction that withholds a push or PR action controls that action. GitHub PR merges remain outside this workflow; local feature-branch integration is allowed.
<!-- END BEADS INTEGRATION -->

## Architecture

The v1 decision that `writ` is the only authoritative runtime, that `SKILL.md` files are thin clients, and that Codex `babysit-pr` is not replaced by a `writ` skill repo is recorded in [ADR 0001](docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md).

`writ` is a **Rust workspace**. One binary owns both layers:

- **Enforcement** — git worktrees, exact-base identity, path sandboxing, process supervision/timeouts, and **hard safety enforcement** (no GitHub PR merge path, local feature-branch merge allowlisted, force-with-lease only, branch verification).
- **Coordination state** *(M1, partial)* — `writ hook` grants and releases SQLite lease rows (`leases` + `agents`). Path scopes, ownership freeze modes, and the rest of the coordination schema remain later work. `state.rs` still only *reads* `watched.json`; that file is superseded by the lease store, not given a writer.
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
| Rust core and CLI | Resolve sandboxed paths, supervise child processes, verify branches, reject unsafe git/GitHub operations, dispatch `writ hook`, and hold SQLite lease rows. `writ install` and path-scoped coordination remain later M1 work. |
| External tools | Runtime `git` and `gh` operations are selected and validated by Rust. GitHub repository rules own remote PR merges. The OS supplies filesystem and process primitives. |

**Why enforce at the hook boundary?** A tool that must be *called* to help is advisory: an agent that does not call it is unconstrained. As a `PreToolUse` hook, enforcement applies to the agent's own commands whether or not the agent cooperates, and a blocking exit cannot be overridden by another hook. Hard stops live in Rust, at the binary boundary, so a malformed prompt cannot bypass them.

## Source ownership

### Rust

Rust code lives in `crates/`:

- `crates/writ-core/` is the reusable library and source of truth for worktrees, state, process execution, paths, and safety policy.
- `crates/writ/` is the `writ` command-line adapter. It parses arguments, calls `writ-core`, emits human or JSON output, and maps policy failures to exit code 2.

Keep security boundaries in `writ-core`, not only in the CLI parser. Git must be invoked as a subprocess rather than through libgit2. New mutating commands require branch verification and path-sandbox tests. The hook dispatcher (`crates/writ-core/src/hook.rs`) is the production PreToolUse/`WorktreeCreate` boundary.

### Agent skill

The installable `SKILL.md` will own platform-facing prompts and command guidance. It may adapt spawning instructions to a host platform, but it must preserve the same safety invariants and call the `writ`/Rust boundary for mutating work instead of bypassing it. Local assigned-branch integration goes through that boundary; GitHub PR merges do not.

## Data flow

**Supported today.** Exact-base worktree creation, `writ git-safe` / `writ gh-safe` / `writ supervisor`, and `writ hook` (JSON on stdin) are implemented. Claude Code does not register that hook until `writ install` lands.

1. The operator or agent supplies GitHub or Linear issue/PR context.
2. The harness (Claude Code agent teams, `/batch`, or an equivalent) assigns work and creates an isolated worktree — or `writ worktree create` does, which is the supported path today.
3. On `WorktreeCreate`, `writ hook` verifies the exact start point, creates or reattaches the worktree, and records a lease row. A non-zero exit aborts creation.
4. A worker agent changes only that worktree and branch.
5. On `PreToolUse`, `writ hook` validates each `git`/`gh` mutation and blocks an unsafe one with exit 2, which no other hook can override. Until the hook is registered, validation also happens when `writ git-safe` / `writ gh-safe` is invoked.
6. On `WorktreeRemove`, the lease row is released.
7. The installed companion `babysit-pr` skill handles interactive monitoring after a PR handoff.
8. A human decides whether to merge the pull request on GitHub. `writ` does not merge PRs. Local feature-branch integration may already have happened in the assigned worktree.

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
2. **[Safe Verified Commit → PR](docs/workflows/safe-verified-commit-to-pr.md)** ([#8](https://github.com/rmems/writ/issues/8) / [RM-123](https://linear.app/rpd-34/issue/RM-123/issue-pr-workflow-never-auto-merge)): open or update a PR that links the issue, hand off URL + SHA, and do not merge the pull request during that workflow. Review checklist: [`REVIEW.md`](REVIEW.md). The installed companion `babysit-pr` skill handles any interactive monitoring after handoff. GitHub owns remote PR merges.

## Review expectations

Use [`REVIEW.md`](REVIEW.md) for the shared checklist. Reviewers should verify behavior at both the soft-policy and hard-enforcement layers, with particular attention to GitHub PR merge prohibition, default-branch and dirty-WIP local merge blocks, force-push parsing, expected-branch checks, path traversal, JSON compatibility, cross-platform path handling, alternate or interactive interpreter routes, platform-specific wrapper operands and command-lookup overrides, nested environment resets and unsets, runtime configuration, lab child commands, ambient Git pagers and fsmonitor hooks, optional index-lock suppression, partial-clone lazy fetches, exact case-sensitive Git built-ins, positional config actions, named, peeled, sorted, and ref-format signature settings, alternate-ref options across revision consumers, clustered patch flags, and Git reads or mutations that can launch nested helpers, hooks, filters, viewers, transports, signature tools, credential helpers, diff tools, aliases, or archive formatters without preserving their capability requirements.

## Related planning

- Product epic: GitHub #1
- Current phase (hook enforcement): GitHub #124
- Threat model and boundary tests: GitHub #22, #81
- Architecture decision: [ADR 0001](docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md) (Linear [RM-169](https://linear.app/rpd-34/issue/RM-169/v1-record-rust-only-architecture-and-codex-babysit-pr-boundary))
- Linear project: <https://linear.app/rpd-34/project/worktrees-hives-e3052de4caa3>
