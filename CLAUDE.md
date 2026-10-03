# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

`writ` is a Rust-first, provider-neutral coordination layer for parallel coding agents: harnesses own worker and checkout lifecycle, and `writ` supplies shared coordination state (identity, ownership leases, declared paths, messages, handoff, recovery). GitHub is the source/PR/review/checks authority; Linear is the task tracker used by this repository's maintainers.

## Repository policy

[`AGENTS.md`](AGENTS.md) is the authoritative contribution and autonomy contract. Claude agents must read it before mutating work. Do not duplicate that contract here.

Use these sections:

- [Contribution](AGENTS.md#contribution)
- [Attribution semantics](AGENTS.md#attribution-semantics)
- [Local collaboration](AGENTS.md#local-collaboration-allowed)
- [Remote GitHub merges](AGENTS.md#remote-github-merges)

Interactive PR monitoring belongs to the installed companion `babysit-pr` skill. That skill is portable operator guidance, not a security boundary. Rust `writ-core` remains the hard code-enforced boundary for worktree, branch, path, process, push, and default-branch controls. Local feature-branch integration is allowlisted.

## Claude-specific delegation

Read-only review stays in the current checkout. When Claude delegates writable work, give each writer its own harness-owned checkout and branch, then join it with `writ worktree register`. The controller coordinates results and does not give two writable workers one worktree. Use [`SKILL.md`](SKILL.md) and [`REVIEW.md`](REVIEW.md).
