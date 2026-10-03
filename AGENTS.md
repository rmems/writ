# AGENTS.md

See @CLAUDE.md for additional repository context. The @-mention makes Amp load it; Amp reads `CLAUDE.md` on its own only when no `AGENTS.md` exists. Where the two files overlap, this file takes precedence, as `CLAUDE.md` itself states.

## Purpose

`writ` is a Rust-first, provider-neutral coordination layer for parallel coding agents. Native harnesses create workers and checkouts. Writ records identity and ownership in a shared same-host SQLite store, exposes collaboration status, and supplies bounded process supervision. Claims, messages and handoffs build on that store; see the current capability inventory in [README.md](README.md).

This is the repository contribution contract. `CLAUDE.md`, `SKILL.md`, `REVIEW.md` and workflows link here and specialize their own surfaces. User instructions determine the authorized scope. GitHub owns remote permissions, repository rules, reviews, checks and merge policy.

## Non-negotiable safety

### Core prohibitions

- Preserve another worker's checkout, branch and uncommitted changes. Never seize a live assignment or discard WIP to make integration easier.
- Never use bare `git push --force` or `git push -f`. Use `--force-with-lease` only for an authorized rewrite of your own branch after verifying the expected remote ref.
- Keep edits within the assigned checkout and scope. Verify repository and branch identity before local mutation or publication.
- Never fabricate a pushed SHA, a successful check, a completed handoff, or a merge. A rejected push is not delivery.
- Preserve commit attribution. Do not rewrite another agent's commit solely to change credit.

### Local collaboration (allowed)

Merge, rebase and cherry-pick compatible peer work into the assigned branch. Branch names are not a permission system. Resolve ordinary conflicts in the assigned checkout, use `--continue`, `--abort` or `--quit` as appropriate, and preserve uncommitted work. Writ's direct local merge helpers conservatively refuse dirty-tree integration; commit or stash your own WIP first.

