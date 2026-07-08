use std::path::Path;

use async_trait::async_trait;

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    SevenZ,
    TarGz,
    TarBz2,
    TarXz,
    Tar,
    Gz,
    Xz,
    Bz2,
    Msi,
    Rar,
}

pub fn detect_format(path: &str) -> Option<ArchiveFormat> {
    let lower = path.to_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    if path.ends_with(".zip") || path.ends_with(".nupkg") {
        Some(ArchiveFormat::Zip)
    } else if path.ends_with(".7z") {
        Some(ArchiveFormat::SevenZ)
    } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        Some(ArchiveFormat::TarGz)
    } else if path.ends_with(".tar.bz2") || path.ends_with(".tbz2") || path.ends_with(".tbz") {
        Some(ArchiveFormat::TarBz2)
    } else if path.ends_with(".tar.xz") || path.ends_with(".txz") {
        Some(ArchiveFormat::TarXz)
    } else if path.ends_with(".tar") {
        Some(ArchiveFormat::Tar)
    } else if path.ends_with(".gz") {
        Some(ArchiveFormat::Gz)
    } else if path.ends_with(".xz") && !path.ends_with(".tar.xz") {
        Some(ArchiveFormat::Xz)
    } else if path.ends_with(".bz2") || path.ends_with(".bzip2") {
        Some(ArchiveFormat::Bz2)
    } else if path.ends_with(".msi") {
        Some(ArchiveFormat::Msi)
    } else if path.ends_with(".jar") {
        Some(ArchiveFormat::Zip)
    } else if path.ends_with(".rar") {
        Some(ArchiveFormat::Rar)
    } else {
        None
    }
}

/// Determine the archive format to use for extraction, honoring
/// Scoop's `url#/filename.ext` convention: some manifests append a URL
/// fragment (e.g. `#/dl.7z`) to declare a download's *true* archive
/// format when its literal HTTP extension doesn't reflect it. A common
/// real case: Git for Windows ships PortableGit as a self-extracting
/// `.exe` that is actually a `.7z` archive under the hood. When a
/// fragment with a recognizable extension is present, it is
/// authoritative for extraction purposes — never fall back to
/// re-deriving the format from a cache file's own path, since caching
/// logic may have already stripped this fragment.
pub fn detect_format_for_url(url: &str) -> Option<ArchiveFormat> {
    if let Some((_, fragment)) = url.split_once('#')
        && let Some(fmt) = detect_format(fragment)
    {
        return Some(fmt);
    }
    let base = url.split('#').next().unwrap_or(url);
    detect_format(base)
}

#[async_trait]
pub trait ArchiveService: Send + Sync {
    /// `format` must be resolved by the caller via
    /// [`detect_format_for_url`] applied to the manifest download URL —
    /// NOT re-derived from `src`'s file path, which may not carry the
    /// same format information (e.g. after cache-filename normalization).
    async fn extract(&self, src: &Path, dest: &Path, format: ArchiveFormat) -> Result<()>;
}

/// Native archive extractor backed by pure-Rust libraries, with optional
/// delegation to external helper tools (7-Zip, unrar) discovered under
/// the Rake/Scoop install root.
///
/// `root` should be the package-manager root (i.e.
/// `config().root_path`). When set, helper tools like 7-Zip are looked
/// up at `<root>/apps/<helper>/current/<exe>` — the standard Scoop/Rake
/// install layout. When `None`, only a PATH scan is performed, which
/// misses helper apps installed under the root (the previous default
/// behaviour, and the root cause of `7z: BadSignature` failures on
/// self-extracting archives like Git for Windows' PortableGit).
pub struct NativeArchive {
    root: Option<std::path::PathBuf>,
}

impl NativeArchive {
    pub fn new(root: Option<std::path::PathBuf>) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ArchiveService for NativeArchive {
    async fn extract(&self, src: &Path, dest: &Path, format: ArchiveFormat) -> Result<()> {
        match format {
            ArchiveFormat::Zip => extract_zip(src, dest),
            ArchiveFormat::SevenZ => extract_7z(src, dest, self.root.as_deref()),
            ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz
            | ArchiveFormat::Tar => extract_tar(src, dest).await,
            ArchiveFormat::Gz => extract_gz(src, dest).await,
            ArchiveFormat::Xz => extract_xz(src, dest).await,
            ArchiveFormat::Bz2 => extract_bz2(src, dest).await,
            ArchiveFormat::Msi => extract_msi(src, dest),
            ArchiveFormat::Rar => extract_rar(src, dest, self.root.as_deref()),
        }
    }
}

fn extract_zip(src: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(src)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| crate::Error::Archive(format!("zip: {}", e)))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| crate::Error::Archive(format!("zip entry: {}", e)))?;

        let out_path = match entry.enclosed_name() {
            Some(p) => dest.join(p),
            None => continue,
        };

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out).map_err(|e| {
                crate::Error::Archive(format!("write {}: {}", out_path.display(), e))
            })?;
        }
    }

    Ok(())
}

