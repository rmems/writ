# Contributing

[`AGENTS.md`](AGENTS.md) is the autonomy and safety contract. This file is the
short filing guide so humans and agents share one label vocabulary and one
issue shape.

## Issue templates

Pick one from [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/) (GitHub
forms; blank issues stay enabled):

| Template | Use when |
| --- | --- |
| **Feature** | New skill, CLI, or enforcement capability |
| **Bug** | Unexpected skill, CLI, CI, or process failure |
| **Chore** | Hygiene, packaging, or docs-only work |

Each template asks for **Summary**, **Problem / context** (or a repro),
**Acceptance criteria** (checkboxes), a **canonical label**, and a **Linear**
link when one already exists. Task trackers are optional; a direct request or PR can carry the scope. Keep the write-up short.

Optional link pattern (omit when no Linear task exists):

```text
Linear: <url> (`RM-N`)
```

GitHub holds source and review. Maintainers may use Linear for planning; contributors need no private tracker account. Do not create issue twins or mirrors.

## Canonical labels

Use **only** these six product labels unless the epic expands the taxonomy.
GitHub defaults (`bug`, `enhancement`, `documentation`, …) may remain on the
repo; skill and product issues still use this set. Prefer **`docs`** over
GitHub’s default `documentation`.

| Label | Description |
| --- | --- |
| `epic` | Multi-issue umbrella / milestone grouping |
| `core` | Skill loop primitives (discover, claim, PR, babysit, state) |
| `orchestrator` | Multi-subagent scheduling, caps, join/report |
| `docs` | README, SKILL.md, templates, operator docs |
| `platform` | Install paths, multi-agent-host packaging, validation on second host |
| `safety` | WIP preservation, force-with-lease, owner targeting, hang recovery |

Use an existing label matching the changed area. Label metadata helps people find work; it is not an implementation prerequisite or a reason to create a duplicate tracker record.

## Pull requests

Follow [Safe Issue → Verified Commit](docs/workflows/safe-issue-verified-commit.md)
then [Safe Verified Commit → PR](docs/workflows/safe-verified-commit-to-pr.md).
Remote merges follow GitHub permissions and explicit operator authorization. Review checklist: [`REVIEW.md`](REVIEW.md).

Use focused checks for checkpoints. Before a readiness claim, run the relevant native gates (see README):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
