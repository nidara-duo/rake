use std::path::{Path, PathBuf};

use super::ArchiveFormat;
use super::staging::StagedExtraction;
use crate::Result;

/// Extract a `.7z` archive, including self-extracting `.7z.exe` (SFX) archives
/// such as Git for Windows' PortableGit installer — which is why a manifest URL
/// carries `#/dl.7z`: the fragment renames the download to something
/// [`super::detect_format`] recognises as a 7-Zip archive.
///
/// `external_only` is set for formats no Rust crate handles (`.rar`, `.zst`,
/// `.lzma`, `.iso`, …). Those fail with an actionable message instead of a
/// misleading "BadSignature" from a native parser.
pub(super) fn extract_7z(
    src: &Path,
    dest: &Path,
    extract_dir: Option<&str>,
    root_path: Option<&Path>,
    external_only: bool,
) -> Result<()> {
    let staged = StagedExtraction::new(dest, extract_dir)?;

    if let Some(helper) = find_7z(root_path) {
        run_7z(&helper, src, &staged.staging, staged.extract_dir.as_deref())?;
        return staged.finish();
    }

    if external_only {
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "{} requires 7-Zip — run 'rake install 7zip' first",
            src.display()
        ))));
    }

    // No external 7-Zip available — pure-Rust fallback. This handles plain,
    // non-SFX `.7z` files; for SFX archives it will fail, and the user should
    // `rake install 7zip` (see rake checkup).
    sevenz_rust2::decompress_file(src, &staged.staging)
        .map_err(|e| crate::Error::Archive(format!("7z: {e}")))?;
    staged.finish()
}

pub(super) fn run_7z_stream_fallback(
    helper: &Path,
    src: &Path,
    staging: &Path,
    format: ArchiveFormat,
    extract_dir: Option<&str>,
) -> Result<()> {
    if matches!(
        format,
        ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz
    ) {
        // 7-Zip unpacks only the outer compression layer of `.tar.{gz,bz2,xz}` on the
        // first pass, producing a single `.tar` file (Scoop `Expand-7zipArchive`,
        // lib/decompress.ps1:83-102). Unpack that outer stream into a sibling temp dir,
        // then extract the inner `.tar` into `staging`.
        let tar_tmp = tempfile::Builder::new()
            .prefix("rake-tar-")
            .tempdir_in(staging.parent().unwrap_or(staging))
            .map_err(crate::Error::Io)?;

        run_7z(helper, src, tar_tmp.path(), None)?;

        let inner_tar = std::fs::read_dir(tar_tmp.path())
            .map_err(crate::Error::Io)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.is_file())
            .ok_or_else(|| {
                crate::Error::Archive(format!(
                    "7-Zip outer pass produced no inner tar for {}",
                    src.display()
                ))
            })?;

        run_7z(helper, &inner_tar, staging, extract_dir)
    } else {
        run_7z(helper, src, staging, extract_dir)
    }
}

/// Run 7-Zip's `x` command into `dest`.
///
/// `-ir!<extract_dir>\*` narrows the extraction to the requested subtree, the
/// same switch Scoop passes (lib/decompress.ps1:97). `-xr!*.nsis` drops the
/// NSIS payload that some SFX archives carry, also matching Scoop.
pub(super) fn run_7z(
    helper: &Path,
    src: &Path,
    dest: &Path,
    extract_dir: Option<&str>,
) -> Result<()> {
    crate::infra::fs::ensure_dir(dest)?;

    let mut args: Vec<String> = vec![
        "x".to_owned(),
        src.to_string_lossy().into_owned(),
        format!("-o{}", dest.display()),
        "-xr!*.nsis".to_owned(),
        "-y".to_owned(),
    ];
    if let Some(dir) = extract_dir {
        args.push(format!("-ir!{}\\*", dir.replace('/', "\\")));
    }

    let status = std::process::Command::new(helper)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("7z: {e}"))))?;

    if !status.success() {
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "7z exited with {:?} (using {})",
            status.code(),
            helper.display()
        ))));
    }

    Ok(())
}

pub(super) fn find_7z(root_path: Option<&Path>) -> Option<PathBuf> {
    find_helper("7zip", root_path)
        .or_else(|| find_helper("7z", root_path))
        .or_else(|| find_helper("7-Zip", root_path))
}

/// Resolve a helper app name to the executable filename looked up on
/// disk / PATH. If the caller already passed a name ending in `.exe`,
/// it is used verbatim.
///
/// e.g. `helper_exe_name("innounp")` → `"innounp.exe"`,
///      `helper_exe_name("7zip")`   → `"7z.exe"`.
fn helper_exe_name(app_name: &str) -> String {
    if app_name.ends_with(".exe") {
        return app_name.to_owned();
    }
    let exe = match app_name {
        "innounp" | "innounp-unicode" => "innounp.exe",
        "7z" | "7-Zip" | "7zip" | "sevenzip" => "7z.exe",
        "unrar" => "unrar.exe",
        "dark" => "dark.exe",
        "lessmsi" => "lessmsi.exe",
        other => other,
    };
    exe.to_owned()
}

/// Find a Scoop helper tool (innounp, 7z, unrar, etc.) in installed apps or PATH.
///
/// `root_path` should be the Scoop root (e.g. `config().root_path`).
/// If `None`, falls back to CWD-relative paths.
pub(super) fn find_helper(name: &str, root_path: Option<&Path>) -> Option<PathBuf> {
    let exe_name = helper_exe_name(name);
    let mut candidates = Vec::new();

    // First, look in actual root_path if provided
    if let Some(root) = root_path {
        // Scoop puts a shim for every helper in `<root>\shims`, so a helper
        // that is on PATH only through Scoop is found here even when the
        // process PATH has not been refreshed.
        candidates.push(root.join("shims").join(&exe_name));
        candidates.push(root.join("apps").join(name).join("current").join(&exe_name));
        // Also try windows-capitalized form (7-Zip → 7zip)
        let normalized = name.to_lowercase().replace(['-', ' '], "");
        if normalized != name {
            candidates.push(
                root.join("apps")
                    .join(&normalized)
                    .join("current")
                    .join(&exe_name),
            );
        }
    }

    // Fallback: CWD-relative paths (for tests or flat layout)
    candidates.push(
        PathBuf::from("apps")
            .join(name)
            .join("current")
            .join(&exe_name),
    );
    candidates.push(
        PathBuf::from("apps")
            .join("apps")
            .join(name)
            .join("current")
            .join(&exe_name),
    );
    // bare name in CWD
    candidates.push(PathBuf::from(&exe_name));

    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }

    // Search PATH
    if let Ok(paths) = std::env::var("PATH") {
        for path in std::env::split_paths(&paths) {
            let p = path.join(&exe_name);
            if p.exists() {
                return Some(p);
            }
        }
    }

    None
}
