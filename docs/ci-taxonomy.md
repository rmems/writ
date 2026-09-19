# CI taxonomy (Class A / B / C)

Companion-skill PR monitoring classifies each status check so workers **fix
what is fixable**, **rerun flaky GitHub Actions when appropriate**, and **never
spam empty commits** to kick third-party review bots.

This is policy for agents and `writ ci classify`. It is not a merge path, not
auto-merge, and not a babysit orchestration loop. Interactive monitoring still
belongs to the installed companion `babysit-pr` skill; residual codes feed
watchlist notes and the final handoff report.

**Prefer a real source fix or an official `gh run rerun` over any empty
`chore: retrigger CI` commit.**

## Classes

| Class | What it is | Examples | Allowed | Forbidden |
| --- | --- | --- | --- | --- |
| **A** | First-party CI: GitHub Actions (`workflow` set or `github.com/.../actions/runs/...`) and Azure build/test (`dev.azure.com`) | `Build & Test`, `Validate, Test & Doc`, `rustsec`, `trivy`, `reviewdog`, Actions `Codecov`, `Analyze (rust)`, `docker`, `Limen-Neural.<repo> (BuildTest linux\|mac\|windows)` | Fetch logs; fix source in the assigned worktree; **one** official `gh run rerun` on flake (`timed_out` / `startup_failure` / `cancel` with a run id); reply with SHA after a real push | Empty retrigger commits; rerun spam on `pending`; treating skip as failure |
| **B** | Quality gates, identified by name/link (`Codacy`, `app.codacy.com`; also Sonar / Code Climate / DeepSource) | `Codacy Static Code Analysis` | Fix **actionable** file+line findings (same path as review threads); push + SHA reply | Empty push to “wake” Codacy; `gh run rerun` of a dashboard gate; looping on `ACTION_REQUIRED` with no in-repo finding |
| **C** | Third-party review status with little or no fixable log surface | `Kilo Code Review` (`kilo.ai`), `CodeRabbit`, `Gitar`, unknown StatusContext | Report residual; if the bot left **inline threads**, handle those as review work; continue Class A fixes in the same cycle | Empty retrigger commits; burning a fix budget on a gate the agent cannot close; rerunning to kick the bot |

Unknown providers default to **Class C** (conservative: no empty push).

## Inputs

### `gh pr checks`

Minimum fields:

```text
gh pr checks <n> --json name,state,bucket,workflow,link
```

| Field | Role |
| --- | --- |
| `name` | Display name; vendor matching (Codacy, Kilo, …). **Not** requiredness. |
| `bucket` | `pass` / `fail` / `pending` / `skipping` / `cancel` — mapped onto conclusions |
| `state` | GitHub state; `ERROR` is a failure; `EXPECTED` is pending |
| `workflow` | GitHub Actions workflow name. **Non-empty means Class A** unless a B/C name/link overrides |
| `link` | Actions run URL (extract run id), `app.codacy.com`, `dev.azure.com`, `kilo.ai`, … |
| `isRequired` / `required` | Optional. Present on GraphQL rollup; usually absent from `gh pr checks`. |

Legacy aliases (`conclusion`, `workflowName`, `detailsUrl`) are accepted.

### GraphQL `statusCheckRollup`

Use rollup when `bucket` is not enough (especially `ACTION_REQUIRED`):

| Field | Role |
| --- | --- |
| `__typename` | `CheckRun` vs `StatusContext` |
| `conclusion` | CheckRun conclusion, including `ACTION_REQUIRED` |
| `detailsUrl` / `targetUrl` | Same URL rules as `link` |
| `checkSuite.workflowRun.databaseId` | Actions run id when the URL is missing |
| `context` / `state` | StatusContext name and `SUCCESS` / `PENDING` / `FAILURE` / `ERROR` / `EXPECTED` |
| `isRequired` / `required` | **Requiredness.** `true` → required, `false` → advisory. Absent → unknown. Never inferred from the vendor name. |

## Bucket / conclusion interaction

| Input | Classifier conclusion | Cycle behavior |
| --- | --- | --- |
| `bucket: pass` / `SUCCESS` / `NEUTRAL` | success / neutral | Ignore (non-actionable) |
| `bucket: skipping` / `SKIPPED` | skipped | **Non-blocking.** Do not fail the cycle. |
| `bucket: pending` / `EXPECTED` / queued | pending | **Wait.** Continue other work. Do not rerun or empty-push. Class C pending still records a residual code. |
| `bucket: cancel` / `CANCELLED` | cancelled | Class A with a run id: **one** `gh run rerun`. Otherwise residual or fix-source as class dictates. |
| `FAILURE` / `ERROR` / `fail` | failure | Class A/B: fix source. Class C: residual. |
| `TIMED_OUT` / `STARTUP_FAILURE` | timed_out / startup_failure | Class A with run id: official rerun **once** before writing a code change. |
| `ACTION_REQUIRED` | action_required | Class B: **residual human gate** unless there are concrete file+line findings to fix. |
| `STALE` | stale | Treat as a failure (not success). |

