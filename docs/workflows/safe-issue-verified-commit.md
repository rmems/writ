# Task → tested commit

Use the existing request, Linear task or GitHub issue. A tracker record is optional. Follow [`AGENTS.md`](../../AGENTS.md) for the contribution contract; this workflow is a helper, not an additional gate.

1. Read the task and actual source. Check peer ownership and existing PRs before implementing.
2. Reuse a suitable assigned checkout and branch, or let the harness create isolation for an independent writer. Read-only helpers use the current checkout. Inspect branch identity, upstream and WIP; preserve useful history. No exact-base reset, managed directory or tracker bootstrap is required.
3. Register the checkout if using writ's shared lease store. Missing optional helpers do not block the host's authorized native workflow.
4. Add a failing regression for a bug or characterization before changing unclear behavior. Implement the bounded change, run focused checks, and integrate compatible peer work locally. Preserve uncommitted changes and never bare-force-push.
5. Audit the staged files and commit attribution. Publish a checkpoint when authorized, with honest validation and remaining work. A failed or unrun check is a residual, not a reason to discard useful work or misstate readiness.
6. Before a ready-for-review handoff, obtain risk-appropriate independent review and run the native gates on the submitted tree:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

7. Verify the intended remote, branch and upstream relationship before pushing. Reconcile unexpected peer changes safely. Use `--force-with-lease` only for an authorized rewrite of your own branch. Report a pushed SHA only after the remote accepted it.
8. Continue to [PR handoff](safe-verified-commit-to-pr.md). Update the existing task if one was supplied; no GitHub twin, extra issue, database sync or separate issue comment is required.

A dry run stops before mutations/publication. Stop for a genuine ownership collision, destructive or out-of-scope action, or material unresolved design choice. Ordinary conflict repair is part of collaboration. GitHub decides remote permissions; actual merges require operator authorization and are not an automatic consequence of this workflow.
