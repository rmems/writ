# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

`writ` is a Rust-first, provider-neutral coordination layer for parallel coding agents: harnesses own worker and checkout lifecycle, and `writ` supplies shared coordination state (identity, ownership leases, declared paths, messages, handoff, recovery). GitHub is the source/PR/review/checks authority; Linear is the task tracker.

## Repository policy

[`AGENTS.md`](AGENTS.md) is the authoritative contribution, autonomy, attribution, review, validation, Beads, and session-completion contract. Claude agents must read it before mutating work. Do not duplicate that common contract here.

Use these `AGENTS.md` sections as the single source of truth:

- [Beads Issue Tracker](AGENTS.md#beads-issue-tracker)
- [Session Completion](AGENTS.md#session-completion)
- [Attribution semantics](AGENTS.md#attribution-semantics)
- [Local collaboration](AGENTS.md#local-collaboration-allowed)
- [Remote GitHub merges](AGENTS.md#remote-github-merges)

Interactive PR monitoring belongs to the installed companion `babysit-pr` skill. That skill is portable operator guidance, not a security boundary. Rust `writ-core` remains the hard code-enforced boundary for worktree, branch, path, process, push, and default-branch controls. Local feature-branch integration is allowlisted.

## Claude-specific delegation

When Claude delegates writable work, give each worker its own assigned worktree and branch — created by the harness or plain `git worktree add`, then joined via `writ worktree register`. The controller coordinates results and retains publication authority; it never gives two writable workers a shared worktree. Use the portable worker prompt and lifecycle in [`SKILL.md`](SKILL.md), and the shared review checklist in [`REVIEW.md`](REVIEW.md).
