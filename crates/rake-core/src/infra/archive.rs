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
mod extraction_tests {
    use super::*;
    use std::io::Write;

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap();
    }

    /// The ordinary case, so the guard tests below cannot pass by extraction simply
    /// being broken.
    #[test]
    fn extracts_a_zip_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("out");
        write_zip(&src, &[("app.exe", b"binary"), ("readme.txt", b"hello")]);

        extract_zip(&src, &dest).unwrap();

        assert_eq!(std::fs::read(dest.join("app.exe")).unwrap(), b"binary");
        assert_eq!(std::fs::read(dest.join("readme.txt")).unwrap(), b"hello");
    }

    #[test]
    fn extracts_nested_directories_and_directory_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("out");
        write_zip(
            &src,
            &[
                ("bin/", b""),
                ("bin/tool/", b""),
                ("bin/tool/deep.txt", b"x"),
            ],
        );

        extract_zip(&src, &dest).unwrap();

        assert_eq!(std::fs::read(dest.join("bin/tool/deep.txt")).unwrap(), b"x");
    }

    /// Zip-slip. Extraction holds because `extract_zip` goes through
    /// `ZipFile::enclosed_name()`, which returns `None` for a path that leaves the
    /// destination — and the code skips those entries.
    ///
    /// Worth pinning explicitly: swapping `enclosed_name()` for the raw `name()`
    /// would reintroduce the vulnerability with no other test noticing.
    #[test]
    fn zip_entries_escaping_the_destination_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        for evil in [
            "../escaped.txt",
            "../../escaped.txt",
            "sub/../../escaped.txt",
        ] {
            let src = tmp.path().join("evil.zip");
            write_zip(&src, &[(evil, b"pwned"), ("safe.txt", b"ok")]);

            extract_zip(&src, &dest).unwrap();

            assert!(
                !dest.parent().unwrap().join("escaped.txt").exists(),
                "{evil} must not be written outside the destination"
            );
            assert!(
                !tmp.path().join("escaped.txt").exists(),
                "{evil} must not be written anywhere above the destination"
            );
            // The harmless entry still extracted, so this is a real extraction.
            assert!(dest.join("safe.txt").is_file());
            std::fs::remove_file(dest.join("safe.txt")).unwrap();
        }
    }

    /// An absolute entry name is refused for the same reason.
    #[test]
    fn zip_entries_with_absolute_paths_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("evil.zip");
        let dest = tmp.path().join("out");

        let absolute = if cfg!(windows) {
            r"C:\escaped.txt"
        } else {
            "/escaped.txt"
        };
        write_zip(&src, &[(absolute, b"pwned"), ("safe.txt", b"ok")]);

        extract_zip(&src, &dest).unwrap();

        assert!(
            !Path::new(absolute).exists(),
            "an absolute entry must not be written"
        );
        assert!(dest.join("safe.txt").is_file());
    }

    /// Detection happens on the extension, so a manifest pointing at a URL with a
    /// query string must not be misread.
    #[test]
    fn query_strings_do_not_confuse_detection() {
        assert_eq!(
            detect_format("https://x/tool.zip?token=abc"),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            detect_format("HTTPS://X/TOOL.ZIP"),
            Some(ArchiveFormat::Zip),
            "detection should be case-insensitive"
        );
    }

    fn put_octal(field: &mut [u8], value: u64) {
        let digits = format!("{:0width$o}", value, width = field.len() - 1);
        field[..digits.len()].copy_from_slice(digits.as_bytes());
        field[field.len() - 1] = 0;
    }

    /// A hand-built ustar header, so the entry name can be anything at all — including
    /// a path that escapes. `tokio_tar::Builder` would not necessarily let us write one.
    fn tar_with_entry(name: &str, data: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        assert!(
            name_bytes.len() < 100,
            "test helper handles short names only"
        );
        header[..name_bytes.len()].copy_from_slice(name_bytes);
        put_octal(&mut header[100..108], 0o644); // mode
        put_octal(&mut header[108..116], 0); // uid
        put_octal(&mut header[116..124], 0); // gid
        put_octal(&mut header[124..136], data.len() as u64); // size
        put_octal(&mut header[136..148], 0); // mtime
        header[148..156].fill(b' '); // checksum placeholder
        header[156] = b'0'; // typeflag: regular file
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[265..269].copy_from_slice(b"root");

        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());

        let mut out = header.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(512), 0);
        out.resize(out.len() + 1024, 0); // two terminating zero blocks
        out
    }

    #[tokio::test]
    async fn extracts_a_tar_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.tar");
        let dest = tmp.path().join("out");
        std::fs::write(&src, tar_with_entry("app/bin/tool", b"hello")).unwrap();

        extract_tar(&src, &dest).await.unwrap();

        assert_eq!(
            std::fs::read(dest.join("app").join("bin").join("tool")).unwrap(),
            b"hello"
        );
    }

    /// The tar equivalent of zip-slip. Unlike zip, this one relies on `tokio_tar`
    /// refusing the path rather than on any check in this crate, so it is worth
    /// proving rather than assuming — a regression in the dependency would otherwise
    /// turn into a silent write outside the version directory.
    #[tokio::test]
    async fn tar_entries_escaping_the_destination_are_not_written_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let escaped = dest.parent().unwrap().join("escaped.txt");
        let _ = std::fs::remove_file(&escaped);

        let src = tmp.path().join("evil.tar");
        std::fs::write(&src, tar_with_entry("../escaped.txt", b"pwned")).unwrap();

        extract_tar(&src, &dest).await.unwrap();

        assert!(
            !escaped.exists(),
            "tar entry ../escaped.txt must not land outside the destination"
        );
    }

    /// And the deeper form, plus a check that ordinary entries in the same archive
    /// still extract — so this cannot pass by the whole archive being rejected.
    #[tokio::test]
    async fn deep_tar_traversal_is_refused_while_ordinary_entries_extract() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let escaped = tmp.path().join("escaped.txt");
        let _ = std::fs::remove_file(&escaped);

        let mut archive = tar_with_entry("good.txt", b"ok");
        let evil = tar_with_entry("../../escaped.txt", b"pwned");
        archive.extend_from_slice(&evil[..evil.len() - 1024]);

        let src = tmp.path().join("mixed.tar");
        std::fs::write(&src, &archive).unwrap();

        extract_tar(&src, &dest).await.unwrap();

        assert!(!escaped.exists(), "the traversal entry must not escape");
        assert!(
            dest.join("good.txt").is_file(),
            "the ordinary entry must still extract"
        );
    }
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

