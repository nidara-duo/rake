//! Installing, updating and removing Rake's own binary.
//!
//! This is deliberately not PowerShell. Manifest scripts (`pre_install`, `install`,
//! `post_install`, …) are authored in PowerShell by bucket maintainers and always run
//! through PowerShell — see [`crate::infra::script`]. Rake's own lifecycle, however, is
//! our code, and keeping it in the same language as the rest of the engine is what makes
//! it testable. The previous PowerShell implementation had no tests at all and shipped
//! three silent-failure bugs.
//!
//! Replacing a running binary follows the scheme Scoop uses for its own update: rename
//! the current directory to `old`, swap in the new one, and remove `old` on the next run.
//! A single file cannot be handled quite the same way, since there is no surrounding
//! directory to rename, but the shape carries over — see
//! [`crate::infra::self_replace`].
//!
//! Operations here return facts rather than printing: the CLI owns presentation, and
//! `tracing` has no subscriber configured, so anything logged would be invisible.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use rake_domain::package::PackageIdent;
use rake_domain::version::compare_versions;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::Result;
use crate::infra::archive::{ArchiveFormat, ArchiveService, NativeArchive};
use crate::infra::env;
use crate::infra::fs;
use crate::infra::self_replace;
use crate::session::Session;

/// Repository that release assets are pulled from.
pub const REPO: &str = "nidara-duo/rake";

/// Directory holding the Rake binary and nothing else.
///
/// Distinct from the Scoop-style package root (`~/rake`), which holds `apps`, `cache`
/// and `buckets` and is deliberately left alone when Rake uninstalls itself.
pub fn install_root() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        crate::Error::Custom("LOCALAPPDATA is not set; cannot locate the Rake install".into())
    })?;
    Ok(PathBuf::from(local).join("rake"))
}

/// Directory holding the Rake executable.
pub fn bin_dir() -> Result<PathBuf> {
    Ok(install_root()?.join("bin"))
}

/// Path of the installed Rake executable.
pub fn exe_path() -> Result<PathBuf> {
    Ok(bin_dir()?.join("rake.exe"))
}

/// Which releases the user is willing to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReleaseChannel {
    /// Finished releases only. A prerelease is never chosen.
    #[default]
    Stable,
    /// Prereleases as well, when they are newer than every finished release.
    PreRelease,
}

/// A published release, reduced to the few fields selection needs.
///
/// GitHub returns the whole release object, including bodies and asset lists, which is a
/// lot of data to carry around for a comparison. Parsing it into this shape is also what
/// makes selection testable without a network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCandidate {
    /// The tag as GitHub reports it, `v0.1.4-alpha.2`.
    pub tag: String,
    /// The version with any leading `v` removed: `0.1.4-alpha.2`.
    pub version: String,
    /// Whether this counts as a prerelease. See [`ReleaseCandidate::from_json`].
    pub prerelease: bool,
    /// Drafts are not published and must never be installed.
    pub draft: bool,
}

impl ReleaseCandidate {
    /// Read one entry of GitHub's release list.
    ///
    /// A release counts as a prerelease when *either* signal says so. The two can
    /// disagree, because the semver suffix is what actually decides the ordering while the
    /// `prerelease` field is a checkbox the release workflow fills in by matching the tag
    /// for a `-`. Trusting only the flag would let a mistagged prerelease sort as a
    /// finished release; trusting only the version would work today but depend on every
    /// future tag being spelled correctly.
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let tag = value["tag_name"].as_str()?.to_owned();
        let version = tag.strip_prefix(['v', 'V']).unwrap_or(&tag).to_owned();
        let flagged = value["prerelease"].as_bool().unwrap_or(false);

        Some(Self {
            tag,
            prerelease: flagged || has_prerelease_part(&version),
            version,
            draft: value["draft"].as_bool().unwrap_or(false),
        })
    }
}

/// Whether a version carries a `-something` suffix after its release numbers.
///
/// Build metadata is not a prerelease marker: `1.0.0+linux` is the finished 1.0.0 built
/// differently, and treating it as a prerelease would hold back a stable release.
fn has_prerelease_part(version: &str) -> bool {
    let without_build = version.split('+').next().unwrap_or(version);
    without_build
        .split_once('-')
        .is_some_and(|(_, pre)| !pre.is_empty())
}

/// Why no release was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoUpgrade {
    /// The installed version is already the newest one this run would accept.
    UpToDate,
    /// A newer prerelease exists, but this run asked for finished releases only.
    ///
    /// Reported separately from [`NoUpgrade::UpToDate`] because the two demand different
    /// answers from the user. "You are up to date" closes the question; "a 0.1.4-alpha.2
    /// exists, pass --pre-release to take it" leaves it open.
    PrereleaseHeldBack { newest: String },
}

