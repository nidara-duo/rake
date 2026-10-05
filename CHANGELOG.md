# Changelog

Notable changes to Rake. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each release section lists the user-visible effect, not the commits that
produced it. Anything that changes behaviour, output, or the on-disk layout of a
Scoop root belongs here.

## [Unreleased]

### Added

- `rake status` reports `Install failed`, `Manifest removed` and `Deprecated`,
  matching the flags `scoop status` already prints. An app whose manifest moved
  to `<bucket>/deprecated/` is now distinguished from one whose manifest is
  gone entirely.
- `rake status --check-buckets` fetches every bucket before judging it, so the
  verdict is against upstream. The fetch is reused by a later `rake update`
  instead of being repeated.

### Changed

- `rake status` is offline by default. It compares each bucket against the state
  left by the last fetch and says so in its output, so a stale bucket is never
  mistaken for a current one.
- `rake list` and `rake status` now agree with `scoop list`: apps installed by a
  recent Scoop are no longer missing. Scoop renamed its per-version metadata
  files to `scoop-install.json` / `scoop-manifest.json`; Rake reads both
  spellings and writes both, so old and new Scoop can each read what Rake
  installs.
- `rake info` renders manifest `notes` as a single field and substitutes
  `$dir` / `$original_dir` / `$persist_dir`, as Scoop does.
- `rake status` matches Scoop's wording and column layout: `Installed Version`,
  `Latest Version`, `Missing Dependencies`, `Info`, with empty cells rather
  than placeholders.

### Fixed

- An installed app whose manifest is missing or corrupt no longer disappears
  from `list` and `status`; its version falls back to the `current` junction
  target.
- `rake status` no longer reports a bucket as current when it could not check
  it. Failed checks are surfaced instead of being folded into "all fine".

### Performance

- `rake status` runs offline in ~40 ms on this machine, against ~5.9 s for
  `scoop status` (~1.2 s for `scoop status -l`). Bucket freshness is read from
  local git refs rather than asking each remote.

## [0.1.3]

- `rake self install`, `rake self update` and `rake self uninstall` implemented
  in Rust; the shim is vendored from `ScoopInstaller/Shim` and embedded.

## [0.1.2]

- Bucket operations use libgit2 instead of the `git` binary, with the binary
  kept as a fallback.

## [0.1.1]

- Windows-only CI and release workflow.

## [0.1.0]

- Initial release.