#[cfg(test)]
#[cfg(windows)]
mod innosetup_extraction_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Regression: rake must never delete the cache file after InnoSetup
    /// extraction.  Official Scoop copies from cache into the version
    /// directory first, then extracts from the copy (the cache survives).
    /// Rake extracts directly from the cache, so remove_file(src) would
    /// break the cache-reuse guarantee.
    #[test]
    fn cache_file_survives_innosetup_extraction() {
        let dir = tempdir().unwrap();

        // Create a mock innounp.exe that find_helper can discover
        let innounp_dir = dir.path().join("apps").join("innounp").join("current");
        fs::create_dir_all(&innounp_dir).unwrap();
        fs::write(innounp_dir.join("innounp.exe"), b"").unwrap();

        // Create a fake cache file
        let cache_file = dir.path().join("cache").join("test-installer.exe");
        fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
        fs::write(&cache_file, b"fake installer content").unwrap();

        let dest = dir.path().join("extracted");
        fs::create_dir_all(&dest).unwrap();

        // Empty lines means no Expand-InnoArchive lines — innounp is
        // resolved but never invoked.  The function returns Ok after
        // an empty loop.  Without the fix, the old code would still
        // unconditionally remove src in extract_innosetup, or
        // conditionally remove it in extract_innosetup_with_script
        // if any line had -Removal.
        let lines: Vec<String> = vec![];
        let result = extract_innosetup_with_script(&lines, &cache_file, &dest, Some(dir.path()));

        assert!(result.is_ok(), "empty script should succeed");
        assert!(
            cache_file.exists(),
            "cache file must survive InnoSetup extraction"
        );
    }
}
