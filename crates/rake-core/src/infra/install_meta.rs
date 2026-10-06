//! Read/write access to the per-version metadata files that Scoop keeps in
//! `apps/<app>/<version>/`.
//!
//! Scoop renamed these files at some point and kept reading the old names as
//! a fallback, so both spellings exist on real machines:
//!
//! | purpose    | current Scoop       | legacy       |
//! |------------|---------------------|--------------|
//! | install    | `scoop-install.json`| `install.json` |
//! | manifest   | `scoop-manifest.json` | `manifest.json` |
//!
//! References in Scoop (`ScoopInstaller/Scoop`):
//!   * `install_info` in lib/manifest.ps1 — `scoop-install.json`, falls back to
//!     `install.json`
//!   * `installed_manifest` in lib/manifest.ps1 — `scoop-manifest.json`, falls
//!     back to `manifest.json`
//!   * `Get-InstalledVersion` in lib/versions.ps1 — globs both spellings
//!
//! Rake must read **both**, newest name first, or it silently loses every
//! package installed by a recent Scoop. Writing goes to both names so that
//! old and new Scoop can each read what Rake installed.

use std::path::Path;

use rake_domain::manifest::Manifest;
use rake_domain::package::InstallRecord;

use crate::Result;

/// Install record written by current Scoop.
pub const INSTALL_RECORD: &str = "scoop-install.json";

/// Install record written by Scoop before the rename.
pub const INSTALL_RECORD_LEGACY: &str = "install.json";

/// Installed manifest written by current Scoop.
pub const INSTALLED_MANIFEST: &str = "scoop-manifest.json";

/// Installed manifest written by Scoop before the rename.
pub const INSTALLED_MANIFEST_LEGACY: &str = "manifest.json";

/// Read the first of `names` that exists and parses.
///
/// Returns `Ok(None)` when none of the files exist, and an error when a file
/// exists but cannot be parsed — callers that iterate over every installed app
/// should treat that as "skip this app", not "abort the whole listing".
fn read_first<T: serde::de::DeserializeOwned>(dir: &Path, names: &[&str]) -> Result<Option<T>> {
    for name in names {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let content = crate::infra::json::read_to_string(&path)?;
        match serde_json::from_str(&content) {
            Ok(value) => return Ok(Some(value)),
            Err(e) => {
                tracing::warn!("{} is not valid JSON: {e}", path.display());
                return Err(crate::Error::Serde(e));
            }
        }
    }
    Ok(None)
}

fn write_all<T: serde::Serialize>(dir: &Path, names: &[&str], value: &T) -> Result<()> {
    crate::infra::fs::ensure_dir(dir)?;
    let json = serde_json::to_string_pretty(value)?;
    for name in names {
        std::fs::write(dir.join(name), format!("{json}\n"))?;
    }
    Ok(())
}

/// Read `InstallRecord` for a version directory, or `current`.
pub fn read_install_record(dir: &Path) -> Result<Option<InstallRecord>> {
    read_first(dir, &[INSTALL_RECORD, INSTALL_RECORD_LEGACY])
}

/// Read the installed `Manifest` for a version directory, or `current`.
pub fn read_installed_manifest(dir: &Path) -> Result<Option<Manifest>> {
    read_first(dir, &[INSTALLED_MANIFEST, INSTALLED_MANIFEST_LEGACY])
}

/// Write `InstallRecord` under both spellings.
///
/// There must be exactly one writer for these files; everything else should
/// preserve the existing record via read-modify-write (see the
/// `InstallRecord` doc comment in `rake-domain`).
pub fn write_install_record(dir: &Path, record: &InstallRecord) -> Result<()> {
    write_all(dir, &[INSTALL_RECORD, INSTALL_RECORD_LEGACY], record)
}

/// Write the installed manifest under both spellings.
pub fn write_installed_manifest(dir: &Path, manifest: &Manifest) -> Result<()> {
    write_all(
        dir,
        &[INSTALLED_MANIFEST, INSTALLED_MANIFEST_LEGACY],
        manifest,
    )
}

