# Contributing to Rake

Thank you for considering contributing to Rake! This document outlines the conventions and guidelines for contributing to this project.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Pre-commit Checklist](#pre-commit-checklist)
- [Commit Message Format](#commit-message-format)
- [Code Style & Rust Conventions](#code-style--rust-conventions)
- [Pull Request Process](#pull-request-process)

## Architecture Overview

Rake follows a four-layer architecture:

- **`rake-domain`** — Pure types (Package, Manifest, Version, Config), zero I/O, no async, no framework dependencies.
- **`rake-core`** — Async orchestration (install, download, query, remove), state management, and the internal Event Bus.
- **`rake-cli`** — Terminal frontend powered by `Clap` for argument parsing and `Indicatif` for interactive progress bars.
- **`rake-hash`** / **`rake-shim-bin`** — Supporting crates for file hashing and Windows executable shimming.

All I/O is behind traits (`HttpClient`, `GitService`, `ArchiveService`). Operations are async using `tokio`, and progress/prompts flow through a two-channel `EventBus` (flume).

## Pre-commit Checklist

Before every commit, run **all** of the following in order:

```sh
cargo clippy --fix --allow-dirty
cargo fmt
cargo clippy
cargo test
```

Only commit when all pass clean. No warnings, no formatting diffs, no failing tests.

## Commit Message Format

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

## Code Style & Rust Conventions

- Use `crate::` imports for internal crate references.
- Errors use `crate::error::Fallible` and `crate::error::Error`.
- Public API gets doc comments (`///`); internal items are `pub(crate)`.
- Match existing patterns: look at neighbouring files before writing new code.
- **Domain purity**: `rake-domain` must have no I/O, no async, no framework dependencies.
- **Trait-based infra**: All I/O operations must be behind traits.
- **Throttled progress**: Download progress is capped at 100ms intervals.

## Pull Request Process

1. Ensure your code passes the full [pre-commit checklist](#pre-commit-checklist).
2. Update the `readme.md` or architecture docs if your change introduces new commands or modifies existing behaviour.
3. Open a Pull Request with a clear title and description, following the commit message format as the PR title.
4. A maintainer will review your changes. Please be responsive to feedback.

## License

By contributing to Rake, you agree that your contributions will be licensed under the [GPL-3.0-or-later](COPYING) license.
