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

use std::path::{Path, PathBuf};

use rake_domain::package::PackageIdent;
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

/// What an install or update actually did, so the CLI can report it accurately.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Where the executable now lives.
    pub exe: PathBuf,
    /// Whether the PATH entry had to be created. Repeated runs are a no-op.
    pub path_added: bool,
    /// The previous binary, parked under a `.old` name and swept on the next run.
    pub stale: Option<PathBuf>,
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

/// Fetch the newest release for this architecture, verify it, and extract the binary.
async fn fetch_release_payload(session: &Session) -> Result<Payload> {
    let asset = release_asset_name()?;
    let api = format!("https://api.github.com/repos/{REPO}/releases/latest");

    let body = session.http_client().get_text(&api).await?;
    let release: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| crate::Error::Download(format!("parse release JSON: {e}")))?;

    let tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| crate::Error::Download("release has no tag_name".into()))?;

    let url = release["assets"]
        .as_array()
        .and_then(|assets| {
            assets
                .iter()
                .find(|a| a["name"].as_str() == Some(asset.as_str()))
        })
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| crate::Error::Download(format!("release {tag} has no asset named {asset}")))?
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

    Ok(Payload {
        exe,
        _extracted_from: Some(extracted),
    })
}

/// Resolve the binary to install: a local build, or a verified release.
async fn resolve_payload(session: &Session, local: Option<&Path>) -> Result<Payload> {
    let Some(path) = local else {
        return fetch_release_payload(session).await;
    };

    if !path.is_file() {
        return Err(crate::Error::Custom(format!(
            "local binary not found: {}",
            path.display()
        )));
    }
    Ok(Payload {
        exe: path.to_path_buf(),
        _extracted_from: None,
    })
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
            if replaced {
                let _ = std::fs::rename(&stale, dest);
            }
            Err(crate::Error::Io(e))
        }
    }
}

/// Install Rake, creating the layout if it is not there yet.
pub async fn install(session: &Session, local: Option<&Path>) -> Result<InstallOutcome> {
    // Fetching and verifying happens before the lock is taken: it is the slow part,
    // touches nothing on disk, and holding a lock across an await would block every
    // other operation for the duration of the download.
    let payload = resolve_payload(session, local).await?;

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
    })
}

/// Update an existing Rake installation.
pub async fn update(session: &Session, local: Option<&Path>) -> Result<InstallOutcome> {
    let dest = exe_path()?;
    if !dest.is_file() {
        return Err(crate::Error::Custom(format!(
            "Rake is not installed at {}. Run 'rake self install' first.",
            dest.display()
        )));
    }

    let payload = resolve_payload(session, local).await?;

    let _guard = session.write_lock()?;
    let bin = bin_dir()?;
    let stale = replace_binary(&payload.exe, &dest)?;

    // A Rake installed before PATH handling existed may still be missing from it.
    let path_added = env::add_user_path(&bin)?;

    Ok(InstallOutcome {
        exe: dest,
        path_added,
        stale,
    })
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
}
