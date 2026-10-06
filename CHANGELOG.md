# Changelog

Notable changes to Rake. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each release section lists the user-visible effect, not the commits that
produced it. Anything that changes behaviour, output, or the on-disk layout of a
Scoop root belongs here.

## [0.1.4-alpha.2]

A development build. As with 0.1.4-alpha.1, `install <app>` and `update <app>`
are still open; this release does not claim otherwise. It sorts above 0.1.4-alpha.1
and below 0.1.4.

### Changed

- `rake status` column headings are now `Name`, `Installed`, `Latest`,
  `Missing Deps` and `Info`. This reverses an earlier change that matched Scoop's
  headings word for word — the extra words were noise in a table this narrow.
  Reported values remain Scoop-compatible.
- Command names in `rake status` output are shown in bold instead of wrapped in
  backticks, which a terminal prints verbatim.
- Packages, buckets and the cache now always live in Scoop's directory layout
  (`~/scoop`, or `$SCOOP` when set). Rake previously fell back to a `~/rake`
  directory when Scoop was not installed, which split the two tools into separate
  package trees. Sharing one layout means installing Scoop later finds the packages
  already there rather than starting empty.
- Rake reads the same settings file Scoop writes: `%USERPROFILE%\.config\scoop\
  config.json`, or `%XDG_CONFIG_HOME%` when that is set. It previously looked only
  at `<root>/config.json`, which is the portable-install case, so on a normal
  machine the file was never found and settings such as the chosen shim were
  silently ignored. `$SCOOP_CACHE` is now honoured too, matching Scoop's
  precedence for the cache directory.

### Added

- `rake status --quiet` (`-q`) suppresses informational notes. Warnings and
  errors are unaffected, so an out-of-date or uncheckable bucket is still
  reported.
- 107 tests, most of them covering behaviour that had none: manifest parsing and
  architecture fallback, the install-record format on disk, persist data safety,
  archive extraction guards, `cleanup`, `uninstall` and the PowerShell quoting
  rules.

### Fixed

- `rake checkup` no longer reports the Windows Defender exclusion check as passing
  when it could not be performed. Two separate problems combined: the exclusion
  query answered "excluded" whenever PowerShell could not be started or its own
  `catch` fired, and the service query answered "Defender is not running" in the
  same situation, which short-circuited the check to OK before it even ran. Both
  now distinguish "no" from "could not tell", and an unanswered check is reported
  as such.
- JSON files carrying a UTF-8 byte order mark are now read correctly. This
  affected bucket manifests, installed manifests and the settings file: the file
  was discarded and the app simply did not appear, while Scoop — which strips the
  mark — carried on working. Windows tooling writes one readily.
- Installing over an existing install no longer fails with `AlreadyExists` when
  the new version ships its own copy of a persisted directory. Scoop keeps that
  copy as `<name>.original` next to the app; Rake now does the same, so the
  shipped defaults stay recoverable instead of colliding with the junction.
- `rake cleanup` and `rake uninstall` no longer report success for files they
  could not delete. An app still running from the old version made both discard
  the error and print a success line while the directory was still on disk. Both
  now stop at the failure and say what could not be removed and why, as Scoop does.
- `rake cleanup -k` no longer deletes the cached download for the version you are
  running. It emptied the whole cache; Scoop keeps the current version's entry and
  removes only stale mirrors and interrupted `.download` files. `-k` is also now
  scoped to the apps actually being cleaned rather than hitting every app.
- A shortcut's `target` is validated, not only its `name`. A `..` in the target
  let a manifest point a Start Menu shortcut at any file on the disk.
- `persist` entries can no longer name a path outside `apps/` and `persist/`. A
  `..` in either half of a manifest's `persist` field created a junction, or moved
  your data, elsewhere on the disk.
- Manifest values interpolated into generated PowerShell are escaped. An app name
  containing `"` closed the string in the `.ps1` shim and turned the rest of the
  name into code, which ran every time the shim was invoked.
- `env_add_path` entries containing `..` are refused. They passed a check that
  compared path strings, so an entry like `..\..\..\..\Startup` was accepted and
  resolved to `C:\Startup` — a PATH entry that persists after uninstall.

### Internal

- `validate_relative_path` is now the single place that rejects a manifest-supplied
  path which could escape its directory. `bin`, `persist`, shortcuts and
  `env_add_path` all go through it; three divergent copies of the check existed.
- Test sessions record environment writes instead of performing them, so tests
  cannot modify `HKCU\Environment`.

## [0.1.4-alpha.1]

A development build: the features below are in, but `install <app>` and
`update <app>` are still open and this release does not claim otherwise. It
sorts above 0.1.3 and below a future 0.1.4.

### Added

- `rake status` reports `Install failed`, `Manifest removed` and `Deprecated`,
  matching the flags `scoop status` already prints. An app whose manifest moved
  to `<bucket>/deprecated/` is now distinguished from one whose manifest is
  gone entirely.
- `rake status --check-buckets` fetches every bucket before judging it, so the
  verdict is against upstream. The fetch is reused by a later `rake update`
  instead of being repeated.
- Manifest `installer.script` hooks now run. They are not standalone
  PowerShell — they call Scoop's own functions, which only resolve when
  Scoop's library is loaded — so Rake vendors it and dot-sources it for that
  one hook. Other hooks keep running standalone.

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

- `rake update` no longer breaks `scoop update`. It left every bucket in
  detached HEAD, and Scoop's `git pull` then failed with "You are not currently
  on a branch" — silently, since Scoop carries on past the error and still
  reports success. Buckets are shared between the two tools, so they now stay
  on a branch, and a bucket an older build already detached is repaired.
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
