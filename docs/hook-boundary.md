# Hook-boundary contracts

This is the kernel test surface for GitHub [#81](https://github.com/rmems/writ/issues/81) / Linear RM-348. The production path is `writ hook`: Claude Code writes one JSON payload on stdin, and the binary exits `0` or `2`.

These tests run in CI from fixture payloads. They do not start a live Claude Code session.

## Host composition (documented, empirically applied)

Claude Code's published rule is:

- `PreToolUse` exit `2` blocks the tool call. Stderr is the reason. A JSON `permissionDecision` of `"allow"` from this hook or a competing hook cannot override exit `2`.
- `WorktreeCreate` treats **any** non-zero exit as abort. Command-hook stdout is the created path, not JSON.

CI proves the fail-closed exit: `writ hook` returns 2 with a policy reason on stderr for blocked `git`/`gh` mutations. A JSON `permissionDecision: "allow"` from this process is never emitted on that path. A live two-hook session is still required once per [#124](https://github.com/rmems/writ/issues/124) burn-in, because only the host can run a competing hook.

## Manual end-to-end (once per #124 exit)

In a throwaway git repo, not a live multi-repo settings file:

1. Build `writ` and put it on `PATH` (or set `WRIT_BIN`).
2. Register only in that throwaway repo's `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "if": "Bash(git *)",
            "command": "writ hook"
          },
          {
            "type": "command",
            "if": "Bash(gh *)",
            "command": "writ hook"
          }
        ]
      }
    ],
    "WorktreeCreate": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "writ hook"
          }
        ]
      }
    ],
    "WorktreeRemove": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "writ hook"
          }
        ]
      }
    ]
  }
}
```

3. Ask Claude Code to run `git push --force`, `git merge`, `gh pr merge`, and `gh api`. Each must be blocked with the policy reason visible.
4. Add a second PreToolUse hook that prints `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}` and exits 0. The force-push and merge commands must still be blocked.
5. Confirm a normal session (`git status`, `git log`, `git commit`, `git push` to the assigned branch, `git rebase` onto the base, `gh pr view`, `gh pr list`) is not blocked.

Do not run `writ install` against a shared `~/.claude/settings.json` until that burn-in is complete. `writ install` is not part of this issue.

## What the suite proves

| Invariant | Where |
| --- | --- |
| Exact-base identity | `writ-core` worktree tests + `WorktreeCreate` hook CLI |
| Ambient/stale HEAD rejected | worktree create uses the requested start point |
| Ambiguous unqualified refs | same-named branch and tag, fully-qualified refs still work |
| NUL porcelain including newline paths | `--porcelain -z` parser |
| Verified reuse vs unproven resume | matching identity reattaches; moved HEAD stays fail-closed |
| Path escape | sandbox tests; hook create stays under `WRIT_WORKTREE_BASE` |
| Branch/`HEAD` postcondition | residual typed evidence, no automatic cleanup |
| Merge / auto-merge / merge-queue spellings | `PreToolUse` corpus |
| Bare force-push vs `--force-with-lease` | hook CLI |
| Protected paths | Bash writes to `.claude/settings.json` / enforcer |
| Child timeout leaves no live child | supervisor (Unix `/proc` check) |
| Malformed JSON fail-closed | exit 2 |
| Unrecognized `hook_event_name` | exit 0, no decision |
| False-positive corpus | listed ordinary commands must pass |

Prompt-only merge and force-push rules in `AGENTS.md` / `SKILL.md` remain as defense in depth until `writ install` and burn-in land. The Rust hook is now the testable boundary; it is not yet registered by default.
