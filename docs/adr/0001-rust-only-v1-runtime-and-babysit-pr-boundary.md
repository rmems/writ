# ADR 0001: Rust-only v1 runtime and Codex `babysit-pr` boundary

- Status: Accepted
- Date: 2026-09-15
- Linear: [RM-169](https://linear.app/rpd-34/issue/RM-169/v1-record-rust-only-architecture-and-codex-babysit-pr-boundary) (parent [RM-168](https://linear.app/rpd-34/issue/RM-168/epic-worktree-hive-v1-rust-only-control-plane))
- Follow-on command/state model: [RM-170](https://linear.app/rpd-34/issue/RM-170/v1-define-unified-wh-command-and-rust-state-model)

## Context

v1 started as Worktree Hive, with CLI name `wh`. The GitHub repository and binary are now `writ`. This decision applies to that single runtime regardless of the rename; `wh` and Worktree Hive are historical names for the same product.

Two pressures collided:

1. **A second control plane is unsafe.** A Python orchestrator that owned workflow, job state, or direct `git`/`gh` mutation could diverge from Rust policy. Prompt text cannot be the security boundary.
2. **PR babysitting is already a skill.** OpenAI Codex ships [`babysit-pr`](https://github.com/openai/codex/blob/main/.codex/skills/babysit-pr/SKILL.md) for persistent PR monitoring, CI diagnosis, flaky retries, review handling, and continued polling. Publishing a competing `pr-babysit` repository would duplicate that loop instead of hardening the primitives it should call.

The operational contract lives in [`AGENTS.md`](../../AGENTS.md). This ADR records the architecture decision that contract implements.

## Decision

### 1. `writ` is the only authoritative runtime and mutation boundary

The v1 control plane is one Rust binary: `writ` (CLI) over `writ-core` (policy, worktrees, paths, process supervision).

Required production architecture:

```text
Agent / SKILL.md
        |
        | intent and operator context
        v
writ (CLI) -> writ-core (policy)
        |
        | allowlisted subprocess operations
        v
git / gh / operating system
```

Claude Code hooks (`PreToolUse`, `WorktreeCreate`, `WorktreeRemove`, `SubagentStart`/`SubagentStop`) are the planned unbypassable admission path (GitHub #124). They dispatch into the same Rust binary. They are not a second policy engine.

`writ` owns:

- exact-base worktree identity and path sandboxing
- `git`/`gh` mutation allowlists (`git-safe`, `gh-safe`)
- process supervision and timeouts
- runtime rejection of merge, auto-merge, merge-queue, and bare force-push
- job/watched state, once a writer exists (M1)

This ADR does not add a merge command to `writ`. A primary interactive agent may perform a human-authorized one-shot merge only through the host connector protocol in `AGENTS.md`. That path is unavailable to the runtime, workers, orchestrators, and companion monitoring skills.

### 2. `SKILL.md` files are thin, platform-facing clients

Installable skills (`SKILL.md`, `CLAUDE.md`, host-specific companions) adapt prompts to a platform. They are not an authoritative runtime, state store, or mutation policy.

A skill may:

- discover work, spawn workers, and report results
- tell an agent *when* to call `writ`
- hand a PR to an installed companion `babysit-pr` skill for interactive monitoring

A skill may not:

- relax `AGENTS.md`
- own a second job-state machine
- invoke production `git`/`gh` mutations except by routing them through `writ`

If the Rust boundary is unavailable, the mutating flow stops. Re-implementing the allowlist in a wrapper that then calls `git` directly is prompt-level text, not enforcement.

### 3. No separate `pr-babysit` repository in v1

v1 will not publish a standalone `pr-babysit` (or equivalently named) repository, skill pack, or competing babysitting product.

Interactive PR monitoring belongs to the **installed companion** `babysit-pr` skill on the host (Codex's skill, or another platform's equivalent). `writ` exposes hardened worktree, state, policy, and PR-supervision **primitives** those skills can call. Command names must not claim that `writ` replaces Codex `babysit-pr`. A future `supervise-pr` command is a runtime primitive (see RM-170), not a duplicate skill.

### 4. Production `git`/`gh` mutation outside Rust policy is prohibited

For production and fleet use, every mutating `git` or `gh` invocation goes through `writ`:

| Allowed production path | Not a production mutation path |
| --- | --- |
| `writ git-safe …` | raw `git` that mutates refs, the index, or the worktree |
| `writ gh-safe …` | raw `gh` that mutates PRs, checks, comments, or repo state |
| `writ worktree create` / `remove` / `prune` | raw `git worktree add` / `remove` |
| `writ supervisor run …` | an unsupervised helper that then mutates |

Reads are not mutations. GitHub MCP is preferred for PR/issue/check/review reads; shell `gh` reads are a fallback when MCP is unavailable. Local `git` reads used only to inspect identity (`rev-parse`, `status`, `branch --show-current`) are not a second control plane.

The host-connector one-shot merge is the sole documented exception, and only after the complete human-authorization protocol. It is not a `writ` command and is not available to babysitting flows.

**Honest current enforcement.** Until M1 hooks land, policy applies only to commands that actually enter `writ`. This ADR states the required production path. It does not claim that raw `git`/`gh` is currently impossible on the operator's machine.

### 5. Future Python, if retained, is a thin `writ --json` client only

Python is not part of the required v1 runtime. The previous Python orchestrator was removed (GitHub #144).

If a Python SDK or helper is retained later, it may only:

- spawn `writ --json …`
- parse the versioned JSON envelope on stdout
- surface exit codes (`0` success, `1` operational failure, `2` policy violation)

It must not own workflow, persist a second job-state schema, or call `git`/`gh` itself. The consumption pattern already shown in [`docs/status-schema.md`](../status-schema.md) is the allowed shape.

## Codex `babysit-pr` integration and compatibility

Reference skill: [openai/codex `babysit-pr`](https://github.com/openai/codex/blob/main/.codex/skills/babysit-pr/SKILL.md).

That skill owns the **monitoring loop**: poll PR/CI/review state, classify branch vs flake, retry likely flakes within its budget, patch the PR head when the failure or review item is in-scope, and keep watching until the PR is merged/closed or the operator must intervene. A green, mergeable, review-clean PR is a progress milestone, not a license to merge.

`writ` owns the **enforcement primitives** that loop should call when the job is under this runtime:

| `babysit-pr` action | v1 expectation under `writ` |
| --- | --- |
| Snapshot PR/CI/review (read) | GitHub MCP first; `gh` read fallback. Not a `writ` mutation. |
| Patch code on the PR head | Assigned worktree/branch only; identity checklist in `AGENTS.md`. |
| `git push` / `--force-with-lease` | `writ git-safe` on the assigned branch. Bare `--force`/`-f` remain forbidden. |
| `gh` writes (rerun checks, resolve eligible threads, comments) | `writ gh-safe`. Merge, auto-merge, and merge-queue remain blocked. |
| “Until merged or closed” | Observational stop condition. The skill must not merge. |
| Ready-to-merge report | Handoff signal only. Human merge, or the separately authorized primary-agent one-shot protocol. |

Compatibility rules:

1. **Do not fork or republish** Codex `babysit-pr` as a `writ` product.
2. **Do not vendor** its watcher scripts as a second orchestrator.
3. **When a babysit skill operates inside a `writ`-managed worktree**, production mutations go through `writ git-safe` / `writ gh-safe` (and later `supervise-pr` if added). The Codex skill's default raw `git`/`gh` examples are host defaults, not an exemption from this runtime.
4. **This repository's `SKILL.md`** is a portable client that hands monitoring to the installed companion skill; it is not a reimplementation of the Codex watcher.
5. **Naming:** `supervise-pr` (or successor) describes a primitive. It must not be marketed or documented as “the `writ` babysit-pr skill.”

## Consequences

- Skills stay portable across Codex, Claude, Cursor, and later hosts because they share one mutation boundary.
- PR monitoring quality can improve on the host skill without a fork in this repository.
- RM-170 can define the unified command surface, including a non-skill `supervise-pr` primitive, without reopening the product-boundary question.
- Operators must route fleet mutations through `writ` until hooks make that path unbypassable.
- A later Python SDK, if any, is documentation-and-wrapper work, not a control-plane revival.

## Non-goals

- Reimplementing OpenAI Codex `babysit-pr`.
- Automated merging, auto-merge, or merge queues.
- A Python-owned workflow, state store, or direct `git`/`gh` mutation path.
- Changing the live `writ` command surface in this ADR (that is RM-170).