/// Pick the release to install.
///
/// Three rules, in the order their violation would surprise most:
///
/// * drafts are never installable — they are not published yet;
/// * a prerelease is only eligible on [`ReleaseChannel::PreRelease`];
/// * nothing older than `current` is chosen, so an update never downgrades.
///
/// The last rule is not defensive padding, it is the fix for a real hazard. GitHub's
/// `/releases/latest` endpoint and "newest finished release" are not the same thing once a
/// prerelease exists: someone who installed 0.1.4-alpha.2 and then ran a plain update
/// would otherwise be silently moved back to 0.1.3, because 0.1.4-alpha.2 is newer than
/// everything that endpoint will ever return. Refusing the downgrade and naming the
/// prerelease is both safer and more useful than installing something older on request.
///
/// The list is not assumed to be sorted. Selection takes the maximum by version rather
/// than trusting the order the API returned.
pub fn select_release<'a>(
    releases: &'a [ReleaseCandidate],
    channel: ReleaseChannel,
    current: Option<&str>,
) -> std::result::Result<&'a ReleaseCandidate, NoUpgrade> {
    let mut best: Option<&ReleaseCandidate> = None;

    for candidate in releases {
        if candidate.draft {
            continue;
        }
        if candidate.prerelease && channel == ReleaseChannel::Stable {
            continue;
        }
        if let Some(current) = current
            && compare_versions(&candidate.version, current) != Ordering::Greater
        {
            continue;
        }

        let better = match best {
            None => true,
            Some(incumbent) => {
                compare_versions(&candidate.version, &incumbent.version) == Ordering::Greater
            }
        };
        if better {
            best = Some(candidate);
        }
    }

    match best {
        Some(found) => Ok(found),
        None => Err(match newest_prerelease_above(releases, current) {
            Some(newer) => NoUpgrade::PrereleaseHeldBack {
                newest: newer.version.clone(),
            },
            None => NoUpgrade::UpToDate,
        }),
    }
}

/// The highest prerelease above `current`, ignoring any release channel.
fn newest_prerelease_above<'a>(
    releases: &'a [ReleaseCandidate],
    current: Option<&str>,
) -> Option<&'a ReleaseCandidate> {
    releases
        .iter()
        .filter(|c| !c.draft && c.prerelease)
        .filter(|c| {
            current.is_none_or(|current| compare_versions(&c.version, current) == Ordering::Greater)
        })
        // The tag breaks ties so the answer does not depend on the API's ordering when two
        // entries carry the same version, which happens after a release is re-cut.
        .max_by(|a, b| compare_versions(&a.version, &b.version).then_with(|| a.tag.cmp(&b.tag)))
}

/// What an install actually did, so the CLI can report it accurately.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Where the executable now lives.
    pub exe: PathBuf,
    /// Whether the PATH entry had to be created. Repeated runs are a no-op.
    pub path_added: bool,
    /// The previous binary, parked under a `.old` name and swept on the next run.
    pub stale: Option<PathBuf>,
    /// The version taken from the release, when one was downloaded. `None` for `--local`,
    /// where the binary's own version is not knowable from here.
    pub version: Option<String>,
}

/// What an update did.
///
/// Not folded into [`InstallOutcome`] on purpose. An update that finds nothing newer has
/// not installed anything, and returning an outcome with a binary path in it would invite
/// the CLI into printing "updated" — the same class of bug this module has already fixed
/// once, where a failed update reported success.
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// A newer version was installed.
    Updated(InstallOutcome),
    /// Nothing was installed. The reason travels with it so the CLI can say which, rather
    /// than printing one generic "already up to date" for both.
    Unchanged(NoUpgrade),
}

/// What an uninstall actually did.
#[derive(Debug, Clone)]
pub struct UninstallOutcome {
    /// The renamed binary, if one was running and therefore had to be deferred.
    pub deferred: Option<PathBuf>,
    /// Whether a PATH entry was present and removed.
    pub path_removed: bool,
}

