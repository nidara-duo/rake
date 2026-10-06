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

- `rake self update --pre-release` (also on `rake self install`) takes the newest
  pre-release when one is newer than the newest finished release, and nothing else.
  A tag carrying a `-` counts as a pre-release, and so does one GitHub marks as
  such — either signal is enough, because the two can disagree and the version is
  what the ordering is actually computed from. `1.0.0+linux` is not a pre-release:
  build metadata does not make one.
- `rake self update` names the version it installs, and says which pre-release is
  being held back rather than reporting a flat "already up to date".
- `scripts/bootstrap.ps1` takes `-Prerelease` and `-Version`, so the very first
  install can choose which release it takes. It had no way to: the endpoint it
  used never returns pre-releases. A dash in the tag is treated as the signal,
  rather than GitHub's `prerelease` flag, because that flag is written once when
  the release is created and `v0.1.4-alpha.1` predates the workflow setting it.
- Dot-sourcing `bootstrap.ps1` now only defines its functions. Installing happened
  as a side effect, so anything that loaded the script for its helpers installed
  Rake as well.
- `rake settings` — get, set and reset user preferences, stored in
  `~/.config/rake/settings.json`. Deliberately a separate file from Scoop's
  `config.json`, whose `set_config` rewrites the whole document and would drop
  anything stored there. `rake settings` lists every setting with its value and
  default, marking the ones you changed; `settings edit` opens the file in
  `$EDITOR` and validates what you wrote.
  - `status.offline_by_default` (default `true`) — run `status` without touching
    the network unless asked
  - `status.hide_offline_note` (default `false`) — suppress the note explaining
    that the bucket verdict came from the last fetch, without typing `-q`
- `rake status` flags now resolve as flag → setting → built-in default, so an
  explicit flag always overrides the file. `-l -C` together is now reported as
  contradictory rather than silently resolved.
- An unreadable entry in the settings file no longer costs you the whole file. The
  document is read key by key: the broken entry falls back to the built-in default,
  the readable ones still apply, and each substitution is reported — on stderr for
  every command, since a quietly replaced default looks exactly like a preference
  that did nothing. A file that is not valid JSON costs every setting and is
  reported with its line and column. `rake settings edit` validates what you saved
  and exits non-zero if part of it is unusable.
- `rake status --quiet` (`-q`) suppresses informational notes. Warnings and
  errors are unaffected, so an out-of-date or uncheckable bucket is still
  reported.

### Fixed

- The bootstrap no longer closes the caller's terminal. The documented one-liner ran
  the script through `powershell -Command`, and inside that scope five `exit 1` calls
  ended the user's shell instead of printing why — a refused install, or a checksum
  mismatch, took the window down with it and printed nothing. Those exits are now
  exceptions that reach the caller; `powershell -Command` still returns 1, so
  `$LASTEXITCODE` checks keep working. The one-liner also no longer leaks eight
  helper functions and a `$Source` variable into the session.
- `rake self update` no longer downgrades. It asked GitHub for the newest *finished*
  release, so anyone who had installed a pre-release was silently moved back to the
  previous version on their next update — a pre-release is newer than anything that
  endpoint will ever return. It now takes the maximum by version across the
  published releases and refuses to install anything older than what is running.
  A finished release supersedes the pre-release it followed, as usual.
- A download that returned an HTTP error no longer wrote the error page into the cache
  under the archive's name and reported success. A dead mirror is routine, and the
  corruption used to surface much later as a broken archive.
- The final download progress event is no longer dropped when the event bus is full. It
  is the event carrying `downloaded == total`, which is what ends the progress display,
  so a busy channel left the bar spinning on a download that had actually finished.
- `rake bucket add` and `rake bucket remove` reject a bucket name containing `..` or an
  absolute path. The name was joined onto `<root>/buckets` unconstrained, so
  `rake bucket remove ..\apps` would have removed an arbitrary directory.
- `rake update` no longer prints "Everything is up to date!" when it just failed. It
  did so unconditionally, after the progress display had already shown the failures.
- `rake update` reports what happened to each bucket as plain lines. The results were
  delivered only through the progress display, which hides itself entirely when the
  output is not a terminal — so redirecting the command or piping it to a file produced
  no bucket names and no failures at all.
- Buckets that `rake update` declines to fetch are now named. A held bucket or a
  directory that is not a git repository used to be skipped with no message, so the
  command reported success over a tree it had deliberately left stale. A held bucket
  also says how to release it.
- `rake update` exits non-zero when a bucket fails, so a script running it notices.
- `rake cache rm` no longer reports files it failed to delete. The count was taken
  before anything was removed, so a cache file held open by another process was still
  announced as removed.
- Cache sidecar `.txt` files are no longer listed or counted as cache entries of their
  own. Their names contain `#`, so they parsed as entries and every archive with a
  sidecar was reported twice.
- `rake self update` no longer silently loses the previous binary when a failed
  update cannot be rolled back. The old executable is reported at the temporary path
  it is still sitting at, instead of the error being discarded.
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

- 230 new tests, most of them covering behaviour that had none: manifest parsing and
  architecture fallback, the install-record format on disk, persist data safety,
  archive extraction guards, `cleanup`, `uninstall`, the PowerShell quoting
  rules, settings loading, bucket classification, release selection, HTTP
  downloads and `checkup`.
- `validate_relative_path` is now the single place that rejects a manifest-supplied
  path which could escape its directory. `bin`, `persist`, shortcuts and
  `env_add_path` all go through it; three divergent copies of the check existed.
  `rake bucket add`/`remove` go through it too.
- Each `checkup` verdict was split from the system probe that feeds it, so severity,
  wording and the presence of advice are testable without touching the machine.
- Release selection for `rake self` is a pure function over the published list. It
  takes the maximum by version rather than trusting the API's order, which says
  nothing about version order once a release is re-cut.
- The release workflow compares the tag against `crates/rake-cli/Cargo.toml` — the
  crate that actually produces `rake.exe`. It used to read the workspace root,
  where the only version on offer was a `[workspace] version` key that Cargo does
  not read at all. Clippy is now clean with no exceptions.
- `settings.rs` validated its sections with a one-element loop, a placeholder that
  read as "more coming" without saying which. It is a function over a section name
  now, so the fallback wording follows the name instead of being a literal repeated
  per section — which is how a copy-paste would have told the user their `status`
  defaults were replaced when the problem was elsewhere.
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
