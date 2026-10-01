# AGENTS.md

## Purpose

This file defines how coding agents contribute to `writ` and how the collaboration layer divides responsibility. The project is a Rust workspace designed for multiple agent platforms.

`writ` is a **Rust-first, provider-neutral coordination layer for parallel coding agents**. Native harnesses (Cursor, Claude Code, Codex, plain `git`) own worker/session creation and checkout/worktree lifecycle. `writ` registers existing checkouts and supplies shared coordination state: task/agent/session identity, checkout/branch/head identity, ownership leases, declared paths and overlap visibility, intent/dependency/blocker/help/handoff messages, and pause/timeout/recovery state.

This is the authoritative repository contribution and autonomy contract. `CLAUDE.md`, `SKILL.md`, `REVIEW.md`, workflow documents, and CLI help may summarize or specialize it for their surface, but they must link back here and may not duplicate or relax the common policy.

Interactive PR monitoring belongs to the installed companion `babysit-pr` skill. It is operator guidance, not a security boundary: Rust `writ-core` is the hard code-enforced boundary for worktree, branch, path, process, and push controls. GitHub owns remote source, PR, review, check, and protected-branch merge authority; Linear is the task tracker this repository's maintainers use. Neither is duplicated by `writ`.

## Non-negotiable safety

**These rules are absolute. No agent, orchestrator, or platform may invent an additional exception.** They do **not** ban routine local integration in an assigned worktree — they protect WIP and coordination integrity.

### Core prohibitions

