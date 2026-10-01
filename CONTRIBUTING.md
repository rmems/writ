# Contributing

[`AGENTS.md`](AGENTS.md) is the contribution and safety contract. This file is
the short filing guide: one task record, one pull request, and one label
vocabulary.

Maintainers track the task in Linear and open a GitHub pull request. They do
not file a GitHub issue twin, child issue, or duplicate checklist. Contributors
use a GitHub issue or pull request and do not need Linear, Beads, or any other
maintainer service. Existing GitHub issues stay as history.

Beads and the host's own task list are optional. A checkpoint or draft pull
request is not merge or release readiness. State which checks ran. Required
GitHub checks still govern readiness when you claim it. Details:
[Contribution](AGENTS.md#contribution).

## Issue templates

Optional forms live in [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/)
(GitHub forms; blank issues stay enabled). Use one when filing a GitHub issue:

| Template | Use when |
| --- | --- |
| **Feature** | New skill, CLI, or enforcement capability |
| **Bug** | Unexpected skill, CLI, CI, or process failure |
| **Chore** | Hygiene, packaging, or docs-only work |

Each template asks for **Summary**, **Problem / context** (or a repro),
**Acceptance criteria** (checkboxes), and a **canonical label**. A Linear
footer is optional for maintainers who already have an `RM-*` id. Contributors
leave it blank. Do not add extra required fields, and do not invent a Linear
issue or a GitHub twin to fill the footer.

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

Follow [checkpoint push](docs/workflows/safe-issue-verified-commit.md) then
[pull request](docs/workflows/safe-verified-commit-to-pr.md). Never merge from
those workflows. Review checklist: [`REVIEW.md`](REVIEW.md).

Focused checks while working. This workspace suite is for merge or release
readiness, not for every checkpoint:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
