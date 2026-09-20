# writ

A Rust safety core for coding-agent fleets: isolated git worktrees, `git`/`gh` mutation allowlists, path sandboxing, process containment, and **no GitHub PR merge path**. Local feature-branch integration is allowlisted.

Workers run in one worktree each. The portable [`SKILL.md`](SKILL.md) tells an agent host how to spawn them. The installed companion `babysit-pr` skill can watch a pull request until it is merge-ready. **A human still merges the pull request on GitHub** — `writ` exposes no `gh pr merge` path, auto-merge, or merge queue.

> [!IMPORTANT]
> **This repository is mid-pivot.** It is becoming **`writ`** — the enforcement and admission-control layer for agent fleets. See [#1](https://github.com/rmems/writ/issues/1) for the product epic and [#124](https://github.com/rmems/writ/issues/124) for the current phase. The crate rename and the GitHub repository rename have both landed under milestone M2.

## Status

> [!NOTE]
> **The enforcement core and the Phase 1 hook dispatcher are real; live burn-in is outstanding.**
> Shipping today are the `git`/`gh` allowlists, path sandboxing, process supervision, checkout registration (`writ worktree register`), local feature-branch merge admission, and a hard block on GitHub PR merges — reachable through the `writ` CLI. The managed lifecycle (`worktree create`/`remove`/`prune`) is deprecated: the agent harness or plain `git` owns checkout creation and physical cleanup.
> Enforcement still applies only to commands routed through `writ` deliberately — it is **opt-in, not unbypassable** — until the remaining **M1** burn-in lands ([#124](https://github.com/rmems/writ/issues/124)).


| Area | Today | Later |
| --- | --- | --- |
| Repo, license, [`SKILL.md`](SKILL.md), this README | Done | Living docs; update as milestones close |
| `writ` CLI (`git-safe`, `gh-safe`, `worktree`, `supervisor`, `status`) | Implemented | Envelope-compatible additions only |
| Claude Code hook dispatcher / `writ install` | Implemented | Live burn-in outstanding ([#124](https://github.com/rmems/writ/issues/124), install narrative [#18](https://github.com/rmems/writ/issues/18)) |
| SQLite lease store, path scopes, budgets | Skeleton (grant/release + registration) | M1 then M3/M4 ([#1](https://github.com/rmems/writ/issues/1), [#167](https://github.com/rmems/writ/issues/167)) |
| Owner-allowlist enforcement | Enforced in `writ-core` | [#146](https://github.com/rmems/writ/issues/146) |
| Hive verbs `discover` / `add` / `check` / `check-all` | **Removed** | Do not expect them; see [Commands](#commands) |

Canonical command names live in [`SKILL.md`](SKILL.md) and `writ --help`. This README summarizes them.

## What this is for

Coordination is commoditized. Claude Code agent teams, `/batch`, Cursor `/multitask`, and Codex all already decompose work and hand each agent an isolated worktree. What none of them enforce is **safe concurrent writing**.

Measured on 33,596 agent pull requests across 2,807 repositories ([arXiv:2607.04697](https://arxiv.org/abs/2607.04697)):

- **41.7%** cross-agent textual conflict rate, versus 19.8% intra-agent (non-overlapping confidence intervals)
- **79.4%** of agent PRs were open concurrently with another agent's
- **84.4%** of conflicts were in source code, and largely *structural* — agents disagreeing about whether a file should exist at all

Claude Code agent teams have real coordination and [documented zero isolation](https://code.claude.com/docs/en/agent-teams): "two teammates editing the same file leads to overwrites." `/batch` and Cursor have real isolation and no coordination. The two never co-occur, and nothing in either column enforces safe integration.

`writ` fills that gap. It does not assign work. It **admits writes**.

> [!NOTE]
> **Status: Phase 1 hook dispatcher is in this tree.** `writ hook` admits `PreToolUse` (Bash `git`/`gh` only) and answers `SubagentStart`/`SubagentStop` plus `WorktreeCreate`/`WorktreeRemove` as coordination-only events — it never creates or deletes a checkout. `writ install` writes the hook block (PreToolUse + subagent registry only). The SQLite lease store is a skeleton: grant/release plus reserved budget columns, not MCP or budget enforcement. Live Claude Code end-to-end burn-in is still outstanding on [#124](https://github.com/rmems/writ/issues/124).

## Install

Two pieces: the **binary** (enforcement) and the **skill** (portable agent procedure). Install both. Neither is a substitute for the other.

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

4. Inspect watched jobs (usually empty today — nothing in this repo writes the state file):

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
                     Durable state / watchlist
                     watched.json is read-only today
                     SQLite leases planned (M1 / #124)
```

Call-outs:

- **Isolation:** workers must not share a dirty worktree. One writable worker per assigned worktree and branch.
- **Babysit:** success means merge-ready (CI green, mergeable, reviews clean, threads resolved). The human still merges.
- **Stacks:** handle from the bottom of the stack upward. Do not run parallel writers on one stack.
- **State location:** default watched-state path is platform user-data (`writ/watched.json`), overridable with `WRIT_STATE_PATH` (legacy `WH_STATE_PATH` if unset). Do not treat that file as a writer API; this repo only *reads* it. The planned store is SQLite leases, not a new JSON path.

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

| **Watchlist / hive** | Durable list of jobs. Today: optional `watched.json` *read* by `writ status` / `writ jobs`. Writer and SQLite leases are not in this tree (M1). |
| **Merge-ready** | CI green, conflict-free, required checks successful, review threads resolved. Report it; do not merge. |
| **Lease** | Planned M1 record that admits a writer to a path scope. Not implemented. |

## Commands

Implemented `writ` surface (`writ --help` is authoritative):

| Command | Status | Operator meaning |
| --- | --- | --- |
| `writ status` / `writ jobs` | Implemented (read-only) | Show watched jobs. Empty unless an external process wrote the state file. See [`docs/status-schema.md`](docs/status-schema.md). |
| `writ git-safe …` | Implemented | Run a git command after the allowlist and, for mutations, expected-branch checks. |
| `writ gh-safe …` | Implemented | Run a `gh` command after the allowlist. GitHub PR merge operations are rejected. |
| `writ supervisor run --timeout <secs> …` | Implemented | Spawn a child with wall-clock timeout. Unix kills the process group; Windows kills only the direct child (grandchildren may survive). |
| `writ worktree register\|unregister\|inspect\|list` | Implemented | Coordination records for harness-owned checkouts. Register/unregister never touch files or branches. |
| `writ worktree create\|remove\|prune` | Deprecated | Managed lifecycle kept for caller compatibility during the transition. `create` requires `--schema-version 2` and `--start-point`. |
| `writ --json` | Implemented | Version-1 JSON envelopes on stdout; diagnostics on stderr. Fixtures: [`docs/examples/`](docs/examples/). |
| `writ install` / hook dispatcher | **Planned (M1)** | Register unbypassable hooks. Not a command today. |

JSON envelopes look like:

```json
{"ok":true,"schema_version":1,"command":"cli.bootstrap","data":{},"error":null}
```

Policy rejections print to stderr and exit **2** (they do not wrap a JSON envelope):

```console
$ writ --json git-safe push --force
writ: policy violation [BARE_FORCE_PUSH]: bare --force/-f is not allowed; use --force-with-lease only

$ writ --json gh-safe pr merge 1
writ: policy violation [MERGE_BLOCKED]: `gh pr merge` is not allowed
```

### Removed hive verbs

Milestone A skill stubs used these names. They are **not** commands in `writ` or [`SKILL.md`](SKILL.md). Do not invoke them.

| Old verb | Was supposed to mean | Use instead |
| --- | --- | --- |
| `discover` | Show candidate issues/PRs under allowlisted owners | Host GitHub/Linear tools; operator-configured owners only ([#146](https://github.com/rmems/writ/issues/146)) |
| `add` | Enqueue issues/PRs into the hive | Harness creates the checkout; `writ worktree register` joins it to coordination |
| `check` | One maintenance/fix cycle in the current context | Worker flow in [`SKILL.md`](SKILL.md) + `writ git-safe` / `writ gh-safe` |
| `check-all` | Orchestrated cycles across the hive | Host orchestrator; no hive runtime in this repo |
| `list` | Show hive items / workers | `writ status` / `writ jobs` / `writ worktree list` |

## Safety invariants

These apply to every agent, platform, and command path. [`SKILL.md`](SKILL.md) restates them as procedure; [`AGENTS.md`](AGENTS.md) is the contract.

- **Never merge a GitHub pull request through `writ`.** Local feature-branch `git merge` is allowlisted; `gh pr merge`, auto-merge, and merge queues are not. GitHub repository protection owns remote PR merges ([`AGENTS.md`](AGENTS.md#remote-github-merges)).
- Auto-merge, merge queues, scheduled merges, and admin bypasses are always forbidden.
- Force pushes may use only `--force-with-lease`; bare `--force` and `-f` are forbidden.
- Each job edits only its assigned branch and isolated worktree.
- Mutating operations verify the expected branch and stay inside the configured path sandbox.
- Stacked pull requests are handled from the bottom of the stack upward.
- **Fix cap / budget:** unbounded fix loops are a named failure class (F2). There is **no runtime cap today**. The old prompt-only “3 code-fix commits per babysit cycle” is not enforced. Lease budgets are planned in [#167](https://github.com/rmems/writ/issues/167).

Soft prompt text is not runtime enforcement. Hard stops live in Rust, at the binary boundary, where a malformed prompt cannot bypass them.

## Owner allowlist

Repository access is controlled by a configured owner allowlist, not a built-in org list.

- Set `WRIT_ALLOWED_OWNERS=acme,example-org` (comma-separated), or pass `--allowed-owners` / explicit owners at the API boundary.
- An empty allowlist denies owner-taking operations (`writ worktree create` and `gh` commands that select a repository via `-R` / `--repo` in any pflag spelling, or via `GH_REPO`) rather than permitting them.
- Comparison uses the same host/case normalization as `github_repo_slugs_match`, so `Acme/Repo` and `github.com/acme/repo` cannot diverge.

Examples use generic owners such as `acme` and `example-org`.

Linear may mirror planning for an operator's own team; that team id is operator-local, not a product default.

## Related skills

| Skill | Role |
| --- | --- |
| **`writ`** ([`SKILL.md`](SKILL.md)) | Fleet procedure: isolate worktrees, spawn workers, local integration, issue → PR. Does not merge GitHub pull requests. |
| **`babysit-pr`** (installed companion) | Single-PR interactive monitoring in the current checkout: CI, reviews, threads, merge-ready report. Not a hive orchestrator and not a merge button. |

`writ` admits writes across jobs, including local feature-branch integration. `babysit-pr` watches one PR. Neither merges a GitHub pull request.

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
| Watched state | platform user-data `writ/watched.json` | `WRIT_STATE_PATH`, else `WH_STATE_PATH` |
| Rust binary | `writ` on `PATH` | `WRIT_BIN` |

If the new `writ` data root is absent and a pre-rename `worktrees-hives` root still exists, the path resolver keeps using the legacy root so an upgrade does not hide existing state. That is a read/fallback, not an automatic directory move.

## Troubleshooting

| Symptom | What to check |
| --- | --- |
| Agent does not see the `writ` skill | `ls "$HOME/.agents/skills/writ/SKILL.md"`; confirm the host's actual skill root; recreate the symlink. |
| `writ: command not found` | `cargo install --path crates/writ` from the clone, or set `WRIT_BIN`. |
| `writ status` / `writ jobs` is always empty | Expected. Nothing in this workspace writes `watched.json`. See [`docs/status-schema.md`](docs/status-schema.md). |
| `policy violation [BARE_FORCE_PUSH]` or `[MERGE_BLOCKED]` | Exit 2 is the safety boundary working. Use `--force-with-lease` only when allowed. `MERGE_BLOCKED` covers `gh pr merge`, `git mergetool`, default-branch local merge, and dirty-WIP merge — not routine feature-branch integration. |
| Owner allowlist did not block another org | Confirm `WRIT_ALLOWED_OWNERS` / `--allowed-owners` is set. Empty lists deny. The gate covers worktree create and `gh` repo selectors, not host MCP calls. |
| `writ install` is missing | Planned M1. Do not invent a second installer. Track [#18](https://github.com/rmems/writ/issues/18) and [#124](https://github.com/rmems/writ/issues/124). |

## Issue labels and templates

Canonical product labels (use these; do not invent new names unless the epic
expands the taxonomy). Prefer **`docs`** over GitHub’s default `documentation`.
Keep GitHub label descriptions identical to this table (commands in
[`CONTRIBUTING.md`](CONTRIBUTING.md)):

| Label | Description |
| --- | --- |
| `epic` | Multi-issue umbrella / milestone grouping |
| `core` | Skill loop primitives (discover, claim, PR, babysit, state) |
| `orchestrator` | Multi-subagent scheduling, caps, join/report |
| `docs` | README, SKILL.md, templates, operator docs |
| `platform` | Install paths, multi-agent-host packaging, validation on second host |
| `safety` | Never-merge, force-with-lease, fix caps, owner allowlist, hang recovery |

Optional GitHub issue forms live in [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/):
**Feature** (new capability), **Bug** (unexpected failure), **Chore** (hygiene,
packaging, docs-only). Each asks for Summary, Problem / context, Acceptance
criteria checkboxes, and the Linear footer documented in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Roadmap and issues

- Product epic: [#1](https://github.com/rmems/writ/issues/1)
- Current phase (hook enforcement): [#124](https://github.com/rmems/writ/issues/124)
- Hook install narrative: [#18](https://github.com/rmems/writ/issues/18)
- Owner-allowlist enforcement: [#146](https://github.com/rmems/writ/issues/146)
- Lease budgets (fix-loop bound): [#167](https://github.com/rmems/writ/issues/167)
- Threat model: [#22](https://github.com/rmems/writ/issues/22) · Boundary tests: [#81](https://github.com/rmems/writ/issues/81)
- Portable workflows: issue → commit [#84](https://github.com/rmems/writ/issues/84) / isolation [#6](https://github.com/rmems/writ/issues/6); commit → PR [#8](https://github.com/rmems/writ/issues/8)
- Planning mirror: [Linear `worktrees-hives` project](https://linear.app/rpd-34/project/worktrees-hives-e3052de4caa3) (this issue: [RM-118](https://linear.app/rpd-34/issue/RM-118/expand-readme-install-commands-architecture))

Milestone groups from the epic: **M1** hook enforcement + minimal lease store; **M2** rename (landed); **M3** cross-repo lease/state + MCP; **M4** path-scoped admission.

## Project documentation

- [`AGENTS.md`](AGENTS.md) — the authoritative contribution, autonomy, and safety contract
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — issue templates and canonical labels
- [`SKILL.md`](SKILL.md) — portable agent procedure (guidance, not a security boundary)
- [`docs/install.md`](docs/install.md) — clone + symlink skill install, cross-agent roots, uninstall
- [`REVIEW.md`](REVIEW.md) — pull-request lifecycle and review checklist
- [`docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md`](docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md) — v1 Rust-only runtime and Codex `babysit-pr` boundary
- [`docs/workflows/safe-issue-verified-commit.md`](docs/workflows/safe-issue-verified-commit.md) — issue → verified push
- [`docs/workflows/safe-verified-commit-to-pr.md`](docs/workflows/safe-verified-commit-to-pr.md) — verified push → PR handoff (does not merge the pull request)
- [`docs/status-schema.md`](docs/status-schema.md) — `status` / `jobs` JSON
- [`docs/examples/`](docs/examples/) — captured response envelopes
- [`docs/hook-boundary.md`](docs/hook-boundary.md) — hook-boundary contract tests and two-hook burn-in (#81)

## License

Licensed under the [Apache License 2.0](LICENSE).
