# Dogfood: two same-host workers on one bounded task (#97)

Date: 2026-10-25 (UTC). Binary: `writ` built from `feat/coord-handoff-drift` at `137ac34` (PR [#232](https://github.com/rmems/writ/pull/232)). Shared store: one `leases.db` via `WRIT_LEASE_PATH`; `WRIT_ALLOWED_OWNERS=rmems`.

This records what two independent worker processes observed while using `writ coord` on a real, bounded change to this repository. Observed behaviour is separated from ideas at the end. Raw command logs (every writ invocation and its JSON) are the per-worker logs kept with the run; the resulting commits are on the branches below.

## Setup

| Worker | Job | Checkout / branch | Declared paths | Task |
| --- | --- | --- | --- | --- |
| agent-a (`sess-a`) | `rmems/writ/dogfood-a` | `writ-dogfood/worker-a`, `exp/dogfood-worker-a` | `docs/examples`, `README.md` | Capture real `coord announce` / `drift` / `handoff` envelopes as fixtures, document them, link from README |
| agent-b (`sess-b`) | `rmems/writ/dogfood-b` | `writ-dogfood/worker-b`, `exp/dogfood-worker-b` | `crates/writ/src/main.rs`, `SKILL.md`, `README.md` | Add `coord drift`/`handoff` parser tests, start a SKILL quick reference, link from README |

The controller created both worktrees with plain `git worktree add`, registered them with `writ worktree register`, and started both workers at the same time as separate processes. `README.md` was declared by both on purpose: each worker appends to the same status-table row, so the overlap is real and the resulting branch conflict is deliberate. The controller did not take part in the coordination until integration.

## Timeline (observed)

| UTC | Who | Event |
| --- | --- | --- |
| 08:48:55 | b | `announce` → claim gen 1, intent msg 1, `overlaps: []` (a had not announced yet) |
| 08:49:02 | a | `announce` → claim gen 1, intent msg 2, `overlaps: [{job dogfood-b, agent-b, branch exp/dogfood-worker-b, paths: [README.md], advisory: true}]`; overlap msgs 3 (a→b) and 4 (b→a) created in the same call |
| 08:49:09 | b | `send intent` 5 to a ("will append a SKILL link to the README coord row"); `send dependency` 6 to a asking for the fixture filenames |
| 08:49:21 | a | `send intent` 7 to b (will edit README line 261 only) |
| 08:49:38–52 | a | Captured the three fixtures in a throwaway repo with its own store (announce, drift with `undeclared_paths: [src/b/mod.rs]`, handoff whose `paths` equal that list) |
| 08:50:02 | a | `ack` 6 → ack msg 8; `send dependency` 9 to b listing the filenames |
| 08:50:19–43 | b | Saw 9 on the next 20 s poll; `ack` 9 → ack msg 10 |
| 08:50:41 | a | `drift dogfood-a` before committing: five changed paths, `undeclared_paths: []`; commit `4a36acf` |
| 08:50:55 | b | Parser tests pass (`cargo test -p writ coord_`), fmt clean; commit `d1f6e8b` (tests + README link) |
| 08:51:15 | b | Started the SKILL section and left it **uncommitted** on purpose |
| 08:51:32 | b | `pause` → `paused_at` set, help msg 11 broadcast; `show` → gen 1; `handoff` 12 to agent-a, `to_job dogfood-b`, `paths: []` (every changed path was declared) |
| 08:51:43 | b | `send` as `agent-c` on dogfood-b → exit 2, `COORD_CLAIM_HELD` |
| 08:51:53 | a | Polling `inbox dogfood-a` showed help 11 but **no handoff** (see finding 1) |
| 08:52:10 | a | `inbox dogfood-b` showed handoff 12 |
| 08:52:29 | a | `ack dogfood-b --id 12` → claim `agent_id: agent-a`, `session_id: sess-a`, `owner_generation: 2`, `paused_at: null`; ack msg 13 at gen 2. `drift dogfood-b`: changed `[README.md, SKILL.md, crates/writ/src/main.rs]`, undeclared `[]`. Re-`ack` 12 → `ok: false`, "message 12 is already acknowledged" |
| 08:52:47 | b | `show` poll saw `agent_id: agent-a`, gen 2 (15 s after the ACK) |
| 08:52:56 | b | Duplicate `handoff` at gen 1 as agent-b → exit 2, `COORD_CLAIM_HELD` ("owned by agent-a (generation 2)") |
| 08:53:02 | a | Finished the SKILL section in worker-b's checkout on top of b's uncommitted edit; commit `8e64d8a` on `exp/dogfood-worker-b` |
| 08:53:20 | a | `coord list`: dogfood-a agent-a gen 1; dogfood-b agent-a gen 2. Inbox b = msgs 1–13 with 6, 9, 12 acked |
| integration | controller | `exp/dogfood-two-workers` ← fast-forward a; merge b → `CONFLICT README.md` on the shared row; resolved by keeping both links (`a0ca06c`); `cargo fmt --check`, `clippy -D warnings`, `cargo test -p writ` green |

Wall-clock overlap: both workers were active from 08:48:55 to 08:53:25; the dependency round-trip and the handoff happened while the other worker was still running.

## Acceptance check

- **Real bounded task, not a fixture.** Yes: three captured envelope fixtures, a docs section, two parser unit tests, a SKILL quick reference, one README row. All of it is on the integration branch and passes the native gates.
- **Two isolated workers genuinely overlap in wall-clock execution.** Yes (table above).
- **Shared-store communication/ownership demonstrated.** Intent, overlap, dependency, ack, help, handoff and ack messages all went through `leases.db`; `coord list` from either process showed both claims.
- **Deliberate overlap → negotiation, not divergence or a freeze.** The overlap was reported to both sides in the announcing call; each worker sent an intent naming the exact line it would touch and kept working. Neither was blocked.
- **Deliberate conflict resolved and validated.** The README row conflicted at merge; the integration owner combined both edits and ran the gates.
- **Recovery does not lose WIP or create two owners.** b's uncommitted SKILL edit survived pause and transfer and is in `8e64d8a`. After the ACK there was exactly one claim for dogfood-b (gen 2); b's duplicate offer and a foreign agent's send were both rejected with `COORD_CLAIM_HELD`; a second ACK of the same offer was rejected.
- **Human interventions.** None during the run. The controller only set up checkouts and integrated.

## Findings (observed)

1. **A handoff is only visible in the inbox of the job being transferred.** The offer for dogfood-b is addressed `to_job: dogfood-b, to_agent: agent-a`, so `coord inbox … dogfood-a` never shows it. agent-a's poller watched its own job and would not have fired; it found the offer by reading dogfood-b's inbox. The SKILL quick reference now says so. An `--agent` filter on `coord inbox` (messages addressed to an agent across jobs) would remove the trap.
2. **`writ status` does not show coord ownership.** With dogfood-b at gen 2 owned by agent-a, `status` reported `agent_id`, `ownership_generation`, `declared_paths` and `handoff` as `null` for both jobs. Those fields exist in the v2 schema but are not populated from `coord_claims`; only `watchlist` overlays coord state. A manager reading `status` cannot see who owns what.
3. **Message ids are store-global.** Ids 5–6 from b landed between a's announce and a's first send. Harmless, but workers should not assume per-job numbering.
4. **Rejections are observable in both channels.** Policy rejections exit 2 with a `COORD_CLAIM_HELD` envelope plus a stderr line; the already-acknowledged case is an `ok: false` envelope with `LEASE_STORE_FAILED`.
5. **Drift was accurate for both jobs.** Both declarations covered every changed path, so `undeclared_paths` was empty and the handoff carried `paths: []`; the fixture run shows the non-empty case.

## Ideas, not observed

- Deliver handoff offers to the target agent's inbox regardless of job (finding 1), or add `coord inbox --agent`.
- Populate `status` ownership fields from `coord_claims` (finding 2) so `status` and `watchlist` agree.
- Scope-growth budgets ([#167](https://github.com/rmems/writ/issues/167)) were not exercised; this run was too small to approach any threshold, so it does not yet justify them.
- Timeouts, crashes and stale leases were not exercised; `pause` was a voluntary stall.
