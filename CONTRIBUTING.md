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
footer. Keep the write-up short. Do not add extra required fields.

Footer pattern (replace the placeholders when a twin exists):

```text
Linear: <url> (`RM-N`)
```

GitHub is the execution surface. Linear stays the planning twin via that
footer. Do not invent a second Linear issue when one is already linked.

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
| `safety` | Never-merge, force-with-lease, fix caps, owner allowlist, hang recovery |

Those description strings are the GitHub label descriptions. Apply the same
label you selected in the template dropdown.

Typical use: `epic` for umbrellas; `core` for loop primitives; `orchestrator`
for multi-worker scheduling; `docs` for operator docs and templates;
`platform` for install and multi-host packaging; `safety` for merge/force/
allowlist/hang-recovery boundaries.

The labels already exist on the repository. After this table changes, keep
GitHub in sync (this does not create extra labels or delete GitHub defaults):

```bash
gh label edit epic --description "Multi-issue umbrella / milestone grouping"
gh label edit core --description "Skill loop primitives (discover, claim, PR, babysit, state)"
gh label edit orchestrator --description "Multi-subagent scheduling, caps, join/report"
gh label edit docs --description "README, SKILL.md, templates, operator docs"
gh label edit platform --description "Install paths, multi-agent-host packaging, validation on second host"
gh label edit safety --description "Never-merge, force-with-lease, fix caps, owner allowlist, hang recovery"
```

## Pull requests

Follow [Safe Issue → Verified Commit](docs/workflows/safe-issue-verified-commit.md)
then [Safe Verified Commit → PR](docs/workflows/safe-verified-commit-to-pr.md).
Never merge from those workflows. Review checklist: [`REVIEW.md`](REVIEW.md).

Native gates (canonical; see README):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
