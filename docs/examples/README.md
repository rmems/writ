# Response envelope examples

Every fixture here is captured from the `writ` binary and then pretty-printed.
Re-capture rather than hand-editing — three fixtures that previously lived here
were fiction, and nothing caught it because nothing compared them to real output:

| Fixture | Claimed | Reality |
| --- | --- | --- |
| `error-force-push.json` | command `git.run`, code `PolicyForcePushForbidden` | the envelope command is `git.safe`; the code is `BARE_FORCE_PUSH` |
| `error-policy.json` | same as above | same as above |
| `error-merge-forbidden.json` | command `gh.run`, code `PolicyMergeForbidden` | the envelope command is `gh.safe`; the code is `MERGE_BLOCKED` |

Neither `PolicyForcePushForbidden` nor `PolicyMergeForbidden` exists in
`PolicyCode`.

## Which commands emit an envelope

All of them, under `--json`:

| Command | Envelope `command` |
| --- | --- |
| no subcommand | `cli.bootstrap` |
| `status` / `jobs` | `cli.status` / `cli.jobs` |
| `git-safe` / `gh-safe` | `git.safe` / `gh.safe` |
| `worktree create\|list\|remove\|prune` | `worktree.create` etc. |

**The exception is the policy-violation path**, which prints plain text on stderr
with exit code 2 instead of an envelope:

```console
$ writ --json git-safe push --force
writ: policy violation [BARE_FORCE_PUSH]: bare --force/-f is not allowed; use --force-with-lease only

$ writ --json gh-safe pr merge 1
writ: policy violation [MERGE_BLOCKED]: `gh pr merge` is not allowed
```

So `git-safe` and `gh-safe` *do* emit `git.safe` / `gh.safe` envelopes on success —
see `git-safe-success.json` — but a rejected command is not reported that way. The
bracketed token is the stable `PolicyCode`; parse that rather than the prose.

An earlier revision of this file claimed those two commands never emit an envelope
at all, and that `cli.bootstrap` did not exist. Both were wrong: `writ --json` with
no subcommand emits `cli.bootstrap`, which is what `bootstrap.json` records.

## Regenerating

```bash
cargo build
writ --json                                   # cli.bootstrap
writ --json status                            # cli.status
writ --json git-safe --repo <repo> rev-parse --is-inside-work-tree   # git.safe
writ --json worktree list                     # worktree.list
# error envelope, no repository mutation:
writ --json worktree create --schema-version 2 --repo <repo> <owner> <repo-name> <job> <branch>
```
