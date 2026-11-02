# Collaboration safety model

This is the small safety kernel behind `writ` (GitHub [#22](https://github.com/rmems/writ/issues/22), Linear RM-135). It separates two things that older enforcement-first plans mixed together:

1. **Integrity invariants.** Conditions whose violation loses or corrupts someone's work, or leaves the shared store unable to say who owns what. Rust enforces these at the helper boundary and fails closed (exit 2 with a stable policy code such as `BARE_FORCE_PUSH`, `MERGE_BLOCKED`, `LEASE_CONFLICT`, or `COORD_CLAIM_HELD`, or a `needs_attention` state that refuses to adopt or clean up).
2. **Collaboration policy.** Signals that help parallel workers negotiate: overlap, scope drift, pause/help, handoff, budgets. These are advisory. They surface the current owner and a path forward; they never freeze unrelated progress or veto an ordinary local merge.

GitHub remains the authority for remote permissions, protected branches, reviews, and required checks. `writ` is not a second merge-approval protocol and not a shell sandbox ([AGENTS.md](../AGENTS.md#enforcement-layers)).

## Integrity invariants (fail closed)

| Invariant | What would be lost | Enforced by | Regression coverage |
| --- | --- | --- | --- |
| A pinned mutating command runs on the branch it was pinned to, in an allowed repository. | Another worker's branch or a protected branch is mutated unintentionally. | `git-safe --expected-branch` rejects a mismatch (`BRANCH_MISMATCH`); `supervisor run` requires that pin for direct mutating git. The pin is optional for direct `git-safe`, and an unpinned call proves nothing about runtime assignment and consults no registration. The owner allowlist bounds which repositories `gh` and managed commands may target (`OWNER_NOT_ALLOWED`); it does not bind a command to a checkout. | `git_safe/tests.rs` (`run_verifies_expected_branch_for_mutating`, `push_to_expected_branch_ok`); `supervisor_policy_tests.rs`; `owners.rs` tests. |
| No bare history destruction on a shared ref. | Peer commits vanish from the remote. | `git-safe` rejects `--force` / `-f`; only `--force-with-lease` is admitted (`BARE_FORCE_PUSH`). The PreToolUse hook applies the same classifier. | `git_safe/tests.rs` (`bare_force_rejected`, `force_with_lease_accepted`, `force_with_lease_and_bare_force_rejected`); `hook_cli.rs`. |
| Local integration never discards uncommitted WIP. | Unstaged or untracked changes are overwritten by `merge`/`pull`. | `git-safe` and the hook refuse a dirty-tree merge/pull (`MERGE_BLOCKED`); `--abort` / `--continue` / `--quit` stay allowed so recovery is possible. | `git_safe/tests.rs` (`merge_refuses_to_lose_uncommitted_wip`, `merge_abort_is_allowed_with_dirty_tree`); `hook.rs` (`pre_tool_use_refuses_merge_that_would_lose_wip_when_cwd_is_set`). |
| Two live writers never own the same task. | Duplicate work and a silent second owner after a crash or a stale ACK. | One `ACTIVE` lease per owner/repo/job and per live checkout path (`LEASE_CONFLICT`); one coord claim per job, moved only by a generation-bound handoff ACK (`COORD_CLAIM_HELD`, `COORD_STALE_GENERATION`). | `lease/tests/grant.rs`; `coord/tests.rs` (`stale_generation_handoff_is_rejected`, `handoff_ack_rejects_a_foreign_job_identity`, `regrant_deletes_stale_handoff_so_it_cannot_transfer_fresh_assignment`, `pause_rejects_released_lease_and_transferred_claim`). [#232](https://github.com/rmems/writ/pull/232) adds `coord/tests/handoff.rs` for concurrent ACKs transferring exactly once and the previous owner being rejected after transfer. |
| Crash or interruption cannot create two owners or adopt unproven ownership. | A half-registered checkout is treated as owned, or a released lease is resurrected. | Prepare / mutate / commit journal; `lease inspect` reports without mutating; `lease reconcile` promotes only a fully matching registration; retryable inspection evidence aborts with a retry outcome, and any other mismatch stays `LEASE_NEEDS_ATTENTION`. Reconciliation never revives a released or tombstoned row (`LEASE_RELEASED`, `LEASE_TOMBSTONED`); only an explicit re-registration or grant reactivates a released row, and a tombstoned row is never grantable. | `lease/tests/crash/{allocation,identity,recovery}.rs` (`inspect_does_not_mutate_or_adopt`, `conflicting_head_stays_fail_closed`, `concurrent_reconcile_cannot_resurrect_*`, `crash_after_*_is_fail_closed_without_cleanup`). |
| Registration is non-destructive and lossless. | A harness checkout is moved, reset, or mis-recorded. | `worktree register` records an existing path; it never creates, fetches, resets, or deletes. Identity is read with NUL-separated porcelain so paths containing whitespace or newlines round-trip. | `checkout.rs` tests; `porcelain.rs` tests (#140 NUL contract); `worktree_cli.rs`. |
| Start points and PR heads are exact. | Work starts from an ambiguous ref or the wrong fork commit. | Ambiguous unqualified refs are rejected before any mutation; fork PR heads are imported at the exact object id. | `identity.rs` (`ambiguous_start_point_error`, `git_ambiguous_refname_warning_is_detected`); `worktree.rs` (`create_rejects_ambiguous_unqualified_start_point_without_mutation`, `create_imports_absent_fork_pr_head_at_exact_commit`); `pr_import.rs`. |
| Cleanup never deletes ambiguous or foreign WIP. | A pause, timeout, or unregister erases someone's changes. | `unregister` and `coord pause` touch only store rows; the supervisor's timeout recovery terminates the child and leaves the checkout in place. The deprecated `remove` command rejects a path outside the worktree base; the deprecated `prune` command takes a repository root, validates no path, and drops only administrative entries for worktrees whose directories are already gone. | `coord/tests.rs` (`pause_does_not_release_or_delete_wip_and_requires_handoff_ack`); `supervisor_cancellation_tests.rs`; [timeout-policy.md](timeout-policy.md#safety). |
| Supervised processes are bounded and leave evidence. | An untracked child keeps mutating state after a timeout. | Wall-clock, idle, and grace recovery on the process group; residual fields (`timeout_class`, `recovery_stage`) are recorded, never converted into success. | `supervisor_tests.rs`, `supervisor_cancellation_tests.rs`, `timeout_policy.rs`. |
| Failures are observable. | A rejected push or an unreconciled lease is reported as done. | Policy failures exit 2 with a stable code; a lease conflict names the occupying job and a coord rejection names the live owner and generation; `status` / `watchlist` show interrupted leases. | `status.rs`, `watchlist/view_tests.rs` (`interrupted_leases_appear_on_the_default_watchlist`), `cli-contract.md` exit matrix. |

An invariant row is the only place where `writ` says "no" on its own authority. Everything below it says "here is the owner, negotiate".

## Collaboration policy (advisory)

| Signal | What it reports | What it does not do |
| --- | --- | --- |
| Declared-path overlap (`coord announce`) | Exact-file and ancestor/descendant overlap with another live claim in the same repository, with that claim's owner, branch, and job; both sides get an `overlap` message. | Does not block writes. Overlap on separate branches is a prompt to split, sequence, or hand off. |
| Scope drift (`coord drift`, carried on `coord handoff`; [#232](https://github.com/rmems/writ/pull/232)) | Paths the checkout actually changed since its lease start commit that the declaration does not cover, plus declared paths nothing touched. | Does not reset or stash anything and does not refuse the handoff; a target can re-announce a corrected declaration. |
| Pause and help (`coord pause`, `help`/`blocker`/`dependency` messages) | The owner is stalled or needs input; peers and the manager see it in the inbox and watchlist. | Does not release the lease or let another agent announce over a paused claim; the claim stays held until a handoff ACK. |
| Handoff (`coord handoff` → `coord ack`) | A generation-bound offer from the current owner to a named target. | Does not move ownership until the target ACKs that generation. A stale offer or a second ACK is rejected without cleanup. |
| Scope-growth budgets (planned, [#167](https://github.com/rmems/writ/issues/167)) | Files, churn, and fix cycles approaching a threshold. | Will not deny pushes or PR creation, delete WIP, or act as merge authorization. |
| Timeout residuals ([timeout-policy.md](timeout-policy.md)) | A supervised child was stopped (hard/idle/lost-child) and what remains. | Does not retry, merge, push, or count toward the fix cap. |

## Explicitly allowed

These are ordinary collaboration and must not be rejected merely because of the command name:

- harness-created worktrees, standalone clones, dirty or detached checkouts registering as-is;
- `git merge`, `rebase`, `cherry-pick`, and `pull` of compatible peer work into the assigned branch from a clean tree, and `--abort`/`--continue`/`--quit` from any tree;
- `--force-with-lease` on the worker's own branch after verifying the expected remote ref;
- negotiated scope expansion and handoff without a human approval ceremony for each change;
- peers exchanging SHAs, patches, findings, and conflict-resolution intent through `coord send`;
- remote-only `gh` actions (view, ready, update-branch, merge) for an allowed owner pass the helper without a local checkout. Admission is not authorization: GitHub permissions decide whether they succeed, and a merge additionally needs the operator or automation authorization described in [AGENTS.md](../AGENTS.md#remote-github-merges).

## Out of scope

- Re-implementing GitHub branch protection or remote merge authorization.
- A complete classifier for arbitrary shell syntax, host scripts, or opaque API payloads; the hook and supervisor inspect recognized `git`/`gh` argv only.
- Defending against a deliberately malicious same-user process with filesystem access to the store or checkouts.
- Writ-owned worktree creation and removal (deprecated; harness-owned since [#199](https://github.com/rmems/writ/pull/199)).
- A second task tracker, Python orchestrator, or generic agent framework.

## How to extend it

Add a new fail-closed check only when it protects a row in the invariant table, keep it in `writ-core` rather than the CLI parser, and give it a test for the actual branch, path, and WIP risk ([AGENTS.md](../AGENTS.md#source-ownership)). Anything that only helps workers negotiate belongs in the advisory table and must identify the owner instead of blocking.