/// Map a Rust architecture name onto the target triple the release workflow builds.
///
/// `x86` is published as `i686` and `aarch64` as `aarch64`, matching the matrix in
/// `.github/workflows/release.yml`.
fn target_triple(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("x86_64-pc-windows-msvc"),
        "x86" => Some("i686-pc-windows-msvc"),
        "aarch64" => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

/// Release asset name for the architecture this binary was built for.
fn release_asset_name() -> Result<String> {
    let triple = target_triple(std::env::consts::ARCH).ok_or_else(|| {
        crate::Error::Custom(format!(
            "no Rake release is published for architecture '{}'",
            std::env::consts::ARCH
        ))
    })?;
    Ok(format!("rake-{triple}.zip"))
}

/// Extract the leading hex hash from a `<sha256>  <file>` checksum line.
///
/// Release checksum assets are written in `sha256sum` format, which appends the file
/// name; a bare hash is accepted too since some tooling emits that shape.
fn parse_checksum(text: &str) -> Option<&str> {
    let token = text.split_whitespace().next()?;
    (token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit())).then_some(token)
}

/// The binary to install, plus the temporary directory holding it, if any.
///
/// The directory is kept alive by this struct: dropping a `TempDir` deletes its
/// contents, so the extracted executable would disappear mid-install.
struct Payload {
    exe: PathBuf,
    _extracted_from: Option<tempfile::TempDir>,
}

/// Every published release, newest first as GitHub orders them.
///
/// The list endpoint rather than `/releases/latest`, because the latter is defined as "the
/// newest finished release" and cannot express a prerelease at all. Ordering is not
/// trusted — [`select_release`] takes the maximum by version — so only the contents matter.
async fn fetch_releases(session: &Session) -> Result<Vec<ReleaseCandidate>> {
    // 100 is the API's per-page maximum and comfortably more releases than this project
    // will accumulate; going further would need pagination and the extra failure mode that
    // comes with it.
    let api = format!("https://api.github.com/repos/{REPO}/releases?per_page=100");

    let body = session.http_client().get_text(&api).await?;
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| crate::Error::Download(format!("parse release list JSON: {e}")))?;

    let items = json
        .as_array()
        .ok_or_else(|| crate::Error::Download("release list is not an array".into()))?;

    Ok(items
        .iter()
        .filter_map(ReleaseCandidate::from_json)
        .collect())
}

/// Fetch the full release object, which carries the asset list the list endpoint omits.
async fn fetch_release_by_tag(session: &Session, tag: &str) -> Result<serde_json::Value> {
    let api = format!("https://api.github.com/repos/{REPO}/releases/tags/{tag}");

    let body = session.http_client().get_text(&api).await?;
    serde_json::from_str(&body)
        .map_err(|e| crate::Error::Download(format!("parse release JSON for {tag}: {e}")))
}

/// The download URL of one named asset on a release.
fn asset_url<'a>(release: &'a serde_json::Value, asset: &str) -> Option<&'a str> {
    release["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(asset))?
        .get("browser_download_url")?
        .as_str()
}

/// Fetch the chosen release for this architecture, verify it, and extract the binary.
async fn fetch_release_payload(
    session: &Session,
    chosen: &ReleaseCandidate,
) -> Result<(Payload, String)> {
    let asset = release_asset_name()?;
    let release = fetch_release_by_tag(session, &chosen.tag).await?;

    let url = asset_url(&release, &asset)
        .ok_or_else(|| {
            crate::Error::Download(format!("release {} has no asset named {asset}", chosen.tag))
        })?
        .to_string();

    let staging = tempfile::Builder::new()
        .prefix("rake-self-")
        .tempdir()
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("temp dir: {e}"))))?;
    let archive = staging.path().join(&asset);

    session
        .http_client()
        .download(
            &url,
            &archive,
            PackageIdent::new("", "rake"),
            // No progress channel: `rake self` does not run the event consumer, so the
            // events would only be dropped once the buffer filled.
            None,
        )
        .await?;

    // A release without a usable checksum is not something to install blindly.
    let checksum_url = format!("{url}.sha256");
    let expected = session
        .http_client()
        .get_text(&checksum_url)
        .await
        .and_then(|text| {
            parse_checksum(&text)
                .map(str::to_string)
                .ok_or_else(|| crate::Error::Download("no sha256 in checksum file".into()))
        })
        .map_err(|e| match e {
            crate::Error::Download(m) => crate::Error::Download(format!("{m} at {checksum_url}")),
            other => other,
        })?;

    crate::operations::download::verify_file_hash(&archive, &expected)?;

    let extracted = tempfile::Builder::new()
        .prefix("rake-self-extract-")
        .tempdir()
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("temp dir: {e}"))))?;

    NativeArchive::new(None)
        .extract(&archive, extracted.path(), ArchiveFormat::Zip)
        .await?;

    let exe = extracted.path().join("rake.exe");
    if !exe.is_file() {
        return Err(crate::Error::Archive(
            "release archive did not contain rake.exe at its root".into(),
        ));
    }

    Ok((
        Payload {
            exe,
            _extracted_from: Some(extracted),
        },
        chosen.version.clone(),
    ))
}

