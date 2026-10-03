# ADR 0001: Rust-only v1 runtime and companion PR monitoring

- Status: Accepted architecture; operating policy revised 2026-10-03
- Original decision: 2026-09-15
- Linear: [RM-169](https://linear.app/rpd-34/issue/RM-169/v1-record-rust-only-architecture-and-codex-babysit-pr-boundary), with follow-on command/state work in [RM-170](https://linear.app/rpd-34/issue/RM-170/v1-define-unified-wh-command-and-rust-state-model)
- Contribution and authorization contract: [AGENTS.md](../../AGENTS.md)

## Context

The project began as Worktree Hive (`wh`) and is now `writ`. It needs one authoritative implementation of shared coordination state across agent platforms. A second orchestrator or state store would duplicate responsibilities and allow participants to disagree about checkout ownership, handoffs, or recovery.

Interactive PR monitoring is already supplied by installed host skills such as `babysit-pr`. Maintaining another watcher product inside this repository would duplicate that work.

## Decision

### Rust owns writ's runtime and shared state

The Rust `writ` CLI calls `writ-core` for checkout registration, lease-backed coordination, local integrity helpers, and process supervision. Those implementations remain the source of truth for writ's behavior. Clients consume the CLI and its versioned output rather than adding another coordination database or state machine.

Responsibilities stay with the layer that owns them:

| Layer | Responsibility |
| --- | --- |
| Harness | Create workers and isolated checkouts, assign writable ownership, and manage checkout lifecycle. |
| `writ` / `writ-core` | Register existing checkouts, maintain shared coordination records, and provide local integrity checks and bounded process execution. |
| GitHub | Authorize remote source, PR, review, check, and merge operations through repository permissions and rules. |
| Installed companion skill | Monitor the PR and handle in-scope CI or review feedback according to the operator's requested endpoint. |

The harness chooses checkout placement. Registration records an existing checkout; it does not seize or reset it. Using writ helpers and hooks is optional. For calls routed through them, direct Git operations retain assigned-branch and WIP checks, and supported GitHub targets retain owner checks. Supervised scripts and opaque API payloads are not recursively inspected. These helpers do not form a universal shell sandbox or a second GitHub permission system.

The current command and schema details are maintained in the [CLI contract](../cli-contract.md), [status schema](../status-schema.md), and [timeout policy](../timeout-policy.md).

### Skills are platform-facing clients

`SKILL.md`, `CLAUDE.md`, and host-specific guidance explain how to coordinate work and use the runtime. They link to [AGENTS.md](../../AGENTS.md) for the common contribution contract. They do not introduce a second state store or require contributors to install a particular private tracker.

Native Git, GitHub connectors, and other authorized host tools remain valid workflows. A future SDK may wrap the Rust interface and parse its versioned responses; it must not become a competing implementation of writ's coordination state.

### PR monitoring belongs to the installed companion

Writ does not vendor a competing `babysit-pr` skill or watcher loop. The installed companion owns polling, feedback triage, and the requested monitoring endpoint. It may use writ's primitives where useful, alongside the host's authorized native tools.

A readiness report and a merge are distinct actions. Remote merge authorization comes from the operator's request or established automation policy and GitHub permissions, as defined in [AGENTS.md](../../AGENTS.md#remote-github-merges). Admission through a writ helper is not itself authorization to merge.

## Retired operating policy

The [original decision at commit `ca446b6`](https://github.com/rmems/writ/blob/ca446b6b794dfde53d9b66e27cd55b89dcc457f6/docs/adr/0001-rust-only-v1-runtime-and-babysit-pr-boundary.md) remains available as immutable history.

Its mandatory mutation routing, managed-checkout procedures, planned universal hook boundary, and blanket remote-merge/API restrictions have been retired. Those procedures are not contribution requirements. The retained decision is one Rust runtime for writ's shared state, thin platform clients, and companion-owned interactive PR monitoring; the current operating contract is [AGENTS.md](../../AGENTS.md).

## Consequences

- Platforms share one coordination implementation without surrendering their native worker or checkout lifecycle.
- PR monitoring can improve independently in the host's installed skill.
- Contributors can use authorized native tools and checkpoints without tracker or helper startup requirements.
- Local ownership/WIP checks, process containment, and GitHub's remote authority remain distinct, with their limits documented at their respective interfaces.