/// Extract a `.7z` archive, including self-extracting `.7z.exe` (SFX)
/// archives such as Git for Windows' PortableGit installer (identified
/// via a manifest URL's `#/name.7z` fragment — see
/// [`detect_format_for_url`]).
///
/// Prefers the external `7z` tool when available: 7-Zip's own archive
/// engine transparently locates 7z data embedded after an SFX stub,
/// which the pure-Rust `sevenz-rust2` crate is not guaranteed to do.
/// Falls back to `sevenz-rust2` only when no external 7z tool is
/// installed — this still correctly handles plain, non-SFX `.7z`
/// files, just not self-extracting ones.
fn extract_7z(src: &Path, dest: &Path, root_path: Option<&Path>) -> Result<()> {
    // Find 7-Zip under as many aliases as Scoop/Rake install it under:
    //   - `7zip`   — the actual Scoop app name (most common)
    //   - `7z`     — what `rake install 7zip` is colloquially called
    //   - `7-Zip`  — the upstream product name (normalised to `7zip` in find_helper)
    // The previous code only tried `7z` and `7-Zip` with root_path=None,
    // so it never looked under `<root>/apps/7zip/current/7z.exe` and
    // silently fell through to sevenz-rust2, which cannot handle SFX
    // `.7z.exe` archives (e.g. Git for Windows PortableGit).
    let helper = find_helper("7zip", root_path)
        .or_else(|| find_helper("7z", root_path))
        .or_else(|| find_helper("7-Zip", root_path));

    if let Some(helper) = helper {
        crate::infra::fs::ensure_dir(dest)?;
        let status = std::process::Command::new(&helper)
            .args([
                "x",
                &src.to_string_lossy(),
                &format!("-o{}", dest.display()),
                "-y",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| crate::Error::Io(std::io::Error::other(format!("7z: {e}"))))?;

        if status.success() {
            return Ok(());
        }

        // External 7-Zip ran but failed. Surface this rather than silently
        // falling back to sevenz-rust2: for an SFX archive the fallback
        // will fail anyway with a misleading `BadSignature([MZ…])`, and
        // the external tool's stderr (which it printed itself) is the
        // actually-useful diagnostic.
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "7z exited with {:?} (using {})",
            status.code(),
            helper.display()
        ))));
    }

    // No external 7-Zip available — pure-Rust fallback. This handles
    // plain, non-SFX `.7z` files; for SFX archives it will fail, and the
    // user should `rake install 7zip` (see rake checkup).
    sevenz_rust2::decompress_file(src, dest).map_err(|e| crate::Error::Archive(format!("7z: {e}")))
}

async fn extract_tar(src: &Path, dest: &Path) -> Result<()> {
    let file = tokio::fs::File::open(src).await?;
    let reader = tokio::io::BufReader::new(file);

    let decompressed: Box<dyn tokio::io::AsyncRead + Unpin + Send> =
        match src.extension().and_then(|s| s.to_str()) {
            Some("gz") | Some("tgz") => Box::new(tokio::io::BufReader::new(
                async_compression::tokio::bufread::GzipDecoder::new(reader),
            )),
            Some("bz2") => Box::new(tokio::io::BufReader::new(
                async_compression::tokio::bufread::BzDecoder::new(reader),
            )),
            Some("xz") => Box::new(tokio::io::BufReader::new(
                async_compression::tokio::bufread::XzDecoder::new(reader),
            )),
            _ => Box::new(reader),
        };

    let mut archive = tokio_tar::Archive::new(decompressed);
    archive
        .unpack(dest)
        .await
        .map_err(|e| crate::Error::Archive(format!("tar: {}", e)))?;

    Ok(())
}

async fn extract_gz(src: &Path, dest: &Path) -> Result<()> {
    let file = tokio::fs::File::open(src).await?;
    let reader = tokio::io::BufReader::new(file);
    let mut decoder = async_compression::tokio::bufread::GzipDecoder::new(reader);

    let out_name = src
        .file_stem()
        .unwrap_or(src.file_name().unwrap_or_default());
    let out_path = dest.join(out_name);

    let mut out = tokio::fs::File::create(&out_path).await?;
    tokio::io::copy(&mut decoder, &mut out)
        .await
        .map_err(|e| crate::Error::Archive(format!("gz: {}", e)))?;

    Ok(())
}

async fn extract_xz(src: &Path, dest: &Path) -> Result<()> {
    let file = tokio::fs::File::open(src).await?;
    let reader = tokio::io::BufReader::new(file);
    let mut decoder = async_compression::tokio::bufread::XzDecoder::new(reader);

    let out_name = src
        .file_stem()
        .unwrap_or(src.file_name().unwrap_or_default());
    let out_path = dest.join(out_name);

    let mut out = tokio::fs::File::create(&out_path).await?;
    tokio::io::copy(&mut decoder, &mut out)
        .await
        .map_err(|e| crate::Error::Archive(format!("xz: {}", e)))?;

    Ok(())
}

