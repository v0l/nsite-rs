# AGENTS.md - Coding Agent Guidelines for NSite-RS

This file is an index. Load only the specific doc(s) relevant to your task to minimize context usage.

**Always load [agents/common.md](agents/common.md) first** — it contains essential guidelines for task sizing, git commits, and git push that apply to all tasks.

<!-- Uncomment and populate when you have active work files:
| File | Description |
|---|---|
| [work/example-task.md](work/example-task.md) | Description of the task |
-->

## Generic Docs

These docs apply to all projects using this agent structure:

| Doc | When to load |
|---|---|
| [agents/bug-fixes.md](agents/bug-fixes.md) | Resolving bugs (includes regression test requirement) |
| [agents/coverage.md](agents/coverage.md) | Any edit that adds or modifies functions (100% function coverage required) |
| [agents/incremental-work.md](agents/incremental-work.md) | Managing a work file for a multi-increment task |

### Language-Specific Docs

Load the appropriate language-specific doc alongside the generic one:

| Doc | When to load |
|---|---|
| [agents/rust/coverage.md](agents/rust/coverage.md) | Rust projects: coverage tooling commands |

## Project-Specific Docs

| Doc | When to load |
|---|---|
| [nip5a.md](nip5a.md) | NIP-5A site format and subdomain encoding rules |