/// Why resolving the payload did not produce one.
///
/// Kept apart from [`crate::Error`] because "there is nothing to install" is not a
/// failure for an update — it is the normal answer when already current, and it carries
/// the prerelease the user could opt into. Collapsing it into an error string would throw
/// that away and force the CLI to re-derive the reason from prose.
enum ResolveError {
    /// The lookup or download itself failed.
    Fetch(crate::Error),
    /// Nothing eligible was found, and why.
    Nothing(NoUpgrade),
}

/// Resolve the binary to install: a local build, or a verified release.
///
/// `current` is the version of the running binary, or `None` when there is nothing
/// installed to compare against. It is what stops an update from moving to an older
/// version; see [`select_release`].
async fn resolve_payload(
    session: &Session,
    local: Option<&Path>,
    channel: ReleaseChannel,
    current: Option<&str>,
) -> std::result::Result<(Payload, Option<String>), ResolveError> {
    let Some(path) = local else {
        let releases = fetch_releases(session).await.map_err(ResolveError::Fetch)?;
        let chosen = select_release(&releases, channel, current).map_err(ResolveError::Nothing)?;
        let (payload, version) = fetch_release_payload(session, chosen)
            .await
            .map_err(ResolveError::Fetch)?;
        return Ok((payload, Some(version)));
    };

    if !path.is_file() {
        return Err(ResolveError::Fetch(crate::Error::Custom(format!(
            "local binary not found: {}",
            path.display()
        ))));
    }
    // `None`, not `CARGO_PKG_VERSION`: that describes the running binary, not the file the
    // user pointed at, which is very often not the same thing.
    Ok((
        Payload {
            exe: path.to_path_buf(),
            _extracted_from: None,
        },
        None,
    ))
}

/// Free the stale-name slot, retrying while something transiently holds it.
///
/// The usual cause is an antivirus scanner that grabbed the binary moments ago, and a
/// brief wait clears it. Whatever still holds the file after that is a real conflict,
/// and is reported rather than worked around.
fn clear_stale_name(stale: &Path) -> Result<()> {
    for attempt in 1..=5 {
        if !stale.exists() {
            return Ok(());
        }
        if std::fs::remove_file(stale).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200 * attempt));
    }

    if stale.exists() {
        return Err(crate::Error::Custom(format!(
            "cannot replace {} because another process is using it. Close any running Rake \
             (or wait for an antivirus scan to finish) and try again.",
            stale.display()
        )));
    }
    Ok(())
}

/// Replace the executable at `dest` with `src`, renaming the old one out of the way.
///
/// Windows locks a running image, so `dest` cannot be overwritten while this process
/// runs; renaming it aside is permitted and frees the name. On failure the previous
/// binary is put back, so a failed update never leaves Rake uninstalled.
fn replace_binary(src: &Path, dest: &Path) -> Result<Option<PathBuf>> {
    let stale = self_replace::stale_path_for(dest).ok_or_else(|| {
        crate::Error::Custom("cannot derive a temporary name for the binary".into())
    })?;

    clear_stale_name(&stale)?;

    let replaced = dest.exists();
    if replaced {
        std::fs::rename(dest, &stale)?;
    }

    match std::fs::copy(src, dest) {
        Ok(_) => Ok(replaced.then_some(stale)),
        Err(e) => {
            if replaced && let Err(rollback) = std::fs::rename(&stale, dest) {
                // The rollback failing is the worst outcome here: the old binary has
                // been renamed away and the new one did not land, so there is no
                // executable at `dest` at all. The previous code discarded this error,
                // while the doc comment above promised "a failed update never leaves
                // Rake uninstalled". Say where the old binary actually is, because it is
                // still on disk under its temporary name and is otherwise unfindable.
                return Err(crate::Error::Custom(format!(
                    "copying the new binary failed ({e}), and restoring the previous one \
                     failed as well ({rollback}). The previous binary is still at {}",
                    stale.display()
                )));
            }
            Err(crate::Error::Io(e))
        }
    }
}

