---
name: verify-technical-claims-before-acting
description: "Verify technical claims made during investigation before acting on them using empirical tests and fail-before/pass-after validation. Use this skill when investigating bugs or tech debt, if initial findings or claims need validation before being fixed or addressed."
trigger: "Use this skill when investigating bugs or tech debt, if initial findings or claims need validation before being fixed or addressed."
author: montoyaraul34
source_sessions:
  - montoyaraul34_montoyaraul34's Organization_default_93a7e2a8-e43e-415f-9c59-f8f6c233d50c
  - montoyaraul34_montoyaraul34's Organization_default_939b04fe-2d19-45b3-bebe-b9443544be27
  - montoyaraul34_montoyaraul34's Organization_default_2b68b7b1-868b-4646-8a16-650e66f53bba
  - montoyaraul34_montoyaraul34's Organization_default_311c21ab-a44b-475d-9238-b2e75fd08405
  - montoyaraul34_montoyaraul34's Organization_default_b9b899fb-1bf0-4b3f-a2fc-a6363915e78f
contributors:
  - montoyaraul34
version: 1
created_by_agent: claude_code
created_at: 2026-09-07T23:07:35.480Z
updated_at: 2026-09-07T23:07:35.480Z
---

## When to use
When you've made a characterization about code behavior, test coverage, or system output during investigation, but haven't yet verified it empirically.

## Workflow
1. **State the claim clearly** — what are you asserting? ("X has no tests", "workflow never runs", "function has no guard")
2. **Design the minimal test** — what would prove/disprove it?
   - Regression test that fails without the fix
   - Mutation test to verify test was not vacuous
   - Direct execution (dispatch workflow, run command)
   - Check existing test coverage in related files (not just the file itself)
3. **Verify fail-before/pass-after** — run the test with the claim in place (baseline), then disable/remove the claim and re-run. If the test still passes when the claim is gone, the test was vacuous; rewrite it to assert exact equality or precise behavior
4. **Correct your characterization** based on evidence — if evidence contradicts your claim, restate it accurately in commit messages and documentation

## Anti-patterns
- Asserting claims without designing how to test them first
- Running tests only one way (always with fix, never without)
- Assuming a test passing means it's not vacuous — you must verify fail-before/pass-after
- Trusting that a file has no tests without checking integration tests or related test files that import it
- Accepting "it worked" without verifying what "it" means (e.g., workflow ran but produced zero artifacts)

## Why
Early characterizations are often incomplete or wrong. "X has no tests" might mean "no unit tests in file X" but overlook integration tests elsewhere. "Workflow never runs" might actually mean "runs but produces no artifacts" — very different problems. Verifying early prevents spreading incorrect framing across commits and PRs, and ensures fixes target the real problem, not a mischaracterization.
