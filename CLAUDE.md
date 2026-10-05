# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

`writ` is a Rust-first, provider-neutral coordination layer for parallel coding agents: harnesses own worker and checkout lifecycle, and `writ` supplies shared coordination state (identity, ownership leases, declared paths, messages, handoff, recovery). GitHub is the source/PR/review/checks authority; Linear is the task tracker used by this repository's maintainers.

## Repository policy

[`AGENTS.md`](AGENTS.md) is the authoritative contribution, autonomy, attribution, review, validation, and session-completion contract. Claude agents must read it before mutating work. Do not duplicate that common contract here.

Use these `AGENTS.md` sections as the single source of truth:

- [Session Completion](AGENTS.md#session-completion)
- [Attribution semantics](AGENTS.md#attribution-semantics)
- [Local collaboration](AGENTS.md#local-collaboration-allowed)
- [Remote GitHub merges](AGENTS.md#remote-github-merges)

Interactive PR monitoring belongs to the installed companion `babysit-pr` skill. That skill is portable operator guidance, not a security boundary. Rust `writ-core` provides local integrity and coordination helpers. GitHub owns remote authorization. Missing optional helpers do not block contribution.

## Claude-specific delegation

Read-only helpers may share the current checkout. Give independent writers separate assigned checkouts and branches; reuse suitable harness-owned checkouts. Register them when using writ shared coordination. The controller coordinates results and retains publication authority; it never gives two writable workers a shared worktree. Use the portable worker prompt and lifecycle in [`SKILL.md`](SKILL.md), and the shared review checklist in [`REVIEW.md`](REVIEW.md).