/// The version of the running binary.
///
/// Every crate in the workspace carries the same version and the release workflow refuses
/// to publish a tag that disagrees with `Cargo.toml`, so this matches the tag this binary
/// was cut from.
pub fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Install Rake, creating the layout if it is not there yet.
pub async fn install(
    session: &Session,
    local: Option<&Path>,
    channel: ReleaseChannel,
) -> Result<InstallOutcome> {
    // Fetching and verifying happens before the lock is taken: it is the slow part,
    // touches nothing on disk, and holding a lock across an await would block every
    // other operation for the duration of the download.
    //
    // `None` for `current` — there is nothing installed to be older than, so an install
    // takes the newest eligible release outright.
    //
    // Unlike an update, "nothing to install" is an error here: the user asked for an
    // install, and there is nothing to put on disk.
    let (payload, version) = resolve_payload(session, local, channel, None)
        .await
        .map_err(|e| match e {
            ResolveError::Fetch(e) => e,
            ResolveError::Nothing(NoUpgrade::UpToDate) => {
                crate::Error::Custom("no published release to install".into())
            }
            ResolveError::Nothing(NoUpgrade::PrereleaseHeldBack { newest }) => {
                crate::Error::Custom(format!(
                    "only a pre-release is available ({newest}). Pass --pre-release to take it."
                ))
            }
        })?;

    let _guard = session.write_lock()?;
    let bin = bin_dir()?;
    let dest = exe_path()?;

    fs::ensure_dir(&bin)?;
    let stale = replace_binary(&payload.exe, &dest)?;

    // Idempotent: repeated installs must not stack duplicate entries.
    let path_added = env::add_user_path(&bin)?;

    Ok(InstallOutcome {
        exe: dest,
        path_added,
        stale,
        version,
    })
}

/// Update an existing Rake installation.
pub async fn update(
    session: &Session,
    local: Option<&Path>,
    channel: ReleaseChannel,
) -> Result<UpdateOutcome> {
    let dest = exe_path()?;
    if !dest.is_file() {
        return Err(crate::Error::Custom(format!(
            "Rake is not installed at {}. Run 'rake self install' first.",
            dest.display()
        )));
    }

    // `--local` is an explicit instruction and is never second-guessed: if the user points
    // at a binary, that binary is what gets installed, whatever its version says. That is
    // also why `current` is `None` there — comparing against the running version would
    // reject the user's own binary.
    let current = local.is_none().then(running_version);

    // Fetching before the lock, for the same reason as in `install`.
    let (payload, version) = match resolve_payload(session, local, channel, current).await {
        Ok(resolved) => resolved,
        // Nothing newer is the normal outcome of running an up-to-date copy, not a failure.
        Err(ResolveError::Nothing(why)) => return Ok(UpdateOutcome::Unchanged(why)),
        Err(ResolveError::Fetch(e)) => return Err(e),
    };

    let _guard = session.write_lock()?;
    let bin = bin_dir()?;
    let stale = replace_binary(&payload.exe, &dest)?;

    // A Rake installed before PATH handling existed may still be missing from it.
    let path_added = env::add_user_path(&bin)?;

    Ok(UpdateOutcome::Updated(InstallOutcome {
        exe: dest,
        path_added,
        stale,
        version,
    }))
}

/// Remove Rake, its PATH entry and its directories.
///
/// The binary is running, so it is renamed aside and a detached batch file finishes the
/// deletion once this process exits. Sweeping on the next startup — the approach that
/// works for update — is not available here, because uninstalling Rake also removes
/// whatever would have done the sweeping.
pub fn uninstall(session: &Session) -> Result<UninstallOutcome> {
    let _guard = session.write_lock()?;
    let root = install_root()?;
    let bin = bin_dir()?;
    let dest = exe_path()?;

    let mut deferred = None;
    if dest.is_file() {
        let stale = self_replace::stale_path_for(&dest)
            .ok_or_else(|| crate::Error::Custom("cannot derive a temporary name".into()))?;
        clear_stale_name(&stale)?;
        std::fs::rename(&dest, &stale)?;
        deferred = Some(stale);
    }

    // Rake used to install itself with a PowerShell bootstrap script that it needed for
    // later self-updates. Nothing needs it now, but installations made before this change
    // still carry it, so clear it out rather than leaving it behind forever.
    let legacy_bootstrap = bin.join("bootstrap.ps1");
    if legacy_bootstrap.is_file() {
        let _ = std::fs::remove_file(&legacy_bootstrap);
    }

    // Reported apart from the deletion: the PATH entry is gone from the registry now,
    // even though the file itself is still finishing removal.
    let path_removed = env::remove_user_path(&bin)?;

    match &deferred {
        Some(stale) => schedule_cleanup(stale, &bin, &root)?,
        // Nothing was running, so nothing is locked and cleanup can finish here.
        None => {
            remove_empty_dir(&bin);
            remove_empty_dir(&root);
        }
    }

    Ok(UninstallOutcome {
        deferred,
        path_removed,
    })
}