Direct `git-safe` branch pinning with `--expected-branch` is optional; omitting it does not prove runtime assignment or consult registration. Supervision requires that pin for direct mutating Git commands. See the [CLI contract](docs/cli-contract.md#state-and-authority-boundaries) for the checks each path provides.

Each independent writer owns its checkout and branch. Read-only helpers may inspect the current checkout without creating a worktree, branch, issue or PR. Reuse a suitable existing checkout; create a separate checkout when independent writes need isolation. Harnesses choose its location. Registration and supervision do not require placement beneath a writ-managed root.

Overlapping filenames across separate branches are a coordination signal. Identify the owner, agree on a split or handoff, and select one integration owner. Keep useful ownership, conflict and recovery state in the existing lease store. Do not invent another coordination database. Process stacked PRs from the bottom upward.

### Remote GitHub merges

GitHub decides whether a remote operation is permitted. Writ does not maintain a second merge approval protocol or ban remote PR merge, readiness, update-branch, auto-merge or merge modes. Admission through a helper is not operator authorization: perform remote merges only when the user or established automation policy authorized them. A request to implement or review a PR alone does not authorize merging it.

Remote-only GitHub actions need no local branch or checkout. Operations that switch or delete the local job branch remain local ownership concerns. Report missing GitHub protection accurately; do not change repository settings unless requested.

### Allow-list for force-with-lease

Before an authorized `--force-with-lease` push, verify the assigned branch, intended remote, and exact remote ref. Stop on an unexpected peer push until it is reconciled. Never rewrite shared history merely to simplify a review.

### Attribution semantics

The primary Git author identifies the responsible contributor. `Co-authored-by` credits an additional contributor. `Agent` identifies the coding agent. Every Codex-authored commit contains:

```text
Agent: Codex
Co-authored-by: Codex <noreply@openai.com>
```

Audit commits reachable from the submitted PR head and not its base. Exclude synthetic review merges and test fixtures. A co-author trailer alone does not prove the primary author. Review replies that report a pushed fix include the real pushed SHA and agent attribution; coordination and no-code messages need no invented SHA or prior push. The attribution formatter is optional.

### Team-maintainer operating model

- **Task context:** Use the request or existing task supplied by the operator. Maintainers use Linear when useful; external contributors need no private tracker account. Do not create GitHub issue twins, mirror databases or extra tickets as an implementation prerequisite. Native task tools are optional. The repository has no Beads integration; no tracker installation, claim, migration or sync is required.
- **Isolation and identity:** Inspect the assigned checkout, current branch, relevant base and expected upstream. Preserve existing WIP and branch history. A branch need not equal the remote base before an edit or checkpoint; reconcile actual conflicts and unexpected peer changes before integrating or publishing.
- **Autonomy:** Scoped implementation authorizes ordinary local changes, tested commits, pushes and reviewable PRs unless the user withholds an action. One controller owns publication for each assignment. Handle safe in-scope repairs without repeated confirmation.
- **Checkpointing:** Focused checks and honest test status are sufficient to publish a checkpoint or draft. Missing optional helpers, tracker setup, a required rebase ritual, metadata paperwork or a full-suite rerun must not prevent useful work from being saved.
- **Readiness:** Before claiming review/merge readiness, obtain an independent review appropriate to risk, run the relevant native gates on the submitted code, address actionable findings, and inspect the current GitHub checks. An unrun or failed gate remains an explicit residual. Do not weaken checks or relabel a checkpoint as ready.
- **Tools:** Use tools authorized by the host. Registration, wrappers, hooks, attribution and companion monitoring are helpers, not mandatory task-entry gates. Be explicit about which checks a chosen path provides.

### Branch/worktree pre-edit checklist

Confirm the intended repository, assigned checkout and branch; inspect existing changes and peer ownership. Work in that checkout. Reuse it when safe. Resolve genuine ownership collisions or destructive/out-of-scope steps before proceeding; ordinary local conflicts and stale primary checkouts are recoverable.

### Enforcement layers

Rust implements the checks of the helper being called: local branch identity, WIP preservation, safe push forms, explicit repository targeting, lease transitions and process containment. `WRIT_ALLOWED_OWNERS` / explicit API arguments supply repository owners; no personal/org default is compiled in. Legacy managed lifecycle path checks remain confined to those deprecated commands.

Writ is not a recursive shell sandbox or a universal remote authorization engine. Host scripts and opaque API payloads can perform actions beyond argv inspection. The harness owns execution permissions; GitHub owns remote authorization. Installing hooks is opt-in and must preserve unrelated user hooks.

## Session Completion

Deliver the requested change with measured validation and a reviewable PR when authorized. Report the actual pushed SHA, local/hosted checks, review state and remaining work. Update only the tracker already used for the task. Do not require duplicate reports or tracker mirrors. Monitoring is optional and belongs to the installed companion `babysit-pr` skill. Merge and release actions require their own authorization.

## Architecture

The v1 decision that `writ` is the only authoritative runtime, that `SKILL.md` files are thin clients, and that Codex `babysit-pr` is not replaced by a `writ` skill repo is recorded in [ADR 0001](docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md).

`writ` is a **Rust workspace**. One binary owns both layers:

- **Coordination state** — `writ worktree register`/`unregister`/`inspect`/`list` and `writ hook` grant and release SQLite lease rows (`leases` + `agents`) for harness-created checkouts. `writ status` / `writ jobs` read that same store. Path scopes, declared-path overlap, messages/handoffs, and ownership transfer build on this store — no second database. `state.rs` still only *reads* leftover `watched.json`; that file is superseded by the lease store, not given a writer, and is not the status authority.
- **Safety boundary** — checkout/branch identity, process supervision/timeouts, WIP preservation, and force-with-lease-only pushes, enforced at the process boundary to protect WIP and coordination integrity.
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
GitHub (source, PRs, reviews, checks) · Linear (task tracking)
```

| Layer | Responsibilities |
| --- | --- |
| Agent skill | Describe when to discover work, spawn subagents, and report results. The installed companion `babysit-pr` skill handles interactive PR monitoring. Prompt content is portable guidance, not a security boundary. |
| Rust core and CLI | Hold SQLite lease rows, register checkouts, validate helper-specific paths, supervise child processes, verify local branches, protect WIP and check explicit repository targets, dispatch `writ hook`. Path-scoped coordination and messaging are later work on the same store. |
| External tools | Git and GitHub helpers apply their documented checks to direct invocations. GitHub repository rules own protected-branch merges; Linear owns task tracking. The OS supplies filesystem and process primitives. |

**Optional hook boundary.** Installed hooks apply these checks to recognized host events and commands; an exit-2 rejection cannot be overridden by another allow result. They do not recursively inspect arbitrary scripts or opaque API payloads. A host may use its authorized native workflow without installing writ hooks.

## Source ownership

### Rust

Rust code lives in `crates/`:

- `crates/writ-core/` is the reusable library and source of truth for worktrees, state, process execution, paths, and safety policy.
- `crates/writ/` is the `writ` command-line adapter. It parses arguments, calls `writ-core`, emits human or JSON output, and maps policy failures to exit code 2.

Keep local integrity and coordination checks in `writ-core`, not only in the CLI parser. Invoke Git as a subprocess rather than through libgit2. Test each new local mutation for its actual branch, path and WIP risks; remote-only operations need no local branch gate. The optional hook dispatcher (`crates/writ-core/src/hook.rs`) applies the same checks to recognized PreToolUse commands; worktree lifecycle events routed to it are coordination-only, never filesystem mutations.

### Agent skill

The installable `SKILL.md` will own platform-facing prompts and command guidance. It may adapt spawning instructions to a host platform, but it must preserve ownership and WIP and accurately describe the optional Rust helpers. A host may use its authorized native tools directly.

## Data flow

**Supported today.** Checkout registration (`writ worktree register`/`unregister`/`inspect`/`list`), `writ status` / `writ jobs` (lease-store snapshot), `writ git-safe` / `writ gh-safe` / `writ supervisor`, and `writ hook` (JSON on stdin) are implemented. The managed lifecycle commands (`worktree create`/`remove`/`prune`) are deprecated. Claude Code does not register that hook until the operator runs `writ install` (implemented; held back from shared settings pending the [#124](https://github.com/rmems/writ/issues/124) burn-in).

1. The operator supplies task scope, optionally through a Linear issue or GitHub PR.
2. The harness (Claude Code agent teams, `/batch`, Cursor, plain `git worktree add`, or an equivalent) assigns work and creates the isolated checkout wherever it wants.
3. `writ worktree register <path>` (or a coordination-only `WorktreeCreate` hook event, where still wired) records a lease row for the existing checkout. Registration never creates, moves, fetches, resets, or deletes anything; a standalone clone and a dirty or detached checkout register fine.
4. A worker agent changes only that checkout and its branch.
5. On `PreToolUse`, `writ hook` checks recognized `git`/`gh` commands for local integrity and configured repository scope; a rejected command exits 2. The same checks are available when `writ git-safe` / `writ gh-safe` is invoked.
6. `writ worktree unregister` (or a `WorktreeRemove` hook event) releases the lease row. The checkout itself is the harness's to delete — writ never removes it, and expiring a claim never erases WIP.
7. The installed companion `babysit-pr` skill handles interactive monitoring after a PR handoff.
8. A timeout or hang on `writ supervisor` is a recovery and handoff event: contain the child, record residual state, and leave the harness-owned checkout in place. Policy: [`docs/timeout-policy.md`](docs/timeout-policy.md). Do not improvise a second timeout path in the CLI.