An empty check list is **unknown**, not success.

## Required vs advisory vs unknown

Class A/B/C is **who owns the check**, not whether GitHub requires it. Requiredness
is parsed only from `isRequired` / `is_required` / `required`. A provider name
(Codacy, CodeRabbit, CodeScene, …) never decides a writ merge gate.

| Observation | When | Cycle behavior |
| --- | --- | --- |
| `required_failure` | `isRequired: true` and a blocking failure | Actual required-check failure. Fix source or official rerun. |
| `advisory_finding` | `isRequired: false` and a blocking failure | Report the finding. Do not treat it as a writ merge gate. |
| `pending` | Still running | Continue other work. Do not rerun-spam. |
| `external_access` | `ACTION_REQUIRED` (dashboard / login / configuration) | Residual human/config gate. Do not empty-push. Unrelated workers continue. |
| `unknown_requiredness` | Blocking outcome with **no** requiredness field | Report it. **Not** a writ merge gate and **not** a pass. |
| `success` / `skipping` | Terminal pass or skip | Ignore. |

`required_failure_count` / `required_failure_codes` on `ci.classify` count only
`required_failure`. GitHub is the required-check authority. This classifier does
not disable checks, fabricate success, or invent repository protection. If the
repository has no required contexts configured, that is an operator gap, not a
reason to treat every bot observation as required.

`fixable_failure_count` counts checks whose recommended action is **fix source**
only (not flake reruns and not residual `ACTION_REQUIRED` gates).

## Classification algorithm

```text
for each check:
  if name/link matches Codacy | Sonar | Code Climate | DeepSource → Class B
  else if name/link matches Kilo | CodeRabbit | Gitar | "code review" SaaS → Class C
  else if workflow is non-empty OR link is github.com/.../actions/runs/...
       OR link is dev.azure.com / visualstudio.com → Class A
  else → Class C
```

Then apply bucket/conclusion policy. Class B/C name matches override an Actions URL
(Codacy-as-CheckRun stays B; Kilo stays C).

## Official rerun vs empty commits

1. Diagnose Class A `fail` from logs: `gh run view <id> --log-failed` (or
   `writ gh-safe run view <id> --log-failed`).
2. If the job is a **flake** (`timed_out` / `startup_failure` / `cancel`) and a
   run id exists: `gh run rerun <id>` **once** (or `writ gh-safe run rerun <id>`).
   `writ gh-safe` allowlists `run view|list|watch|rerun|download` and **rejects**
   `run delete` / `run cancel`.
3. If the log shows a real defect: edit source, commit, push. That counts as a
   code-fix commit; do not invent an empty commit to retrigger.
4. Never create `chore: retrigger CI` / empty-tree commits for Class B or C.

## Residual blocker codes

Structured strings for watchlist state and the final report:

| Code | When |
| --- | --- |
| `class_b:codacy_action_required` | Codacy (or other B gate) concluded `ACTION_REQUIRED` with no closable in-repo finding |
| `class_c:kilo_pending` | Kilo still pending |
| `class_c:coderabbit_pending` | CodeRabbit still pending |
| `class_c:gitar_fail` | Gitar failed / errored |
| `class_c:third_party_fail` | Unknown third-party failure |
| `class_a:<slug>_action_required` | Rare Class A `ACTION_REQUIRED` |

Class A ordinary failures and Class B failures with fixable findings **do not**
emit a residual code until the agent cannot close them. Class A/B **pending**
does not emit a residual (continue other work). Class C pending **does**.

Record these codes in handoff `notes` and any watchlist blocker list. They do
not replace `JobStatus.ci_class` (`pass` / `fail` / `pending` / `unknown`), which
is a job-level rollup, not A/B/C. Watchlist consumers should keep
`required_failure_codes` distinct from `advisory_finding_codes`,
`pending_codes`, `external_access_codes`, and `unknown_requiredness_codes`.

## CLI

```bash
gh pr checks <n> --json name,state,bucket,workflow,link \
  | writ --json ci classify
```

Envelope command is `ci.classify`. Invalid input uses `ok: false` and
`error.code: CLASSIFY_INPUT_INVALID`.