/// Whether a version directory already looks installed.
///
/// Scoop treats an app as installed when either metadata file is present, so
/// a half-written install (one file only) is still "installed" and must not be
/// silently overwritten.
pub fn is_installed(dir: &Path) -> bool {
    dir.join(INSTALL_RECORD).is_file()
        || dir.join(INSTALL_RECORD_LEGACY).is_file()
        || dir.join(INSTALLED_MANIFEST).is_file()
        || dir.join(INSTALLED_MANIFEST_LEGACY).is_file()
}

/// Best-effort version for a version directory whose manifest is missing or
/// unreadable.
///
/// Mirrors Scoop's `Select-CurrentVersion` fallback chain: read the version
/// from the manifest, and when that is unavailable take the junction target's
/// directory name (which matters for `nightly` builds), then the newest
/// version directory by modification time.
pub fn fallback_version(app_dir: &Path, current_dir: &Path) -> Option<String> {
    if let Ok(target) = std::fs::read_link(current_dir)
        && let Some(leaf) = target.file_name().map(|n| n.to_string_lossy().to_string())
    {
        return Some(leaf);
    }

    let mut candidates: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(app_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name != "current" && !name.starts_with('_')
        })
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.file_name().to_string_lossy().to_string()))
        })
        .collect();

    candidates.sort();
    candidates.last().map(|(_, name)| name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_record() -> InstallRecord {
        InstallRecord {
            version: "1.2.3".to_owned(),
            bucket: Some("main".to_owned()),
            arch: "64bit".to_owned(),
            held: false,
            url: None,
        }
    }

    fn sample_manifest() -> Manifest {
        serde_json::from_str(r#"{"version":"1.2.3","description":"d"}"#).unwrap()
    }

    #[test]
    fn reads_new_install_record_name() {
        let dir = tempdir().unwrap();
        write_install_record(dir.path(), &sample_record()).unwrap();
        let read = read_install_record(dir.path()).unwrap().unwrap();
        assert_eq!(read.version, "1.2.3");
        assert_eq!(read.bucket.as_deref(), Some("main"));
    }

    #[test]
    fn reads_legacy_install_record_name() {
        let dir = tempdir().unwrap();
        let json = serde_json::to_string(&sample_record()).unwrap();
        std::fs::write(dir.path().join(INSTALL_RECORD_LEGACY), json).unwrap();
        let read = read_install_record(dir.path()).unwrap().unwrap();
        assert_eq!(read.version, "1.2.3");
    }

    #[test]
    fn writes_both_spellings() {
        let dir = tempdir().unwrap();
        write_install_record(dir.path(), &sample_record()).unwrap();
        assert!(dir.path().join(INSTALL_RECORD).is_file());
        assert!(dir.path().join(INSTALL_RECORD_LEGACY).is_file());
    }

    #[test]
    fn writes_both_manifest_spellings() {
        let dir = tempdir().unwrap();
        write_installed_manifest(dir.path(), &sample_manifest()).unwrap();
        assert!(dir.path().join(INSTALLED_MANIFEST).is_file());
        assert!(dir.path().join(INSTALLED_MANIFEST_LEGACY).is_file());
    }

    #[test]
    fn prefers_new_manifest_name() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(INSTALLED_MANIFEST), r#"{"version":"new"}"#).unwrap();
        std::fs::write(
            dir.path().join(INSTALLED_MANIFEST_LEGACY),
            r#"{"version":"old"}"#,
        )
        .unwrap();
        let read = read_installed_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(read.version, "new");
    }

    #[test]
    fn reads_legacy_only_manifest() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(INSTALLED_MANIFEST_LEGACY),
            r#"{"version":"legacy"}"#,
        )
        .unwrap();
        let read = read_installed_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(read.version, "legacy");
    }

    #[test]
    fn missing_files_yield_none() {
        let dir = tempdir().unwrap();
        assert!(read_install_record(dir.path()).unwrap().is_none());
        assert!(read_installed_manifest(dir.path()).unwrap().is_none());
    }

    #[test]
    fn corrupt_manifest_reports_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(INSTALLED_MANIFEST), "{not json").unwrap();
        assert!(read_installed_manifest(dir.path()).is_err());
    }

    #[test]
    fn is_installed_detects_either_spelling() {
        let dir = tempdir().unwrap();
        assert!(!is_installed(dir.path()));
        std::fs::write(dir.path().join(INSTALL_RECORD_LEGACY), "{}").unwrap();
        assert!(is_installed(dir.path()));
    }
}
