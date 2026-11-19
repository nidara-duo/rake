use std::path::Path;

#[cfg(windows)]
use super::sevenz::find_helper;

/// Parse `-ExtractDir '...'` from an `Expand-InnoArchive` command line.
#[cfg(windows)]
fn parse_extract_dir(line: &str) -> Option<&str> {
    let marker = "-ExtractDir '";
    let start = line.find(marker)?;
    let value_start = start + marker.len();
    let remaining = &line[value_start..];
    let value_end = remaining.find('\'')?;
    Some(&remaining[..value_end])
}

/// Execute `installer.script` lines from a Scoop manifest.
///
/// For each `Expand-InnoArchive` line, parses `-ExtractDir` flag then calls
/// `innounp` for that component.
///
/// IMPORTANT: The source file (a persistent cache entry) is NEVER deleted
/// here, even if the manifest's `installer.script` contains `-Removal`.
/// `-Removal` is a Scoop-side instruction that assumes Scoop's own
/// copy-then-extract pipeline (lib/download.ps1 copies the cached file into
/// the version directory first, then extracts from that copy — the cache
/// entry survives).  Rake extracts directly from the cache file, so
/// `-Removal` must not be honoured as "delete the cache file."
#[cfg(windows)]
pub fn extract_innosetup_with_script(
    lines: &[String],
    src: &Path,
    dest: &Path,
    root_path: Option<&Path>,
) -> crate::Result<()> {
    let innounp = find_helper("innounp", root_path)
        .or_else(|| find_helper("innounp-unicode", root_path))
        .ok_or_else(|| {
            crate::Error::Io(std::io::Error::other(
                "innounp not found — run 'rake install innounp' first",
            ))
        })?;

    for line in lines {
        let line = line.trim();
        if !line.starts_with("Expand-InnoArchive") {
            continue;
        }

        let extract_dir = parse_extract_dir(line);

        let extract_flag = match extract_dir {
            Some(dir) if !dir.is_empty() => {
                if dir.starts_with('{') {
                    format!("-c{dir}")
                } else {
                    format!("-c{{app}}\\{dir}")
                }
            }
            _ => "-c{app}".to_owned(),
        };

        let log_path = dest.join("_innounp.log");

        let status = std::process::Command::new(&innounp)
            .args([
                "-x",
                &format!("-d{}", dest.display()),
                &src.to_string_lossy(),
                "-y",
                &extract_flag,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| crate::Error::Io(std::io::Error::other(format!("innounp: {e}"))))?;

        if log_path.exists() {
            let _ = std::fs::remove_file(&log_path);
        }

        if !status.success() {
            return Err(crate::Error::Io(std::io::Error::other(format!(
                "innounp extraction failed ({}), exit code: {:?}",
                extract_flag,
                status.code()
            ))));
        }
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn extract_innosetup_with_script(
    _lines: &[String],
    _src: &Path,
    _dest: &Path,
    _root_path: Option<&Path>,
) -> crate::Result<()> {
    Err(crate::Error::Io(std::io::Error::other(
        "InnoSetup extraction is not supported on this platform",
    )))
}

/// Extract an InnoSetup installer via `innounp`.
///
/// Finds `innounp.exe` in installed apps or PATH, then runs:
///   innounp -x -d<dest> <path> -y -c{app}
///
/// If `extract_dir` is set, passes `-c{app}\<extract_dir>` to innounp
/// (matching Scoop's `-ExtractDir` semantic).
///
/// IMPORTANT: The source file (a persistent cache entry) is NEVER deleted
/// here.  Scoop copies the cached file into the version directory before
/// extraction (lib/download.ps1), meaning its cache survives and its
/// disposable copy can be deleted.  Rake extracts directly from the cache,
/// so this function must NOT remove it — that would break rake's own
/// cache-reuse guarantee and diverge from Scoop's behaviour.
#[cfg(windows)]
pub fn extract_innosetup(
    src: &Path,
    dest: &Path,
    root_path: Option<&Path>,
    extract_dir: Option<&str>,
) -> crate::Result<()> {
    let innounp = find_helper("innounp", root_path)
        .or_else(|| find_helper("innounp-unicode", root_path))
        .ok_or_else(|| {
            crate::Error::Io(std::io::Error::other(
                "innounp not found — run 'rake install innounp' first",
            ))
        })?;

    let log_path = dest.join("_innounp.log");

    let extract_flag = match extract_dir {
        Some(dir) if !dir.is_empty() => {
            if dir.starts_with('{') {
                format!("-c{dir}")
            } else {
                format!("-c{{app}}\\{dir}")
            }
        }
        _ => "-c{app}".to_owned(),
    };

    let status = std::process::Command::new(&innounp)
        .args([
            "-x",
            &format!("-d{}", dest.display()),
            &src.to_string_lossy(),
            "-y",
            &extract_flag,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("innounp: {e}"))))?;

    if log_path.exists() {
        let _ = std::fs::remove_file(&log_path);
    }

    if !status.success() {
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "innounp extraction failed, exit code: {:?}",
            status.code()
        ))));
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn extract_innosetup(
    _src: &Path,
    _dest: &Path,
    _root_path: Option<&Path>,
    _extract_dir: Option<&str>,
) -> crate::Result<()> {
    Err(crate::Error::Io(std::io::Error::other(
        "InnoSetup extraction is not supported on this platform",
    )))
}
