# writ Skill

Installable agent skill for the `writ` Rust enforcement core.

[`AGENTS.md`](AGENTS.md) is the authoritative repository contribution and autonomy contract. This portable skill supplies platform-neutral procedures and must not broaden or relax that policy.

## When to use

Use this skill when:
- Discovering work from GitHub or Linear issues
- Spawning worker subagents for code changes
- Running [Safe Issue → Verified Commit](docs/workflows/safe-issue-verified-commit.md) then [Safe Verified Commit → PR](docs/workflows/safe-verified-commit-to-pr.md)
- Handing a pull request to the installed companion `babysit-pr` skill when interactive monitoring is needed
- Executing a human-requested one-shot merge after the automated workflows end
- Reporting results back to the operator

## Authoritative safety policy

Before any mutation, read and apply the corresponding `AGENTS.md` sections:

- [core prohibitions and deny-list](AGENTS.md#non-negotiable-safety)
- [human-authorized one-shot merge protocol](AGENTS.md#human-authorized-one-shot-merge-protocol)
- [force-with-lease allow-list](AGENTS.md#allow-list-for-force-with-lease)
- [attribution semantics](AGENTS.md#attribution-semantics)
- [team-maintainer operating model](AGENTS.md#team-maintainer-operating-model)

This skill never grants an exception to those rules. Worker, orchestrator, scheduled, discovery, issue-to-PR, and companion-skill monitoring flows never merge, including by locally merging another PR or stacked/peer branch. If the authoritative policy is unavailable, contradictory, or cannot be enforced by the Rust boundary -- `writ` itself, or an enforcing wrapper that routes the mutation through `writ-core`'s allowlist and branch verification -- stop the mutating flow and report the blocker.

### Branch/worktree pre-edit checklist

Before making any code change, the agent MUST verify:

1. **Worktree isolation:** `pwd` is inside the assigned worktree path (`{worktree_root}/{owner}/{repo}/{job_id}`).
2. **Branch correctness:** `git branch --show-current` matches the assigned feature branch.
3. **Clean state:** `git status` shows no uncommitted changes from other work. A dirty or stale primary checkout is preserved and is not a reason to abort isolated work.
4. **Remote alignment:** For a newly created, unpublished assigned branch, fetch the intended remote base and prove that the branch equals that exact remote-base commit before edits; it may lack an upstream only for this creation proof. For a published assigned branch, fetch and verify its expected upstream and the expected local/remote relationship instead of comparing the branch with the base. Stop on an unexpected upstream, unexpected remote commit, behind state, or divergence.
5. **No cross-boundary edits:** No file outside the worktree is modified (no `../` paths, no absolute paths outside the worktree root).

Repair a clean bootstrap source or a newly created unpublished branch's verified-base alignment before editing. Abort and report an unsafe identity or path mismatch, a genuine ownership collision, or any other non-recoverable failure.

### Validation and final publication sequence

Run focused, task-relevant gates while working. After the first tested
implementation and before final publication, obtain one independent review
matched to the change's risk. Additional review is required only for a named
high-risk boundary or a reproduced finding that warrants follow-up.

Before first publication of an unpublished assigned branch, fetch the verified
remote base and complete final alignment or rebase. For a published branch,
fetch and reconcile it with its expected upstream; do not require equality with
the base. Then run exactly one complete native gate suite on the exact `HEAD`
that would be pushed. An issue may add focused checks; it must not replace or
reduce that final suite. Any later tree change invalidates that run and requires
restoring the applicable alignment and rerunning the suite before push.

### Final status guidance

When handing off a pull request, report:

- **PR status:** Open / Ready for review / Blocked
- **Residual issues:** List of unresolved CI failures, review comments, or blockers
- **Agent attribution:** Every PR comment and commit message includes agent identification

A worker or companion-skill monitoring agent MUST NOT claim it merged the PR. A primary interactive agent may claim a merge only after it performed and verified the authorized one-shot operation. If another actor merged the PR, report that without taking credit.

### Platform-neutral worker prompt template

When spawning a worker subagent, include these safety instructions in the prompt:

```
SAFETY RULES (non-negotiable):
- NEVER merge a PR or invoke any merge API/CLI
- NEVER locally merge another PR or stacked/peer branch
- NEVER use bare `git push --force` or `git push -f`
- `git push --force-with-lease` is allowed only for rebasing your own branch
- NEVER edit files outside your assigned worktree
- One writable worker per assigned worktree and branch
- Before editing, verify: worktree path, branch name, clean assigned state, and remote alignment; exact remote-base equality applies only to a newly created unpublished branch, while a published branch must match its expected upstream relationship
- Repair a clean bootstrap source or unpublished verified-base alignment; abort on unsafe identity or path mismatch
- After the first tested implementation and before publication, obtain one independent risk-matched review; add review only for a named high-risk boundary or an actual finding
- After pushing, reply with SHA and agent attribution
```

Worker prompts remain strictly non-merging. Do not forward the primary agent's merge authorization to a worker or subagent.

### Enforcement routing

This skill is portable procedure, not a security boundary. Route orchestrated
mutations through `writ`, with Rust enforcing the runtime boundary as
defined in [`AGENTS.md`](AGENTS.md#enforcement-layers). The separate interactive
host merge path is available only to the primary agent after it completes the
linked one-shot authorization protocol; never forward that authority to a
worker.