- **Never use bare `git push --force`** or `git push -f`. Only `--force-with-lease` is permitted, and only for rebasing your own assigned branch.
- **Never edit outside** a job's assigned worktree or branch. Do not seize another worker's live worktree or discard their uncommitted work.
- **Repository scope** is a **configured owner allowlist** (env `WRIT_ALLOWED_OWNERS` and/or explicit API args). There is no built-in default org; operators supply the owners they manage. Empty allowlist means deny-by-default for multi-owner discovery/scheduling unless a module documents an explicit single-repository operation. `writ-core` rejects worktree creation and `gh` repository selectors (`-R` / `--repo` in any pflag spelling, plus `GH_REPO`) whose owner is missing from that list.
- **Process stacked PRs** from the bottom of the stack upward.
- **Review replies that claim a pushed fix** include the actual pushed SHA and agent attribution. Discussion, help, and intent messages need neither a push nor a fabricated SHA.
- **Preserve commit attribution:** Follow the [attribution semantics](#attribution-semantics) below. Never rewrite a Cursor-authored or Cursor-co-authored commit merely to change attribution; add a new correctly attributed commit instead.
- **Do not hardcode org/owner names** in product code or agent docs. Owners come only from `WRIT_ALLOWED_OWNERS` / explicit API args. Use the GitHub tools the host already authorizes. Local `git` remains for branch, rebase, merge, and push.

### Local collaboration (allowed)

In an **assigned feature-branch worktree**, workers may integrate compatible peer work:

- `git merge`, `git rebase`, and `git cherry-pick` of another job's branch into the assigned branch are routine. Conflict repair (`merge --continue` / `--abort` / `--quit`, editing contended files) is expected.
- Read-only review and investigation stay in the current checkout. They do not need a separate branch, pull request, or issue.
- Negotiate overlapping writes: identify the current owner and hand off; do not silently diverge. The same filenames on separate branches are a reason to talk, not a global lock. Shared visibility lives in the existing lease store and watchlist — do **not** invent another database, scheduler, permission engine, or cross-host transport.
- Refuse a local merge that would overwrite uncommitted WIP; commit, stash, or abort first.
- Do not merge into `main`/`master` locally. Default-branch writes belong to GitHub policy.

### Deny-list (never execute)

| Command / Operation | Reason |
| --- | --- |
| `git merge` / `git pull` on `main` or `master` | Default-branch integration is not assigned-worktree collaboration. |
| `git push --force` (bare) | Destructive; loses history. |
| `git push -f` (bare) | Short form of same destructive push. |

### Remote GitHub merges

GitHub repository rules, permissions, required checks, and reviews are the intended authority for merging a pull request into a protected branch.

If protection is missing (for example `main.protected=false` and no rulesets), **report that operator gap**. Do not change repository settings under this contract.

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

An explicit user request to implement scoped work authorizes the assigned worker to create the scoped branch/worktree, edit code, commit, make the first push, and create the PR without repeated confirmation. That authority never authorizes work outside the assigned scope. Local integration of peer work into the assigned feature branch is allowed. Rust remains the hard enforcement boundary.

- **Tracking and validation:** Follow [Contribution](#contribution). One Linear task plus a GitHub pull request is enough for maintainer work. Focused checks cover the change in progress. Merge or release readiness is a separate, truthful declaration.
- **Isolation and identity:** A dirty or stale primary checkout is not a blocker. Preserve it, bootstrap a clean source/clone, and register the assigned checkout with `writ worktree register`. Registration records the observed state; it never resets a checkout that is ahead of its base. Prefer reclaim and clear identity over aborting a recoverable setup. A newly created, unpublished assigned branch must equal the verified remote-base commit before edits. A published branch contains job history and is not compared for equality with the base; fetch its expected upstream and verify the configured upstream plus the expected local/remote relationship instead. Stop on an unexpected remote commit, behind state, or divergence until it is reconciled safely.
- **Parallel work:** One writable worker owns one assigned worktree and branch. Read-only helpers are not writers and do not receive a checkout. The manager coordinates separate writers through explicit assignments, status, dependencies, and handoffs. Never allow multiple writers to share one worktree, even for declared disjoint paths. One controller retains commit and push authority for each assignment.
- **Routine remediation:** Automatically fix safe mechanical findings within scope. Stop for a genuine ownership collision, a destructive or out-of-scope action, an unresolved Critical/Important correctness issue, a material user design decision, or an explicit fail-closed condition in a portable worker contract. A check cited for merge or release readiness must be repaired or reported as failed. A checkpoint may still be pushed when its test status is honest. An in-scope repair does not require another confirmation.
- **Handoff:** Commit, push, and a pull request are expected for authorized implementation. Interactive monitoring belongs to the companion `babysit-pr` skill. Do not merge the pull request from this contract.

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
2. **Rust core (`writ-core`):** Hard enforcement. Rejects unsafe git/GitHub operations — including default-branch local merge, dirty-WIP merge, and bare force-push — at the process boundary. Local feature-branch `git merge` is allowlisted; the hook verifies merge/pull against the event `cwd` (or an explicit `-C` target) and fails closed when neither is known. Authoritative safety layer for the product runtime.
3. **GitHub repository protection:** Intended authority for merging a pull request into a protected branch. If protection is missing, report the operator gap.

Rust must enforce safety-sensitive runtime mutation rules. Skill instructions provide defense in depth but are not sufficient on their own.

## Contribution

Maintainers and contributors follow this section. The safety rules above still apply. A Beads installer that reinserts a tracker block into this file is optional local context, not a second contract.

### Tracking

- Maintainers keep the task in Linear and open a GitHub pull request for the code, review, and checks. Do not file a GitHub issue twin, child issue, mirror, duplicate checklist, or metadata/audit-report gate.
- Contributors use a GitHub issue or pull request. They do not need Linear, Beads, or any other maintainer service.
- Existing GitHub issues and pull-request links stay. Do not delete them to satisfy this rule.

### Optional execution aids

Beads, Dolt, and the host's own task list are optional. Missing `bd` must not stop a session from starting. Do not delete existing Beads data, task databases, or unrelated hooks.

### Checkpoint and readiness

- While working, run focused checks for the change.
- Scoped commits, checkpoint pushes, and draft pull requests are allowed. State which checks ran, which did not, and any failure. Do not publish secrets or use destructive git.
- Declare merge or release readiness only after the validation and review that change needs, and only when required GitHub checks are actually green. A full workspace suite or a separate audit is for that declaration, not for every checkpoint.
- Do not describe an unrun or failing check as passed, and do not weaken GitHub's required checks.

### Collaboration

- Read-only review and investigation do not get their own writable checkout, branch, pull request, or mirrored issue.
- Each independent writer uses a harness-owned isolated checkout, registered with `writ worktree register`. Preserve peer WIP and authentic attribution.
- Do not add another claim store, scheduler, permission engine, or cross-host transport.

### Replies and host tools

- A review reply that claims a pushed fix includes that pushed SHA.
- Discussion, help, and intent messages need neither a push nor a fabricated SHA.
- Use the tools the host already authorizes. This repository does not mandate a GitHub client or a task list.

## Architecture

The v1 decision that `writ` is the only authoritative runtime, that `SKILL.md` files are thin clients, and that Codex `babysit-pr` is not replaced by a `writ` skill repo is recorded in [ADR 0001](docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md).

`writ` is a **Rust workspace**. One binary owns both layers:

- **Coordination state** — `writ worktree register`/`unregister`/`inspect`/`list` and `writ hook` grant and release SQLite lease rows (`leases` + `agents`) for harness-created checkouts. `writ status` / `writ jobs` read that same store. Path scopes, declared-path overlap, messages/handoffs, and ownership transfer build on this store — no second database. `state.rs` still only *reads* leftover `watched.json`; that file is superseded by the lease store, not given a writer, and is not the status authority.
- **Safety boundary** — exact-base identity, path sandboxing, process supervision/timeouts, branch verification, and force-with-lease-only pushes, enforced at the process boundary to protect WIP and coordination integrity.
- **Agent skill (`SKILL.md`)** — portable prompts describing when and how agents call the CLI on any platform.

```text
Harness (Cursor / Claude Code / Codex / git)
       |
       | creates + isolates workers and checkouts
       v
Claude Code hooks (PreToolUse, SubagentStart/Stop; WorktreeCreate/WorktreeRemove are
  coordination-only and no longer installed by `writ install`)
       |
       | hook JSON on stdin; exit 2 blocks, and cannot be overridden
       v
Rust binary: writ -> writ-core
       |  SQLite coordination store (leases, agents) — same host
       |  validated subprocess operations
       v
git / gh / operating system
       |
       v
GitHub (source, PRs, reviews, checks) · Linear (maintainer task record)
```

| Layer | Responsibilities |
| --- | --- |
| Agent skill | Describe when to discover work, spawn subagents, and report results. The installed companion `babysit-pr` skill handles interactive PR monitoring. Prompt content is portable guidance, not a security boundary. |
| Rust core and CLI | Hold SQLite lease rows, register checkouts, resolve sandboxed paths, supervise child processes, verify branches, reject unsafe git/GitHub operations, dispatch `writ hook`. Path-scoped coordination and messaging are later work on the same store. |
| External tools | Runtime `git` and `gh` operations are selected and validated by Rust. GitHub repository rules own protected-branch merges; Linear is the maintainers' task record. The OS supplies filesystem and process primitives. |

**Why enforce at the hook boundary?** A tool that must be *called* to help is advisory: an agent that does not call it is unconstrained. As a `PreToolUse` hook, enforcement applies to the agent's own commands whether or not the agent cooperates, and a blocking exit cannot be overridden by another hook. Hard stops live in Rust, at the binary boundary, so a malformed prompt cannot bypass them.

## Source ownership

### Rust

Rust code lives in `crates/`:

- `crates/writ-core/` is the reusable library and source of truth for worktrees, state, process execution, paths, and safety policy.
- `crates/writ/` is the `writ` command-line adapter. It parses arguments, calls `writ-core`, emits human or JSON output, and maps policy failures to exit code 2.

Keep security boundaries in `writ-core`, not only in the CLI parser. Git must be invoked as a subprocess rather than through libgit2. New mutating commands require branch verification and path-sandbox tests. The hook dispatcher (`crates/writ-core/src/hook.rs`) is the production PreToolUse boundary; worktree lifecycle events routed to it are coordination-only, never filesystem mutations.

### Agent skill

The installable `SKILL.md` will own platform-facing prompts and command guidance. It may adapt spawning instructions to a host platform, but it must preserve the same safety invariants and call the `writ`/Rust boundary for mutating work instead of bypassing it. Local assigned-branch integration goes through that boundary.

## Data flow

**Supported today.** Checkout registration (`writ worktree register`/`unregister`/`inspect`/`list`), `writ status` / `writ jobs` (lease-store snapshot), `writ git-safe` / `writ gh-safe` / `writ supervisor`, and `writ hook` (JSON on stdin) are implemented. The managed lifecycle commands (`worktree create`/`remove`/`prune`) are deprecated. Claude Code does not register that hook until the operator runs `writ install` (implemented; held back from shared settings pending the [#124](https://github.com/rmems/writ/issues/124) burn-in).

1. The operator or agent supplies Linear task or GitHub PR context.
2. The harness (Claude Code agent teams, `/batch`, Cursor, plain `git worktree add`, or an equivalent) assigns work and creates the isolated checkout wherever it wants.
3. `writ worktree register <path>` (or a coordination-only `WorktreeCreate` hook event, where still wired) records a lease row for the existing checkout. Registration never creates, moves, fetches, resets, or deletes anything; a standalone clone and a dirty or detached checkout register fine.
4. A worker agent changes only that checkout and its branch.
5. On `PreToolUse`, `writ hook` validates each `git`/`gh` mutation and blocks an unsafe one with exit 2, which no other hook can override. Until the hook is registered, validation also happens when `writ git-safe` / `writ gh-safe` is invoked.
6. `writ worktree unregister` (or a `WorktreeRemove` hook event) releases the lease row. The checkout itself is the harness's to delete — writ never removes it, and expiring a claim never erases WIP.
7. The installed companion `babysit-pr` skill handles interactive monitoring after a PR handoff.
8. A timeout or hang on `writ supervisor` is a recovery and handoff event: contain the child, record residual state, and leave the harness-owned checkout in place. Policy: [`docs/timeout-policy.md`](docs/timeout-policy.md). Do not improvise a second timeout path in the CLI.
