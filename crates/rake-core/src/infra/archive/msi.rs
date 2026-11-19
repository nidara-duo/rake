use std::path::Path;

#[cfg(windows)]
use super::sevenz::find_helper;
#[cfg(windows)]
use super::staging::{merge_move, normalize_manifest_dir};

#[cfg(windows)]
pub(super) fn extract_msi(
    src: &Path,
    dest: &Path,
    extract_dir: Option<&str>,
    root_path: Option<&Path>,
) -> crate::Result<()> {
    // Scoop's Expand-MsiArchive (lib/decompress.ps1:169-225) always stages the
    // extraction and then moves the payload out, because `msiexec /a` insists
    // on writing a `SourceDir` tree:
    //   * with `extract_dir`, stage into `<dest>\_tmp` and move `_tmp\SourceDir\<dir>`;
    //   * without it, move `_tmp\SourceDir` itself up into `dest`.
    // The move must merge: `dest` is the version directory, which already holds
    // the other downloads of this package.
    let staging = dest.join("_msi_tmp");
    crate::infra::fs::remove_dir(&staging)?;
    crate::infra::fs::ensure_dir(&staging)?;

    let target_dir = staging.join("SourceDir");
    let extracted = match find_helper("lessmsi", root_path) {
        Some(lessmsi) => std::process::Command::new(&lessmsi)
            .args([
                "x",
                &src.to_string_lossy(),
                &format!("{}\\", staging.display()),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| crate::Error::Io(std::io::Error::other(format!("lessmsi: {e}"))))?,
        None => std::process::Command::new("msiexec")
            .args([
                "/a",
                &src.to_string_lossy(),
                "/qn",
                &format!("TARGETDIR={}", target_dir.display()),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| crate::Error::Io(std::io::Error::other(format!("msiexec: {e}"))))?,
    };

    if !extracted.success() {
        let _ = crate::infra::fs::remove_dir(&staging);
        return Err(crate::Error::Io(std::io::Error::other(
            "MSI extraction failed (needs lessmsi or msiexec)",
        )));
    }

    // lessmsi writes the payload straight into `staging`; msiexec nests it in
    // `SourceDir`. Both shapes exist, so pick whichever appeared.
    let payload = if target_dir.is_dir() {
        target_dir
    } else {
        staging.clone()
    };

    let result = match normalize_manifest_dir("extract_dir", extract_dir)? {
        Some(dir) => merge_move(&payload.join(dir), dest),
        None => merge_move(&payload, dest),
    };

    // `_msi_tmp` also swallows an extracted copy of the .msi itself when the
    // staging directory is the destination (Scoop does the same cleanup).
    let _ = crate::infra::fs::remove_dir(&staging);
    result
}

#[cfg(unix)]
pub(super) fn extract_msi(
    _src: &Path,
    _dest: &Path,
    _extract_dir: Option<&str>,
    _root_path: Option<&Path>,
) -> crate::Result<()> {
    Err(crate::Error::Io(std::io::Error::other(
        "MSI extraction is not supported on this platform",
    )))
}
