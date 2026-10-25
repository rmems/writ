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

Dispatched commands use these envelope names under `--json`; hook protocol and pre-dispatch failures are exceptions:

| Command | Envelope `command` |
| --- | --- |
| no subcommand | `cli.bootstrap` |
| `status` / `jobs` | `cli.status` / `cli.jobs` |
| `watchlist list\|check\|check-all` | `cli.watchlist.list` / `cli.watchlist.check` / `cli.watchlist.check_all` |
| `watchlist add\|remove` | `cli.watchlist.add` / `cli.watchlist.remove` (never persist) |
| `git-safe` / `gh-safe` | `git.safe` / `gh.safe` |
| `ci classify` | `ci.classify` |
| `worktree register\|unregister\|inspect\|list` | `worktree.register` etc. |
| `worktree create\|remove\|prune` | `worktree.create` etc. (deprecated) |
| `lease inspect\|reconcile` | `lease.inspect` / `lease.reconcile` |
| `coord announce\|show\|list\|inbox\|drift\|send\|ack\|pause\|handoff` | `coord.announce` etc. (see [Coord fixtures](#coord-fixtures)) |
| `attribution format` | `attribution.format` |
| `supervisor run` | `supervisor.run` (also `--idle`/`--stall`, `--grace`, `--progress-secs`, `WRIT_SUPERVISOR_*`; see [timeout-policy.md](../timeout-policy.md)) |
| `install` | `cli.install` |
| `hook` | no envelope even under `--json`: PreToolUse is allow/block via exit code; a WorktreeCreate event prints the registered checkout path only when the path is an existing git checkout — missing, nonexistent, or non-git paths produce no stdout |

**The exception is the policy-violation path**, which prints plain text on stderr
with exit code 2 instead of an envelope:

```console
$ writ --json git-safe push --force
writ: policy violation [BARE_FORCE_PUSH]: bare --force/-f is not allowed; use --force-with-lease only

$ writ --json gh-safe pr checkout 1
writ: policy violation [MERGE_BLOCKED]: gh command changes the local checkout; use assigned-branch git operations
```

So `git-safe` and `gh-safe` *do* emit `git.safe` / `gh.safe` envelopes on success —
see `git-safe-success.json` — but a rejected command is not reported that way. The
bracketed token is the stable `PolicyCode`; parse that rather than the prose.

An earlier revision of this file claimed those two commands never emit an envelope
at all, and that `cli.bootstrap` did not exist. Both were wrong: `writ --json` with
no subcommand emits `cli.bootstrap`, which is what `bootstrap.json` records.

## Coord fixtures

Captured from a throwaway repository (`acme/sample`, one linked worktree `job-a`) with its own
`WRIT_LEASE_PATH`, so the ids and generations start at 1. Two fields are redacted
for portability: `worktree_path` is replaced with `/path/to/job-a` and
`start_commit` with `<start-commit>`; everything else is verbatim binary output.

| Fixture | Command | What it shows |
| --- | --- | --- |
| `coord-announce.json` | `coord announce acme sample job-a --agent agent-a --session sess-a --intent ... --path src/a` | the claim with its `declared_paths`, the broadcast `intent` message, and an empty `overlaps` array (a second claim on `src/a` would list it there, advisory only) |
| `coord-drift.json` | `coord drift acme sample job-a` | `changed_paths` since `start_commit` (a committed `src/a/mod.rs` and an untracked `src/b/mod.rs`) compared with `declared_paths`; `src/b/mod.rs` lands in `undeclared_paths`, and `untouched_declared_paths` is empty because `src/a` was touched |
| `coord-handoff.json` | `coord handoff acme sample job-a --agent agent-a --to-agent agent-b --generation 1 --body ...` | the `handoff` message whose `paths` carry exactly the undeclared changes from drift, so the receiver knows what the declaration missed |

## Regenerating

```bash
cargo build
writ --json                                   # cli.bootstrap
writ --json status                            # cli.status
writ --json git-safe --repo <repo> rev-parse --is-inside-work-tree   # git.safe
writ --json ci classify --file docs/examples/ci-classify-input.json  # ci.classify
writ --json worktree list                     # worktree.list
writ --json worktree register <path>          # worktree.register
writ --json attribution format --body "Looks good!"   # attribution.format
# coord fixtures (separate WRIT_LEASE_PATH, WRIT_ALLOWED_OWNERS=acme, see "Coord fixtures"):
writ --json coord announce acme sample job-a --agent agent-a --session sess-a --intent "implement module a" --path src/a   # coord.announce
writ --json coord drift acme sample job-a                                                                              # coord.drift
writ --json coord handoff acme sample job-a --agent agent-a --to-agent agent-b --generation 1 --body "handing off"     # coord.handoff
# error envelope, no repository mutation:
writ --json worktree create --schema-version 2 --repo <repo> <owner> <repo-name> <job> <branch>  # deprecated
```
