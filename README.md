# writ

A **Rust-first, provider-neutral coordination layer for parallel coding agents**.

Cursor, Claude Code, Codex, and similar harnesses already spawn workers and give each one an isolated checkout. What they don't share is coordination: who owns which task, branch, and worktree; what paths each worker intends to touch; who is blocked on whom; and how to hand work off without losing WIP. `writ` supplies that shared coordination layer so parallel workers can see each other, avoid duplicate work, negotiate overlapping edits early, and recover cleanly from pauses, crashes, and timeouts.

## What writ does

- **Identity and ownership.** Registers tasks, agents/sessions, repositories, checkouts, branches, and heads in a shared SQLite store, and tracks which worker owns what via leases.
- **Visibility.** Declared paths and live status let workers see overlap before it becomes a merge conflict — overlap on separate branches is advisory and negotiated, not a fleet-wide freeze.
- **Communication.** Intent, dependency, blocker, help-request, and handoff messages so workers coordinate directly instead of through a human relay.
- **Recovery.** Pauses, timeouts, and stale leases are handoff events that preserve WIP and branch history — never permission to seize a live worktree or erase changes.
- **Local integration.** `git merge`, `rebase`, and `cherry-pick` of peer work into an assigned feature branch are normal collaboration tools.

## What writ does not do

- **Does not own worker lifecycles.** Harnesses (Cursor, Claude Code, Codex, plain `git worktree add`) create and isolate checkouts. `writ worktree register` records them; it never creates, moves, fetches, resets, or deletes a checkout.
- **Does not assign work.** Task decomposition and scheduling belong to the manager agent and the tracker.
- **Does not replace GitHub or Linear.** Linear is the task tracker for teams that use it. GitHub is the source, PR, review, checks, and protected-branch merge authority.
- **Does not silently allow duplicate ownership.** Two live owners of the same task is a detected collision, not a race.
- **Local only.** SQLite coordination is same-host. Other transports are out of scope until implemented and tested.

## How it fits with Cursor / Claude / Codex

```text
        Cursor / Claude Code / Codex / other harness
        owns worker spawn + checkout/worktree lifecycle
                          │
                          │ register existing checkouts
                          ▼
        ┌──────────────────────────────────────────┐
        │ writ (Rust binary → writ-core)           │
        │ SQLite coordination store, same host     │
        │                                          │
        │  tasks/agents · checkouts/branches/heads │
        │  ownership leases · declared paths       │
        │  intent/dependency/blocker/help/handoff  │
        │  pause/timeout/recovery · status         │
        └──────────────────────────────────────────┘
                          │
                          ▼
                  git / gh / OS primitives
                          │
                          ▼
              GitHub (source, PRs, reviews, checks)
              Linear (task tracking)
```

Workers keep their harness-native isolation. `writ` is the shared memory and message bus between them — the part none of the harnesses provide.

## One end-to-end example

1. The harness creates a checkout for a Linear task and registers it:

   ```bash
   git -C /path/to/repo worktree add /path/to/job-42 -b task/RM-42-example origin/main
   writ --json worktree register /path/to/job-42 --job RM-42
   ```

   Registration is coordination-state only — any path works, including a standalone clone; a branch ahead of its base registers as-is.

2. The worker declares its intended paths and picks up a lease. A second worker whose paths overlap sees the collision, identifies the owner, and negotiates a split or a handoff instead of silently diverging.

3. Work proceeds in the assigned checkout. Compatible peer work can be integrated locally (`git merge` / `cherry-pick` into the assigned branch). Commands routed through the safety boundary are checked first:

   ```bash
   writ git-safe --expected-branch task/RM-42-example --repo <worktree> status
   writ gh-safe -R acme/example-org pr view 1
   ```

4. On a pause, crash, or timeout, the lease plus declared state let another worker resume or take over without losing WIP.

5. The worker opens a PR; GitHub checks and reviews run; Linear tracks the task. Shared status stays truthful for managers and workers throughout.

## Status

