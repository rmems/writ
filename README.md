# writ

A Rust safety core for coding-agent fleets: exact-base worktree verification, `git`/`gh` mutation allowlists, path sandboxing, process containment, and **no runtime merge path at all**.

> [!IMPORTANT]
> **This repository is mid-pivot.** It is becoming **`writ`** — the enforcement and admission-control layer for agent fleets. See [#1](https://github.com/rmems/writ/issues/1) for the product epic and [#124](https://github.com/rmems/writ/issues/124) for the current phase. The crate rename and the GitHub repository rename have both landed under milestone M2.

## What this is for

Coordination is commoditized. Claude Code agent teams, `/batch`, Cursor `/multitask`, and Codex all already decompose work and hand each agent an isolated worktree. What none of them enforce is **safe concurrent writing**.

Measured on 33,596 agent pull requests across 2,807 repositories ([arXiv:2607.04697](https://arxiv.org/abs/2607.04697)):

- **41.7%** cross-agent textual conflict rate, versus 19.8% intra-agent (non-overlapping confidence intervals)
- **79.4%** of agent PRs were open concurrently with another agent's
- **84.4%** of conflicts were in source code, and largely *structural* — agents disagreeing about whether a file should exist at all

Claude Code agent teams have real coordination and [documented zero isolation](https://code.claude.com/docs/en/agent-teams): "two teammates editing the same file leads to overwrites." `/batch` and Cursor have real isolation and no coordination. The two never co-occur, and nothing in either column enforces safe integration.

`writ` fills that gap. It does not assign work. It **admits writes**.

> [!NOTE]
> **Status: the enforcement core is real; the hook layer is not built yet.**
> Shipping today are the `git`/`gh` allowlists, exact-base worktree verification, path sandboxing, process supervision, and the absence of any merge path — reachable through the `writ` CLI, including `writ worktree create`, which remains supported.
> Not yet built: the `PreToolUse`/`WorktreeCreate` hook dispatcher, `writ install`, and the SQLite lease store. Those are milestone **M1** ([#124](https://github.com/rmems/writ/issues/124)). Until they land, enforcement applies only to commands routed through `writ` deliberately — it is **opt-in, not unbypassable**.

## Architecture

Two layers, one binary.

| Layer | Owns | Does not own |
| --- | --- | --- |
| **Enforcement** (per-repo) | Exact base, branch/path identity, path sandbox, git/gh allowlists, no merge path, force-with-lease only, process containment | Which agent does what |
| **Coordination state** (cross-repo) *(planned, M1)* | Agents, leases with path scopes, ownership, blockers, freeze modes. SQLite, single file, derived from `git`/`gh`/disk. Not implemented yet | Task decomposition or scheduling |
| `git`, `gh`, OS | Version-control, GitHub, and process primitives, invoked through allowlists | Policy |

Leases are intended to be the join: coordination state that the enforcement layer will check at write time. No lease store exists yet (M1).

### Why hooks (planned — M1)

Enforcement is designed to run as [Claude Code hooks](https://code.claude.com/docs/en/hooks). None of the hooks below are registered yet: `.claude/settings.json` currently registers only `SessionStart` and `PreCompact`. This section states the target design and the contract it relies on, not current behavior.

- **`PreToolUse`** — "Exit 2 means a blocking error… exit 2 blocks whether or not you print JSON: even a JSON `permissionDecision` of `allow` can't override it."
- **`WorktreeCreate`** — "Any non-zero exit code aborts worktree creation." This is the lease-admission seam.
- **`WorktreeRemove`**, **`SubagentStart`/`SubagentStop`** — lease release and agent registry.

This inverts the usual failure mode. Safety is normally opt-in: a tool must be *called* to help — which is exactly the position `writ` is in today. Once registered as a hook, it will apply regardless of whether the agent cooperates. That gap is the point of M1, and it is the honest reason the "unbypassable" property is described here as a design goal rather than a current guarantee.

## Safety invariants

These apply to every agent, platform, and command path:

- **Never merge autonomously.** The runtime exposes no merge path. A primary interactive agent may perform one immediate merge only after a human unambiguously identifies and requests that exact PR, under the [authorization protocol](AGENTS.md#human-authorized-one-shot-merge-protocol).
- Auto-merge, merge queues, scheduled merges, and admin bypasses are always forbidden.
- Force pushes may use only `--force-with-lease`; bare `--force` and `-f` are forbidden.
- Each job edits only its assigned branch and isolated worktree.
- Mutating operations verify the expected branch and stay inside the configured path sandbox.
- Stacked pull requests are handled from the bottom of the stack upward.

Soft prompt text is not runtime enforcement. Hard stops live in Rust, at the binary boundary, where a malformed prompt cannot bypass them.

## Owner allowlist — not currently enforced

> [!WARNING]
> **`WRIT_ALLOWED_OWNERS` has no reader anywhere under `crates/`.** It was enforced in the Python layer this repository just removed, so owner scoping is presently a stated requirement with no code behind it. Do not rely on it as an access control. Tracked in [#146](https://github.com/rmems/writ/issues/146).

The intended contract, for when enforcement lands:

- Repository access is controlled by a configured owner allowlist, not a built-in org list.
- Set `WRIT_ALLOWED_OWNERS=acme,example-org` (comma-separated), or pass explicit owners at the API boundary.
- An empty allowlist denies multi-owner operations rather than permitting them.

Examples use generic owners such as `acme` and `example-org`.

## Build

Prerequisites: stable Rust from [rustup](https://rustup.rs/), Git, and the GitHub CLI. The workspace MSRV is Rust **1.97.1** (pinned in `rust-toolchain.toml`).

```bash
cargo build --workspace
cargo test --workspace
cargo install --path crates/writ
writ --help
```

Contributor quality gates — these are canonical, and external analyzers are advisory until reproduced:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Project documentation

- [`AGENTS.md`](AGENTS.md) — the authoritative contribution, autonomy, and safety contract
- [`SKILL.md`](SKILL.md) — portable agent procedure (guidance, not a security boundary)
- [`REVIEW.md`](REVIEW.md) — pull-request lifecycle and review checklist
- [`docs/workflows/safe-issue-verified-commit.md`](docs/workflows/safe-issue-verified-commit.md) — issue → verified push
- [`docs/workflows/safe-verified-commit-to-pr.md`](docs/workflows/safe-verified-commit-to-pr.md) — verified push → PR handoff (never merges)
- Product epic: [#1](https://github.com/rmems/writ/issues/1) · Current phase: [#124](https://github.com/rmems/writ/issues/124)
- Threat model: [#22](https://github.com/rmems/writ/issues/22) · Boundary contract tests: [#81](https://github.com/rmems/writ/issues/81)
- [Linear `worktrees-hives` project](https://linear.app/rpd-34/project/worktrees-hives-e3052de4caa3)

## License

Licensed under the [Apache License 2.0](LICENSE).