/// Hide the console window this batch file would otherwise flash.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Delete `dir` when nothing is left in it.
fn remove_empty_dir(dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    let empty = std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    if empty {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Spawn the batch file that deletes the renamed binary after this process exits.
///
/// Written as a file rather than a one-line `cmd /c` because a command line mixing a
/// `for` loop with several `if ... ( ... )` blocks mis-parses. The script removes itself
/// as its last act.
fn schedule_cleanup(stale: &Path, bin: &Path, root: &Path) -> Result<()> {
    const TEMPLATE: &str = r#"@echo off
setlocal
set "OLD=%OLD%"
set "BIN=%BIN%"
set "ROOT=%ROOT%"

rem Wait for the owning rake.exe to exit, then delete the binary it renamed aside.
for /L %%i in (1,1,30) do (
    if exist "%OLD%" (
        ping -n 2 127.0.0.1 >nul
        del /f /q "%OLD%" >nul 2>&1
    )
)

call :prune "%BIN%"
call :prune "%ROOT%"
del /f /q "%~f0"
exit /b 0

:prune
if not exist "%~1" exit /b 0
rem "dir /b" prints one line per entry, so any output means the directory is not empty.
rem Note that "if not exist <dir>\*" is not a valid emptiness test — it matches the
rem directory itself even when empty — hence this loop.
for /f %%i in ('dir /b /a "%~1" 2^>nul') do exit /b 0
rmdir "%~1" >nul 2>&1
exit /b 0
"#;

    let script = std::env::temp_dir().join(format!("rake-cleanup-{}.cmd", std::process::id()));
    let body = TEMPLATE
        .replace("%OLD%", &stale.display().to_string())
        .replace("%BIN%", &bin.display().to_string())
        .replace("%ROOT%", &root.display().to_string())
        // cmd.exe parses batch files line by line and expects CRLF. With bare LF it
        // mis-reads multi-line blocks such as the `for` loop below, silently skipping
        // everything after them.
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");

    std::fs::write(&script, body)
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("write cleanup: {e}"))))?;

    let mut cmd = std::process::Command::new("cmd");
    cmd.arg("/c")
        // The script is cmd syntax, not a C runtime argument list: escaping its quotes
        // the usual way would hand cmd backslashes it does not understand.
        .raw_arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    // Dropping the handle without waiting detaches the child: it keeps running after this
    // process exits, which is exactly what the deferred deletion needs.
    drop(cmd.spawn()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_sha256sum_style_line() {
        let hash = "a".repeat(64);
        assert_eq!(
            parse_checksum(&format!("{hash}  rake-x86_64.zip\n")),
            Some(hash.as_str())
        );
    }

    #[test]
    fn accepts_a_bare_hash() {
        let hash = "0123456789abcdef".repeat(4);
        assert_eq!(parse_checksum(&hash), Some(hash.as_str()));
    }

    #[test]
    fn rejects_anything_that_is_not_a_sha256() {
        assert_eq!(parse_checksum("not a hash at all"), None);
        // 40 hex characters is a sha1, not the sha256 a release publishes.
        assert_eq!(parse_checksum(&"a".repeat(40)), None);
        assert_eq!(parse_checksum(""), None);
    }

    #[test]
    fn maps_architectures_onto_the_published_target_triples() {
        assert_eq!(target_triple("x86_64"), Some("x86_64-pc-windows-msvc"));
        assert_eq!(target_triple("x86"), Some("i686-pc-windows-msvc"));
        assert_eq!(target_triple("aarch64"), Some("aarch64-pc-windows-msvc"));
        assert_eq!(target_triple("mips"), None);
    }

    /// Guards the coupling with the release workflow's `Compress-Archive` step.
    #[test]
    fn release_asset_is_named_after_the_target_triple() {
        let triple = target_triple(std::env::consts::ARCH).expect("host arch is published");
        assert_eq!(release_asset_name().unwrap(), format!("rake-{triple}.zip"));
    }

    // ---- Release selection --------------------------------------------------------
    //
    // The scenarios below are the ones that actually decide what a user ends up running,
    // so they are spelled out as histories rather than as properties of a comparator.

    /// Build a candidate the way the parser would, from just a tag.
    fn rel(tag: &str) -> ReleaseCandidate {
        let version = tag.strip_prefix(['v', 'V']).unwrap_or(tag).to_owned();
        ReleaseCandidate {
            tag: tag.to_owned(),
            prerelease: has_prerelease_part(&version),
            version,
            draft: false,
        }
    }

    /// The tag shape the release workflow produces: a `v` prefix, and `-anything` meaning
    /// prerelease.
    #[test]
    fn a_tag_with_a_dash_is_a_prerelease() {
        assert!(rel("v0.1.4-alpha.2").prerelease);
        assert!(!rel("v0.1.3").prerelease);
        assert_eq!(rel("v0.1.4-alpha.2").version, "0.1.4-alpha.2");
    }

    /// Build metadata is not a prerelease marker. `1.0.0+linux` is the finished 1.0.0, and
    /// treating it as a prerelease would hold back a stable release forever.
    #[test]
    fn build_metadata_does_not_make_a_prerelease() {
        assert!(!has_prerelease_part("1.0.0+linux"));
        assert!(!has_prerelease_part("1.0.0"));
        assert!(has_prerelease_part("1.0.0-rc1"));
        // A trailing dash is not a prerelease name, so it is not a prerelease marker either.
        assert!(!has_prerelease_part("1.0.0-"));
    }

    /// The pre-release flag is honoured, and it also works if the tag itself is silent
    /// about it — GitHub's checkbox and the semver suffix can disagree.
    #[test]
    fn the_prerelease_flag_counts_even_without_a_dashed_tag() {
        let json = serde_json::json!({"tag_name": "v0.2.0", "prerelease": true});
        let candidate = ReleaseCandidate::from_json(&json).unwrap();
        assert!(candidate.prerelease);
        assert_eq!(candidate.version, "0.2.0");

        // And the other direction: a dashed tag is a prerelease even when the flag says no,
        // because the version is what the ordering is computed from.
        let json = serde_json::json!({"tag_name": "v0.2.0-rc1", "prerelease": false});
        assert!(ReleaseCandidate::from_json(&json).unwrap().prerelease);
    }

    #[test]
    fn drafts_are_never_installable() {
        let json = serde_json::json!({"tag_name": "v9.9.9", "draft": true});
        let draft = ReleaseCandidate::from_json(&json).unwrap();
        assert!(draft.draft);

        let releases = [draft, rel("v0.1.0")];
        for channel in [ReleaseChannel::Stable, ReleaseChannel::PreRelease] {
            let chosen = select_release(&releases, channel, None).unwrap();
            assert_eq!(
                chosen.version, "0.1.0",
                "a draft must never win, {channel:?}"
            );
        }
    }

    /// The core of the feature: on `--pre-release`, a prerelease newer than the finished
    /// release is taken.
    #[test]
    fn pre_release_takes_the_newer_prerelease() {
        let releases = [rel("v0.1.3"), rel("v0.1.4-alpha.2"), rel("v0.1.3.1")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::PreRelease, Some("0.1.3"))
                .unwrap()
                .version,
            "0.1.4-alpha.2",
            "0.1.4-alpha.2 sorts above both 0.1.3 and 0.1.3.1"
        );
    }

    /// Without the flag, the prerelease is invisible — the same answer a user got before
    /// this flag existed.
    #[test]
    fn stable_ignores_the_prerelease() {
        let releases = [rel("v0.1.3"), rel("v0.1.4-alpha.2")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::Stable, Some("0.1.2"))
                .unwrap()
                .version,
            "0.1.3",
            "0.1.3 is the newest finished release, and 0.1.4-alpha.2 is not eligible"
        );
    }

    /// The case the feature exists for, stated as the user meets it: on the newest
    /// finished release, with a prerelease sitting above it.
    #[test]
    fn being_on_the_latest_finished_release_offers_the_prerelease() {
        let releases = [rel("v0.1.3"), rel("v0.1.4-alpha.2")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::Stable, Some("0.1.3")),
            Err(NoUpgrade::PrereleaseHeldBack {
                newest: "0.1.4-alpha.2".into()
            })
        );
    }

    /// The hazard this whole design exists to prevent. Someone on 0.1.4-alpha.2 running a
    /// plain update must not be moved back to 0.1.3, which is what GitHub's
    /// `/releases/latest` would have handed over.
    #[test]
    fn a_plain_update_from_a_prerelease_never_downgrades() {
        let releases = [rel("v0.1.3"), rel("v0.1.4-alpha.2")];
        // 0.1.3 is present in the list and would be an acceptable answer for an endpoint
        // that ignored the installed version. Nothing must come back at all.
        assert_eq!(
            select_release(&releases, ReleaseChannel::Stable, Some("0.1.4-alpha.2")),
            Err(NoUpgrade::UpToDate),
            "0.1.3 is older than what is installed, so it must not be installed"
        );
    }

    /// ...and once the finished release catches up, the update proceeds normally.
    #[test]
    fn a_finished_release_supersedes_the_prerelease() {
        let releases = [rel("v0.1.3"), rel("v0.1.4")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::Stable, Some("0.1.4-alpha.2"))
                .unwrap()
                .version,
            "0.1.4",
            "0.1.4 is newer than 0.1.4-alpha.2, so the prerelease is left behind"
        );
    }

    /// The refusal has to name what is being held back, because that is the whole reason
    /// the user learns anything from it.
    #[test]
    fn the_held_back_prerelease_is_named() {
        let releases = [rel("v0.1.0"), rel("v0.1.1"), rel("v0.1.2-rc1")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::Stable, Some("0.1.1")),
            Err(NoUpgrade::PrereleaseHeldBack {
                newest: "0.1.2-rc1".into()
            })
        );
    }

    /// The same shape with the flag: now it installs.
    #[test]
    fn the_flag_resolves_the_held_back_case() {
        let releases = [rel("v0.1.0"), rel("v0.1.1"), rel("v0.1.2-rc1")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::PreRelease, Some("0.1.1"))
                .unwrap()
                .version,
            "0.1.2-rc1"
        );
    }

    /// On a stable channel a prerelease at or below the installed version is not news, so
    /// it must not be reported as something being held back.
    #[test]
    fn an_older_prerelease_is_not_held_back() {
        let releases = [rel("v0.1.3"), rel("v0.1.3-rc1")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::Stable, Some("0.1.3")),
            Err(NoUpgrade::UpToDate)
        );
    }

    #[test]
    fn an_empty_list_has_nothing_to_say() {
        assert_eq!(
            select_release(&[], ReleaseChannel::PreRelease, None),
            Err(NoUpgrade::UpToDate)
        );
    }

    /// Already on the newest eligible version, with nothing newer lurking.
    #[test]
    fn being_current_is_reported_plainly() {
        let releases = [rel("v0.1.3")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::Stable, Some("0.1.3")),
            Err(NoUpgrade::UpToDate)
        );
    }

    /// Selection takes the maximum, not the first or last entry. GitHub orders by creation
    /// date, which says nothing about version order once a release is re-cut.
    #[test]
    fn selection_does_not_trust_the_order_of_the_list() {
        let ascending = [rel("v0.1.0"), rel("v0.1.1"), rel("v0.1.2")];
        let descending = [rel("v0.1.2"), rel("v0.1.1"), rel("v0.1.0")];

        for list in [&ascending[..], &descending[..]] {
            assert_eq!(
                select_release(list, ReleaseChannel::Stable, None)
                    .unwrap()
                    .version,
                "0.1.2"
            );
        }
    }

    /// A fresh install has nothing installed, so it takes the newest eligible release
    /// outright — including a prerelease when asked.
    #[test]
    fn a_fresh_install_takes_the_newest_eligible() {
        let releases = [rel("v0.1.3"), rel("v0.1.4-alpha.2")];
        assert_eq!(
            select_release(&releases, ReleaseChannel::Stable, None)
                .unwrap()
                .version,
            "0.1.3"
        );
        assert_eq!(
            select_release(&releases, ReleaseChannel::PreRelease, None)
                .unwrap()
                .version,
            "0.1.4-alpha.2"
        );
    }

    /// Two entries carrying the same version must not make the answer depend on the API's
    /// ordering.
    #[test]
    fn equal_versions_resolve_deterministically() {
        let a = [rel("v0.1.2"), rel("v0.1.2")];
        let first = select_release(&a, ReleaseChannel::Stable, None)
            .unwrap()
            .tag
            .clone();
        let second = select_release(&a, ReleaseChannel::Stable, None)
            .unwrap()
            .tag
            .clone();
        assert_eq!(first, second);
    }

    /// The asset is looked up by exact name, so a near-miss is reported rather than
    /// downloading the wrong architecture's binary.
    #[test]
    fn an_asset_is_matched_by_exact_name() {
        let release = serde_json::json!({
            "assets": [
                {"name": "rake-i686-pc-windows-msvc.zip", "browser_download_url": "https://x/32.zip"},
                {"name": "rake-x86_64-pc-windows-msvc.zip", "browser_download_url": "https://x/64.zip"}
            ]
        });
        assert_eq!(
            asset_url(&release, "rake-x86_64-pc-windows-msvc.zip"),
            Some("https://x/64.zip")
        );
        assert_eq!(asset_url(&release, "rake-x86_64.zip"), None);
        assert_eq!(asset_url(&serde_json::json!({}), "any.zip"), None);
    }

    /// The running version is what the downgrade guard compares against, so it has to come
    /// from the build and not from anything that could be edited afterwards.
    #[test]
    fn the_running_version_comes_from_the_build() {
        assert_eq!(running_version(), env!("CARGO_PKG_VERSION"));
        assert!(!running_version().is_empty());
    }
}