| Area | Today | Next |
| --- | --- | --- |
| `writ` CLI (`git-safe`, `gh-safe`, `worktree`, `supervisor`, `status`, `attribution`) | Implemented | Envelope-compatible additions only |
| Checkout registration (`worktree register/unregister/inspect/list`) | Implemented | Managed lifecycle (`create`/`remove`/`prune`) is deprecated |
| Claude Code hook dispatcher / `writ install` | Implemented | Live burn-in outstanding ([#124](https://github.com/rmems/writ/issues/124)) |
| SQLite lease store, agent registry | Skeleton (grant/release + registration) | Crash-consistency + same-host claims/messages/handoff (in flight) |
| Declared paths / overlap visibility, handoff protocol | Planned | Reuses the lease store — no second database |
| Path-scoped admission, lease budgets | Planned | [#167](https://github.com/rmems/writ/issues/167) |
| Owner-allowlist enforcement | Enforced in `writ-core` | [#146](https://github.com/rmems/writ/issues/146) |

Canonical command names live in [`SKILL.md`](SKILL.md) and `writ --help`. This README summarizes them.

## Install

Two pieces: the **binary** (coordination + safety core) and the **skill** (portable agent procedure). Install both. Neither is a substitute for the other.

### Prerequisites

- [Git](https://git-scm.com/)
- [GitHub CLI](https://cli.github.com/) authenticated (`gh auth status`)
- Stable Rust from [rustup](https://rustup.rs/). The workspace MSRV is Rust **1.97.1** (pinned in `rust-toolchain.toml`)
- An agent host that loads skills from `~/.agents/skills` (or another skill root — see [Other agent hosts](#other-agent-hosts))

### Binary

```bash
git clone https://github.com/rmems/writ.git "$HOME/src/writ"
cd "$HOME/src/writ"
cargo build --workspace
cargo install --path crates/writ
writ --help
```

Override the binary later with `WRIT_BIN` if it is not on `PATH`.

### Skill (symlink, preferred)

Agents load [`SKILL.md`](SKILL.md) from a skill root. Symlink the clone so edits stay in one tree:

```bash
mkdir -p "$HOME/.agents/skills"
ln -s "$HOME/src/writ" "$HOME/.agents/skills/writ"
```

Copy is the fallback when a symlink is awkward (some Windows setups):

```bash
mkdir -p "$HOME/.agents/skills"
cp -R "$HOME/src/writ" "$HOME/.agents/skills/writ"
```

A copied tree will drift from upstream; prefer the symlink when you can. Publishing via a skills CLI is not required for local use; packaging and hook-install narrative are [#18](https://github.com/rmems/writ/issues/18).

Older docs used `~/.agents/skills/worktrees-hives`. Use `writ` as the skill directory name now.

### Verify

```bash
ls "$HOME/.agents/skills/writ/SKILL.md"
writ --help
```

Then confirm the agent host lists a `writ` skill (the exact UI depends on the host: skill list, `/skills`, or equivalent). If the file is present but the host does not show the skill, the host is not reading `~/.agents/skills`.

### Other agent hosts

Skill directories are not standardized. Common roots include `~/.agents/skills`, `~/.claude/skills`, and a project-local skills folder. Cursor, Cline, Codex, and Claude Code each resolve skills differently. This README documents the `~/.agents/skills/writ` primary path only. Point a copy or symlink of this repository at whichever root your host actually scans. A full install matrix is out of scope here ([#18](https://github.com/rmems/writ/issues/18)).


## Quick start

Until M1 hooks land, git and GitHub commands that `writ` can admit must go through `writ` on purpose. Local `git merge` on an assigned feature branch is allowlisted; `gh pr merge`, auto-merge, and merge queues are not. GitHub owns remote PR merges ([`AGENTS.md`](AGENTS.md#remote-github-merges)).

1. Install the binary and skill as above.
2. The harness creates the isolated checkout (native worktree support, or plain git):

   ```bash
   git -C /path/to/repo fetch origin
   git -C /path/to/repo worktree add /path/to/job-42 -b hive/issue-42-example origin/main
   ```

   Then register it for coordination — any path works, including a standalone clone:

   ```bash
   writ --json worktree register /path/to/job-42 --job job-42
   ```

   Registration is coordination-state only: no creation, relocation, rename, fetch, or reset. A branch ahead of its base registers as-is.

3. Route git and GitHub mutations through the allowlists:

   ```bash
   writ git-safe --expected-branch hive/issue-42-example --repo <worktree> status
   writ gh-safe -R acme/example-org pr view 1
   ```

4. Inspect collaboration status from the shared lease store:

   ```bash
   writ --json status
   ```

Do not expect a `discover` → `add` → `check-all` → `list` hive loop. Those were scaffold-era skill stubs and were removed with the Python orchestrator. Current operator flow is: the harness isolates a worktree, `writ worktree register` joins it to the coordination store, the worker integrates compatible peer work locally when needed, implements in that tree, opens a PR, optionally hands it to `babysit-pr`, and leaves the GitHub PR merge to a human.


## Architecture

Two layers, one binary.

| Layer | Owns | Does not own |
| --- | --- | --- |
| **Enforcement** (per-repo) | Exact base, branch/path identity, path sandbox, git/gh allowlists, local feature-branch merge, no GitHub PR merge path, force-with-lease only, process containment | Which agent does what |
| **Coordination state** (cross-repo) *(planned, M1)* | Agents, leases with path scopes, ownership, blockers, freeze modes. SQLite, single file, derived from `git`/`gh`/disk. Not implemented yet | Task decomposition or scheduling |
| `git`, `gh`, OS | Version-control, GitHub, and process primitives, invoked through allowlists | Policy |

Leases are the join: coordination state that the enforcement layer checks at write time. Phase 1 ships a SQLite skeleton (`leases.db`) so create/remove is not a Markdown-only control plane. Budget columns are reserved and unused; enforcement is [#167](https://github.com/rmems/writ/issues/167).

```text
                    ┌─────────────────────────┐
                    │  Agent host             │
                    │  loads SKILL.md         │
                    └───────────┬─────────────┘
                                │
                    ┌───────────▼─────────────┐
                    │ Orchestrator session    │
                    │ (host agent + writ CLI) │
                    └───────────┬─────────────┘
               ┌────────────────┼────────────────┐
               ▼                ▼                ▼
          Worktree A       Worktree B       Worktree C
          Worker subagent  Worker subagent  Worker subagent
          issue → PR       babysit          …
               │                │                │
               └────────────────┼────────────────┘
                                ▼
                     Durable coordination state
                     SQLite leases.db (status/list/register)
```

Call-outs:

- **Isolation:** workers must not share a dirty worktree. One writable worker per assigned worktree and branch.
- **Babysit:** success means merge-ready (CI green, mergeable, reviews clean, threads resolved). The human still merges.
- **Stacks:** handle from the bottom of the stack upward. Do not run parallel writers on one stack.
- **State location:** collaboration status reads the SQLite lease store (`WRIT_LEASE_PATH`, else platform user-data `writ/leases.db`). `watched.json` (`WRIT_STATE_PATH`) is a leftover reader only and is not the status authority.

### Why hooks

Enforcement runs as [Claude Code hooks](https://code.claude.com/docs/en/hooks): `writ install` writes the hook block into `.claude/settings.json`, and `writ hook` dispatches the JSON payloads. This repo's own `.claude/settings.json` still registers only `SessionStart` and `PreCompact` — hooks are opt-in until the [#124](https://github.com/rmems/writ/issues/124) burn-in completes.

- **`PreToolUse`** — "Exit 2 means a blocking error… exit 2 blocks whether or not you print JSON: even a JSON `permissionDecision` of `allow` can't override it."
- **`SubagentStart`/`SubagentStop`** — agent registry.

`WorktreeCreate`/`WorktreeRemove` are deliberately **not** installed: worktree lifecycle is harness-owned. If an older settings file still routes them to `writ hook`, the dispatcher treats them as coordination-only (register/release records, never filesystem mutation).

Register with `writ install`. Matcher scope starts at `Bash(git *)` and `Bash(gh *)` only.

### Glossary

| Term | Meaning |
| --- | --- |
| **Orchestrator** | The host agent session that loads [`SKILL.md`](SKILL.md), calls `writ`, and spawns workers. It is not a `writ` subcommand. |
| **Worker** | A subagent bound to one assigned worktree and branch. Workers may integrate peer work locally; they never merge a GitHub pull request. |
| **Worktree** | An isolated git checkout created by the harness (or plain `git worktree add`), registered with `writ worktree register`. Any path; no writ-specific root required. |

| **Watchlist / hive** | Legacy name. Live visibility is `writ status` / `writ worktree list` over the SQLite lease store. |
| **Merge-ready** | CI green, conflict-free, required checks successful, review threads resolved. Report it; do not merge. |
| **Lease** | SQLite coordination row for a registered checkout (`WRITER_LOCKED` / `UNASSIGNED` today). |

## Commands

Implemented `writ` surface (`writ --help` is authoritative):

| Command | Status | Operator meaning |
| --- | --- | --- |
| `writ status` / `writ jobs` | Implemented (read-only) | Collaboration snapshot from the SQLite lease store. See [`docs/status-schema.md`](docs/status-schema.md). |
| `writ git-safe …` | Implemented | Run a git command after the allowlist and, for mutations, expected-branch checks. |
| `writ gh-safe …` | Implemented | Run a `gh` command after the allowlist. GitHub PR merge operations are rejected. |
| `writ supervisor run --timeout <secs> …` | Implemented | Spawn a child with wall-clock, idle, and grace recovery (`--idle`/`--stall`, `--grace`, `--progress-secs`, env `WRIT_SUPERVISOR_*`; see [`docs/timeout-policy.md`](docs/timeout-policy.md)). Timeout is a handoff residual: Unix SIGTERM-then-SIGKILL on the process group; Windows kills only the direct child (grandchildren may survive). Never deletes a harness checkout. |
| `writ watchlist list\|check\|check-all` | Implemented (view) | Collaboration status over `leases.db` plus optional live GitHub overlay. Does not persist a watchlist file. |
| `writ worktree register\|unregister\|inspect\|list` | Implemented | Coordination records for harness-owned checkouts. Register/unregister never touch files or branches. |
| `writ worktree create\|remove\|prune` | Deprecated | Managed lifecycle kept for caller compatibility during the transition. `create` requires `--schema-version 2` and `--start-point`. |
| `writ attribution format` | Implemented | Render review and collaboration replies with real agent/task/branch/session identity. Include a SHA only when one exists; never invent one. |
| `writ --json` | Implemented | Envelope-producing commands emit versioned JSON on stdout (generic v1; status/jobs v2); diagnostics on stderr. Exceptions: `hook --json` has no envelope; `git-safe`/`gh-safe` validation failures and `install` failures emit stderr only. See the [`CLI contract`](docs/cli-contract.md) and [`fixtures`](docs/examples/). |
| `writ install` / hook dispatcher | Implemented | Registers `writ hook` into `.claude/settings.json`; burn-in outstanding ([#124](https://github.com/rmems/writ/issues/124)). |

JSON envelopes look like:

```json
{"ok":true,"schema_version":1,"command":"cli.bootstrap","data":{},"error":null}
```

Policy rejections print to stderr and exit **2** (they do not wrap a JSON envelope):

```console
$ writ --json git-safe push --force
writ: policy violation [BARE_FORCE_PUSH]: bare --force/-f is not allowed; use --force-with-lease only
```

## Coordination model

### Identity

A lease row joins a task, an agent/session, and a checkout path — the "who owns what" record. `writ worktree register` creates it for an existing checkout; `unregister` releases it while keeping identity so a verified reclaim can prove ownership. TTL expiry marks a lease stale; it never erases the checkout's WIP.

### Overlap and handoff

Declared intended paths give early overlap visibility. Overlap on separate branches is advisory: the colliding worker identifies the owner and negotiates a split, a sequence, or a handoff. Duplicate live ownership of one task is a detected collision, not silent divergence. A stale lease is a recovery/handoff event — not permission to kill a worker or discard its changes.

### Communication

The shared store carries intent, dependency-ready, blocker, overlap, help-request, handoff, and completion records, with enough identity/version information to distinguish stale messages from live state. A manager can split scope or pick one integration owner without turning every message into a human approval gate.

### Local integration

`git merge`, `git rebase`, and `git cherry-pick` of peer branches into the assigned branch are routine; conflict repair is expected. Resolve the contended files, validate the combined result, and publish the resulting head/dependencies for other workers. A merge that would overwrite uncommitted WIP must be refused first — commit, stash, or abort. Default-branch integration belongs to GitHub policy.

### GitHub vs Linear

| Tool | Role |
| --- | --- |
| **Linear** | Optional task tracker: task identity, status, planning, when the operator uses it. |
| **GitHub** | Source of truth for code, PRs, reviews, checks, and protected-branch merges. |
| **writ** | Same-host coordination state only. No task-tracker clone, no merge authority. |

GitHub issue twins or Beads mirrors are not a required workflow.

## Safety invariants

Coordination integrity and WIP protection are enforced in Rust at the binary boundary, where a malformed prompt cannot bypass them:

- Force pushes may use only `--force-with-lease`; bare `--force` and `-f` are forbidden.
- Each job edits only its assigned branch and isolated checkout; mutating operations verify the expected branch and stay inside the configured path sandbox.
- One writable worker per assigned worktree and branch; stacked PRs are handled bottom-up.
- Unbounded fix loops are a named failure class (F2); lease budgets are planned in [#167](https://github.com/rmems/writ/issues/167).

`writ` enforces these rules only for commands routed through it deliberately — it is **opt-in, not unbypassable** — until the remaining hook burn-in lands ([#124](https://github.com/rmems/writ/issues/124)).

## Owner allowlist

Repository access is controlled by a configured owner allowlist, not a built-in org list.

- Set `WRIT_ALLOWED_OWNERS=acme,example-org` (comma-separated), or pass `--allowed-owners` / explicit owners at the API boundary.
- An empty allowlist denies owner-taking operations (`writ worktree create` and `gh` commands that select a repository via `-R` / `--repo` in any pflag spelling, or via `GH_REPO`) rather than permitting them.
- Comparison uses the same host/case normalization as `github_repo_slugs_match`, so `Acme/Repo` and `github.com/acme/repo` cannot diverge.

Examples use generic owners such as `acme` and `example-org`.

## Related skills

| Skill | Role |
| --- | --- |
| **`writ`** ([`SKILL.md`](SKILL.md)) | Fleet procedure: register checkouts, coordinate workers, local integration, issue → PR. |
| **`babysit-pr`** (installed companion) | Single-PR interactive monitoring in the current checkout: CI, reviews, threads, merge-ready report. |

## Build and gates

Contributor quality gates — these are canonical, and external analyzers are advisory until reproduced:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Install the agent skill

`SKILL.md` is the portable procedure (not a security boundary). Clone once, then symlink that directory into a skill root so `<root>/writ/SKILL.md` resolves. Shared hub:

```bash
git clone https://github.com/rmems/writ.git "$HOME/src/writ"
"$HOME/src/writ/scripts/install-skill.sh"
test -f "$HOME/.agents/skills/writ/SKILL.md" && echo OK
```

`./scripts/install-skill.sh --root all` also links Grok, Cline, Claude Code, and Cursor roots. The installer is idempotent, creates parent directories, and refuses to clobber a non-symlink without `--force`. Restart the agent (or start a new session) and list skills, or run `/skills writ`.

Full root table, uninstall, verification, and WSL notes: [`docs/install.md`](docs/install.md).

This is not `writ install`, which writes the `writ` hook block into `.claude/settings.json` (implemented; do not run it against a shared settings file until the [#124](https://github.com/rmems/writ/issues/124) burn-in completes).

## Runtime paths

| Purpose | Default | Override |
| --- | --- | --- |
| Worktree root | platform user-data `writ/worktrees` | `WRIT_WORKTREE_BASE`, else `WH_WORKTREE_BASE` |
| Job worktree | `{worktree root}/{owner}/{repo}/{job_id}` | Deprecated managed lifecycle only; `register` accepts any path |
| Lease store | platform user-data `writ/leases.db` | `WRIT_LEASE_PATH` |
| Watched state (legacy reader) | platform user-data `writ/watched.json` | `WRIT_STATE_PATH`, else `WH_STATE_PATH` |
| Rust binary | `writ` on `PATH` | `WRIT_BIN` |

If the new `writ` data root is absent and a pre-rename `worktrees-hives` root still exists, the path resolver keeps using the legacy root so an upgrade does not hide existing state. That is a read/fallback, not an automatic directory move.

## Troubleshooting

| Symptom | What to check |
| --- | --- |
| Agent does not see the `writ` skill | `ls "$HOME/.agents/skills/writ/SKILL.md"`; confirm the host's actual skill root; recreate the symlink. |
| `writ: command not found` | `cargo install --path crates/writ` from the clone, or set `WRIT_BIN`. |

| `writ status` / `writ jobs` is empty | Expected until a checkout is registered. Register with `writ worktree register`. See [`docs/status-schema.md`](docs/status-schema.md); for the live GitHub overlay use `writ watchlist` ([`docs/watchlist-schema.md`](docs/watchlist-schema.md)). |
| `policy violation [BARE_FORCE_PUSH]` or `[MERGE_BLOCKED]` | Exit 2 is the safety boundary working. Use `--force-with-lease` only when allowed. `MERGE_BLOCKED` covers `gh pr merge`, `git mergetool`, default-branch local merge, and dirty-WIP merge — not routine feature-branch integration. |
| Owner allowlist did not block another org | Confirm `WRIT_ALLOWED_OWNERS` / `--allowed-owners` is set. Empty lists deny. The gate covers worktree create and `gh` repo selectors, not host MCP calls. |

## Issue labels and templates

Canonical product labels (use these; do not invent new names unless the epic
expands the taxonomy). Prefer **`docs`** over GitHub's default `documentation`.
Keep GitHub label descriptions identical to this table (commands in
[`CONTRIBUTING.md`](CONTRIBUTING.md)):

| Label | Description |
| --- | --- |
| `epic` | Multi-issue umbrella / milestone grouping |
| `core` | Coordination primitives (registration, leases, messages, handoff, status) |
| `orchestrator` | Multi-subagent scheduling, caps, join/report |
| `docs` | README, SKILL.md, templates, operator docs |
| `platform` | Install paths, multi-agent-host packaging, validation on second host |
| `safety` | Force-with-lease, fix caps, owner allowlist, WIP preservation |

Optional GitHub issue forms live in [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/):
**Feature** (new capability), **Bug** (unexpected failure), **Chore** (hygiene,
packaging, docs-only). Each asks for Summary, Problem / context, Acceptance
criteria checkboxes, and the Linear footer documented in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Roadmap and tracking

- Product direction and task tracking for this repo: Linear (`writ` / `RM` tickets). Using Linear is a convention of this repository's maintainers, not a product requirement.
- GitHub issues mirror actionable work items; they are optional, not a required workflow.
- Open coordination work: crash-consistent lease/ownership records and the same-host claim/overlap/message/handoff layer build on the existing SQLite lease store.
- Lease budgets (fix-loop bound): [#167](https://github.com/rmems/writ/issues/167)
- Hook burn-in: [#124](https://github.com/rmems/writ/issues/124) · Install narrative: [#18](https://github.com/rmems/writ/issues/18) · Owner allowlist: [#146](https://github.com/rmems/writ/issues/146)

## Project documentation

- [`AGENTS.md`](AGENTS.md) — the authoritative contribution, autonomy, and safety contract
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — issue templates and canonical labels
- [`SKILL.md`](SKILL.md) — portable agent procedure (guidance, not a security boundary)
- [`docs/install.md`](docs/install.md) — clone + symlink skill install, cross-agent roots, uninstall
- [`docs/cli-contract.md`](docs/cli-contract.md) — command, JSON envelope, exit-code, persistence, and lifecycle compatibility matrix
- [`REVIEW.md`](REVIEW.md) — pull-request lifecycle and review checklist
- [`docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md`](docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md) — v1 Rust-only runtime and Codex `babysit-pr` boundary
- [`docs/workflows/safe-issue-verified-commit.md`](docs/workflows/safe-issue-verified-commit.md) — issue → verified push
- [`docs/workflows/safe-verified-commit-to-pr.md`](docs/workflows/safe-verified-commit-to-pr.md) — verified push → PR handoff
- [`docs/status-schema.md`](docs/status-schema.md) — `status` / `jobs` JSON
- [`docs/timeout-policy.md`](docs/timeout-policy.md) — supervisor hang recovery (hard/idle/lost-child, grace kill, redispatch budget)
- [`docs/watchlist-schema.md`](docs/watchlist-schema.md) — `watchlist` collaboration view
- [`docs/examples/`](docs/examples/) — captured response envelopes
- [`docs/hook-boundary.md`](docs/hook-boundary.md) — hook-boundary contract tests and two-hook burn-in (#81)

## License

Licensed under the [Apache License 2.0](LICENSE).
