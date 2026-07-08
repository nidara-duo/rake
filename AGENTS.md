# Rake — Scoop-compatible package manager for Windows, written in Rust

## What is Rake?

A clean-room reimplementation of the Scoop package manager. Four-layer architecture:

- **Domain** — pure types (Package, Manifest, Version, Config), zero I/O
- **Operations** — async orchestration (install, download, query, remove)
- **Infrastructure** — trait-based I/O (HttpClient, ArchiveService, GitService)
- **CLI** — clap + indicatif, event consumer loop

## Key rules

- **Domain purity**: Domain types have no I/O, no async, no framework deps.
- **Trait-based infra**: All I/O is behind traits (`HttpClient`, `GitService`, `ArchiveService`).
- **Async everywhere**: Operations are async functions using `tokio`.
- **Event-driven UI**: Progress and prompts flow through a two-channel `EventBus` (flume).
- **Throttled progress**: Download progress capped at 100ms intervals.

## Pre-commit checklist

Before every commit, run **all** in order:

```sh
cargo clippy --fix --allow-dirty
cargo fmt
cargo clippy
cargo test
```

Only commit when all pass clean.

## Push policy

Never push to remote unless explicitly asked.
Commit locally only — the user decides when to push.

## Commit message format

```
<type>(<scope>): <short description>

[optional body: bullet points for what and why, not how]

[skip ci]
```

### Types

| Type       | When to use                              |
|------------|------------------------------------------|
| `feat`     | New feature or user-facing addition      |
| `fix`      | Bug fix                                  |
| `refactor` | Code change that is neither feat nor fix |
| `style`    | fmt-only, clippy-only, formatting        |
| `chore`    | CI, deps, tooling, maintenance           |


### Scope

Lowercase, matches the crate or module name, e.g. `list`, `status`, `cli`, `libscoop`, `archive`, `bucket`.

### Body

- Explain **what** changed and **why**, not how.
- Use `- ` bullet points.
- If CI should be skipped (formatting-only, docs, etc.), append `[skip ci]`.

## Rust conventions

- `crate::` imports for internal crate references.
- Errors use `crate::error::Fallible` and `crate::error::Error`.
- Public API gets doc comments (`///`); internal items are `pub(crate)`.
- Match existing patterns: look at neighbouring files before writing new code.

## Acceptance criteria

The work is done when:

- the code compiles (`cargo build`)
- `cargo fmt` produces no diff
- `cargo clippy` produces no warnings
- relevant tests pass
- the behaviour is demonstrable (a CLI invocation or a test assertion)