async fn extract_bz2(src: &Path, dest: &Path) -> Result<()> {
    let file = tokio::fs::File::open(src).await?;
    let reader = tokio::io::BufReader::new(file);
    let mut decoder = async_compression::tokio::bufread::BzDecoder::new(reader);

    let out_name = src
        .file_stem()
        .unwrap_or(src.file_name().unwrap_or_default());
    let out_path = dest.join(out_name);

    let mut out = tokio::fs::File::create(&out_path).await?;
    tokio::io::copy(&mut decoder, &mut out)
        .await
        .map_err(|e| crate::Error::Archive(format!("bz2: {}", e)))?;

    Ok(())
}

fn extract_rar(src: &Path, dest: &Path, root_path: Option<&Path>) -> Result<()> {
    // For .rar files, use external 7z or unrar helper
    // (no pure-Rust RAR library that handles modern RAR5)
    let helper = find_helper("7zip", root_path)
        .or_else(|| find_helper("7z", root_path))
        .or_else(|| find_helper("7-Zip", root_path))
        .or_else(|| find_helper("unrar", root_path))
        .ok_or_else(|| {
            crate::Error::Io(std::io::Error::other(
                "RAR extraction requires 7z or unrar — run 'rake install 7zip' first",
            ))
        })?;

    let status = std::process::Command::new(&helper)
        .args([
            "x",
            &src.to_string_lossy(),
            &format!("-o{}", dest.display()),
            "-y",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("rar: {e}"))))?;

    if !status.success() {
        return Err(crate::Error::Io(std::io::Error::other(
            "RAR extraction failed",
        )));
    }

    Ok(())
}

#[cfg(windows)]
fn extract_msi(src: &Path, dest: &Path) -> crate::Result<()> {
    let tmp = dest.join("_msi_tmp");
    crate::infra::fs::ensure_dir(&tmp)?;

    let status = std::process::Command::new("msiexec")
        .args([
            "/a",
            &src.to_string_lossy(),
            "/qn",
            &format!(r"TARGETDIR={}\SourceDir", tmp.display()),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("msiexec: {e}"))))?;

    if !status.success() {
        let _ = crate::infra::fs::remove_dir(&tmp);
        return Err(crate::Error::Io(std::io::Error::other(
            "msiexec extraction failed",
        )));
    }

    let source_dir = tmp.join("SourceDir");
    if source_dir.exists() {
        crate::infra::fs::copy_dir(&source_dir, dest)?;
    }

    crate::infra::fs::remove_dir(&tmp)?;
    Ok(())
}

#[cfg(unix)]
fn extract_msi(_src: &Path, _dest: &Path) -> crate::Result<()> {
    Err(crate::Error::Io(std::io::Error::other(
        "MSI extraction is not supported on this platform",
    )))
}

/// Parse `-ExtractDir '...'` from an `Expand-InnoArchive` command line.
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
/// For each `Expand-InnoArchive` line, parses `-ExtractDir` and `-Removal`
/// flags then calls `innounp` for that component.
/// The source file is only removed if any line contains `-Removal`.
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

    let mut should_remove = false;

    for line in lines {
        let line = line.trim();
        if !line.starts_with("Expand-InnoArchive") {
            continue;
        }

        let extract_dir = parse_extract_dir(line);
        let has_removal = line.contains("-Removal");
        if has_removal {
            should_remove = true;
        }

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

    if should_remove {
        let _ = std::fs::remove_file(src);
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
/// The original `.exe` is removed after extraction (Scoop's `-Removal` behaviour).
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

    // Remove original .exe (Scoop's -Removal behaviour)
    let _ = std::fs::remove_file(src);

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
fn find_helper(name: &str, root_path: Option<&Path>) -> Option<std::path::PathBuf> {
    let exe_name = helper_exe_name(name);
    let mut candidates = Vec::new();

    // First, look in actual root_path if provided
    if let Some(root) = root_path {
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
        std::path::PathBuf::from("apps")
            .join(name)
            .join("current")
            .join(&exe_name),
    );
    candidates.push(
        std::path::PathBuf::from("apps")
            .join("apps")
            .join(name)
            .join("current")
            .join(&exe_name),
    );
    // bare name in CWD
    candidates.push(std::path::PathBuf::from(&exe_name));

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

#[cfg(test)]
mod format_detection_tests {
    use super::*;

    #[test]
    fn detects_sfx_7z_via_url_fragment() {
        let url = "https://github.com/git-for-windows/git/releases/download/v2.55.0.windows.1/PortableGit-2.55.0-64-bit.7z.exe#/dl.7z";
        assert_eq!(detect_format_for_url(url), Some(ArchiveFormat::SevenZ));
    }

    #[test]
    fn detects_plain_zip_without_fragment() {
        let url = "https://example.com/tool-1.0.0.zip";
        assert_eq!(detect_format_for_url(url), Some(ArchiveFormat::Zip));
    }

    #[test]
    fn falls_back_to_base_url_when_fragment_has_no_extension() {
        let url = "https://example.com/tool-1.0.0.zip#/somelabel";
        assert_eq!(detect_format_for_url(url), Some(ArchiveFormat::Zip));
    }

    #[test]
    fn returns_none_for_unrecognized_extension() {
        let url = "https://example.com/tool-1.0.0.exe";
        assert_eq!(detect_format_for_url(url), None);
    }
}
