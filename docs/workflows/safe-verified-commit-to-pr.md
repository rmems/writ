# Tested commit → PR handoff

Open or update a reviewable PR for the authorized task. Follow [`AGENTS.md`](../../AGENTS.md); use the host's available GitHub tools. Interactive monitoring is an optional companion `babysit-pr` workflow.

1. Verify repository, base, assigned branch and the SHA accepted by the remote. Use the stack parent as base when appropriate and process stacks bottom-up.
2. Update an existing PR for this branch. Otherwise create a focused PR explaining the problem, resulting behavior, compatibility impact and measured validation. A useful checkpoint may be a draft with explicit remaining work.
3. Link the existing task when one exists. Use `Closes #N` only when a real GitHub issue is fully resolved; a Linear URL is not a GitHub closing reference. Do not manufacture a twin or require a tracker account.
4. Use applicable existing labels, assignee and project conventions. Metadata helps review; it is not a prerequisite for saving a checkpoint.
5. Re-fetch the persisted PR and verify its body, base and head. Inspect current CI, conflicts and review feedback before claiming readiness. Fix in-scope findings and report unavailable checks truthfully.
6. Handoff includes the PR URL, actual head SHA, local/hosted validation, review state and precise residuals. Update only the task record already used. No duplicated report is required.

Remote merge modes, readiness and update-branch operations are governed by GitHub and operator authorization, not an extra writ gate. This workflow itself does not authorize a merge, release or repository-settings change. A rejected push is not delivery, and an empty PR is not a way to retrigger CI.
