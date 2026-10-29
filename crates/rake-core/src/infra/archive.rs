use std::path::{Path, PathBuf};

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
    /// A format only 7-Zip can read (`.zst`, `.lzma`, `.lzh`, `.iso`, `.img`,
    /// split `.001` volumes). Scoop hands these to `Expand-7zipArchive` through
    /// `Test-7zipRequirement` (lib/depends.ps1:134), so Rake must too.
    External7z,
}

/// Detect an archive format from a file name.
///
/// The input is the file's name on disk — for a manifest URL that is
/// `url_filename(url)`, i.e. the `#/name.ext` fragment when present. This is
/// exactly what Scoop's `Invoke-Extraction` switches on (`$Name[$i]`,
/// lib/decompress.ps1:25), so `Setup.exe#/dl.7z` is a 7-Zip archive and a bare
/// `tool.exe` is not.
///
/// Two deliberate omissions, both copied from Scoop:
/// * `.cab` is **not** an archive. Main-bucket manifests (`msival2`, `orca`,
///   `visualc`) download `.cab` files next to an `.msi`; `msiexec /a` resolves
///   external cabinets relative to the `.msi`, so the cabinets have to stay on
///   disk. Extracting them breaks those packages.
/// * `.jar` is **not** an archive either. `plantuml` ships a `.jar` that Scoop
///   copies into the version directory and runs; unzipping it would destroy it.
pub fn detect_format(path: &str) -> Option<ArchiveFormat> {
    let lower = path.to_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    // Compound extensions first: `.tar.gz` must not be mistaken for `.gz`.
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
    } else if path.ends_with(".rar") {
        Some(ArchiveFormat::Rar)
    } else if path.ends_with(".zst")
        || path.ends_with(".tzst")
        || path.ends_with(".lzma")
        || path.ends_with(".lzh")
        || path.ends_with(".iso")
        || path.ends_with(".img")
        || path.ends_with(".001")
    {
        Some(ArchiveFormat::External7z)
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

impl NativeArchive {
    /// Extract `src` into `dest`, applying Scoop's `extract_dir` semantics.
    ///
    /// `extract_dir` names a directory *inside* the archive whose contents must
    /// end up directly in `dest`. Scoop does this by extracting into
    /// `<dest>\_tmp` and then `movedir`-ing `_tmp\<extract_dir>` onto `dest`
    /// (lib/decompress.ps1:281-296). The move has to merge, not rename: `dest`
    /// is the version directory, which already holds the other artifacts of the
    /// package and may be the target of a previous URL.
    pub async fn extract_with(
        &self,
        src: &Path,
        dest: &Path,
        format: ArchiveFormat,
        extract_dir: Option<&str>,
    ) -> Result<()> {
        let root = self.root.as_deref();
        match format {
            ArchiveFormat::Zip => extract_zip(src, dest, extract_dir, root),
            ArchiveFormat::SevenZ => extract_7z(src, dest, extract_dir, root, false),
            ArchiveFormat::Rar | ArchiveFormat::External7z => {
                extract_7z(src, dest, extract_dir, root, true)
            }
            ArchiveFormat::Msi => extract_msi(src, dest, extract_dir, root),
            ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz
            | ArchiveFormat::Tar
            | ArchiveFormat::Gz
            | ArchiveFormat::Xz
            | ArchiveFormat::Bz2 => {
                let staged = StagedExtraction::new(dest, extract_dir)?;
                match extract_plain(src, &staged.staging, format).await {
                    Ok(()) => staged.finish(),
                    Err(native_error) => match find_7z(root) {
                        Some(helper) => {
                            tracing::warn!(
                                "{}: native extraction failed ({native_error}), retrying with 7-Zip",
                                src.display()
                            );
                            staged.reset()?;
                            run_7z_stream_fallback(
                                &helper,
                                src,
                                &staged.staging,
                                format,
                                staged.extract_dir.as_deref(),
                            )?;
                            staged.finish()
                        }
                        None => Err(native_error),
                    },
                }
            }
        }
    }
}

#[async_trait]
impl ArchiveService for NativeArchive {
    async fn extract(&self, src: &Path, dest: &Path, format: ArchiveFormat) -> Result<()> {
        self.extract_with(src, dest, format, None).await
    }
}

async fn extract_plain(src: &Path, dest: &Path, format: ArchiveFormat) -> Result<()> {
    match format {
        ArchiveFormat::TarGz
        | ArchiveFormat::TarBz2
        | ArchiveFormat::TarXz
        | ArchiveFormat::Tar => extract_tar(src, dest).await,
        ArchiveFormat::Gz => extract_gz(src, dest).await,
        ArchiveFormat::Xz => extract_xz(src, dest).await,
        ArchiveFormat::Bz2 => extract_bz2(src, dest).await,
        other => Err(crate::Error::Archive(format!(
            "extract_plain called with {other:?}"
        ))),
    }
}

/// One extraction destination, plus the `extract_dir` merge that has to follow.
///
/// When `extract_dir` is set the archive is unpacked into `<dest>\_tmp` and the
/// contents of `_tmp\<extract_dir>` are merged into `dest` afterwards. Without
/// it the archive goes straight into `dest`.
struct StagedExtraction {
    staging: PathBuf,
    dest: PathBuf,
    extract_dir: Option<String>,
}

impl StagedExtraction {
    fn new(dest: &Path, extract_dir: Option<&str>) -> Result<Self> {
        let extract_dir = normalize_manifest_dir("extract_dir", extract_dir)?;
        let staging = match extract_dir {
            Some(_) => dest.join("_tmp"),
            None => dest.to_path_buf(),
        };
        crate::infra::fs::ensure_dir(&staging)?;

        Ok(Self {
            staging,
            dest: dest.to_path_buf(),
            extract_dir,
        })
    }

    /// Clear a staging directory that a failed native extractor may have left
    /// half-written. Never touches `dest` itself, which may already contain
    /// artifacts from earlier URLs.
    fn reset(&self) -> Result<()> {
        if self.extract_dir.is_some() {
            crate::infra::fs::remove_dir(&self.staging)?;
            crate::infra::fs::ensure_dir(&self.staging)?;
        }
        Ok(())
    }

    fn finish(self) -> Result<()> {
        let Some(dir) = &self.extract_dir else {
            return Ok(());
        };

        let result = merge_move(&self.staging.join(dir), &self.dest);
        // Removes `_tmp` (and the empty ancestors of `extract_dir` with it) even
        // when the merge failed: a broken archive must not leave `_tmp` inside
        // the app directory.
        let _ = crate::infra::fs::remove_dir(&self.staging);
        result
    }
}

/// Normalize a manifest `extract_dir` / `extract_to` value.
///
/// Manifests write these with either separator (`PFiles\Gossip\Gossip`,
/// `bin/tool`), and `Path::join` only understands the platform's own. `"."`
/// means the destination itself. Absolute paths and `..` are rejected: they are
/// untrusted manifest input and would move files out of the version directory.
pub(crate) fn normalize_manifest_dir(field: &str, value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };

    let normalized = value
        .replace('\\', "/")
        .trim_end_matches('/')
        .trim_start_matches("./")
        .to_owned();

    if normalized.is_empty() || normalized == "." {
        return Ok(None);
    }

    crate::infra::fs::validate_relative_path(field, &normalized)?;
    Ok(Some(normalized))
}

/// Move everything inside `from` into `to`, merging with what is already there.
///
/// This is Scoop's `movedir` (`robocopy /e /move`, lib/core.ps1:864). A plain
/// `fs::rename` cannot replace `to` on Windows when it is an existing
/// directory, and `to` is the version directory — which always exists and
/// usually already holds the files of other URLs.
pub(crate) fn merge_move(from: &Path, to: &Path) -> Result<()> {
    if !from.exists() {
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "extract_dir '{}' not found inside the archive",
            from.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| from.display().to_string())
        ))));
    }

    crate::infra::fs::ensure_dir(to)?;

    for entry in walkdir::WalkDir::new(from).min_depth(1) {
        let entry = entry.map_err(std::io::Error::other)?;
        let relative = entry
            .path()
            .strip_prefix(from)
            .map_err(std::io::Error::other)?;
        let target = to.join(relative);

        if entry.file_type().is_dir() {
            crate::infra::fs::ensure_dir(&target)?;
            continue;
        }

        if let Some(parent) = target.parent() {
            crate::infra::fs::ensure_dir(parent)?;
        }
        // Windows refuses to rename onto an existing read-only file, and a
        // directory can sit where a file is about to land.
        if target.is_dir() {
            crate::infra::fs::remove_dir(&target)?;
        } else if target.is_file() || target.is_symlink() {
            std::fs::remove_file(&target)?;
        }

        match std::fs::rename(entry.path(), &target) {
            Ok(()) => {}
            // Different volume, or a handle still open: fall back to a copy.
            Err(_) => {
                std::fs::copy(entry.path(), &target)?;
                std::fs::remove_file(entry.path())?;
            }
        }
    }

    crate::infra::fs::remove_dir(from)?;
    Ok(())
}

fn extract_zip(
    src: &Path,
    dest: &Path,
    extract_dir: Option<&str>,
    root_path: Option<&Path>,
) -> Result<()> {
    let staged = StagedExtraction::new(dest, extract_dir)?;

    match extract_zip_native(src, &staged.staging) {
        Ok(()) => staged.finish(),
        Err(native_error) => {
            // A zip the `zip` crate rejects (ZIP64 oddities, exotic compression
            // methods) is still readable by 7-Zip. Scoop reaches for 7-Zip for
            // every `.zip` when the helper is installed for exactly this reason.
            let Some(helper) = find_7z(root_path) else {
                return Err(native_error);
            };

            tracing::warn!(
                "{}: native unzip failed ({native_error}), retrying with 7-Zip",
                src.display()
            );
            staged.reset()?;
            run_7z(&helper, src, &staged.staging, None)?;
            staged.finish()
        }
    }
}

fn extract_zip_native(src: &Path, dest: &Path) -> Result<()> {
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

/// Extract a `.7z` archive, including self-extracting `.7z.exe` (SFX) archives
/// such as Git for Windows' PortableGit installer — which is why a manifest URL
/// carries `#/dl.7z`: the fragment renames the download to something
/// [`detect_format`] recognises as a 7-Zip archive.
///
/// `external_only` is set for formats no Rust crate handles (`.rar`, `.zst`,
/// `.lzma`, `.iso`, …). Those fail with an actionable message instead of a
/// misleading "BadSignature" from a native parser.
fn extract_7z(
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

fn run_7z_stream_fallback(
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
fn run_7z(helper: &Path, src: &Path, dest: &Path, extract_dir: Option<&str>) -> Result<()> {
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

fn find_7z(root_path: Option<&Path>) -> Option<PathBuf> {
    find_helper("7zip", root_path)
        .or_else(|| find_helper("7z", root_path))
        .or_else(|| find_helper("7-Zip", root_path))
}

async fn extract_tar(src: &Path, dest: &Path) -> Result<()> {
    let file = tokio::fs::File::open(src).await?;
    let reader = tokio::io::BufReader::new(file);

    let decompressed: Box<dyn tokio::io::AsyncRead + Unpin + Send> =
        match src.extension().and_then(|s| s.to_str()) {
            Some("gz") | Some("tgz") => Box::new(tokio::io::BufReader::new(
                async_compression::tokio::bufread::GzipDecoder::new(reader),
            )),
            Some("bz2") | Some("tbz") | Some("tbz2") => Box::new(tokio::io::BufReader::new(
                async_compression::tokio::bufread::BzDecoder::new(reader),
            )),
            Some("xz") | Some("txz") => Box::new(tokio::io::BufReader::new(
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

#[cfg(windows)]
fn extract_msi(
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
fn extract_msi(
    _src: &Path,
    _dest: &Path,
    _extract_dir: Option<&str>,
    _root_path: Option<&Path>,
) -> crate::Result<()> {
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

        extract_zip(&src, &dest, None, None).unwrap();

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

        extract_zip(&src, &dest, None, None).unwrap();

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

            extract_zip(&src, &dest, None, None).unwrap();

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

        extract_zip(&src, &dest, None, None).unwrap();

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
    async fn extract_dir_merges_into_an_existing_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("version");
        std::fs::create_dir_all(&dest).unwrap();
        // What a previous URL of the same package already put there.
        std::fs::write(dest.join("first.txt"), b"from another url").unwrap();

        write_zip(
            &src,
            &[
                ("nested/inner/tool.exe", b"binary"),
                ("nested/inner/share/readme.txt", b"hello"),
                ("unrelated.txt", b"must not be kept"),
            ],
        );

        NativeArchive::new(None)
            .extract_with(&src, &dest, ArchiveFormat::Zip, Some("nested/inner"))
            .await
            .unwrap();

        assert_eq!(std::fs::read(dest.join("tool.exe")).unwrap(), b"binary");
        assert_eq!(
            std::fs::read(dest.join("share/readme.txt")).unwrap(),
            b"hello"
        );
        assert!(
            !dest.join("unrelated.txt").exists(),
            "only the contents of extract_dir belong in the version directory"
        );
        assert_eq!(
            std::fs::read(dest.join("first.txt")).unwrap(),
            b"from another url",
            "the merge must not wipe files that were already there"
        );
        assert!(!dest.join("_tmp").exists(), "_tmp must be removed");
        assert!(
            !dest.join("nested").exists(),
            "the extract_dir path must not survive the move"
        );
    }

    /// Manifests write `extract_dir` with either separator; `PFiles\Gossip\Gossip`
    /// is the real spelling in `extras/gossip.json`.
    #[tokio::test]
    async fn extract_dir_accepts_backslash_separators() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("version");
        write_zip(&src, &[("nested/inner/tool.exe", b"binary")]);

        NativeArchive::new(None)
            .extract_with(&src, &dest, ArchiveFormat::Zip, Some(r"nested\inner"))
            .await
            .unwrap();

        assert!(dest.join("tool.exe").is_file());
        assert!(!dest.join("_tmp").exists());
    }

    /// A manifest that names a directory the archive does not contain is broken,
    /// and silently leaving the files nested one level too deep would produce an
    /// app whose `bin` does not exist. Scoop aborts on `movedir`'s robocopy
    /// error; so does this.
    #[tokio::test]
    async fn a_missing_extract_dir_is_an_error_and_leaves_no_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("version");
        write_zip(&src, &[("elsewhere/tool.exe", b"binary")]);

        let err = NativeArchive::new(None)
            .extract_with(&src, &dest, ArchiveFormat::Zip, Some("nested/inner"))
            .await
            .expect_err("a missing extract_dir must fail the extraction");

        assert!(
            err.to_string().contains("not found"),
            "the error should name the missing directory, got: {err}"
        );
        assert!(!dest.join("_tmp").exists(), "_tmp must be cleaned up");
    }

    /// `extract_dir` is manifest input; it must not be able to move files out of
    /// the version directory.
    #[tokio::test]
    async fn an_extract_dir_escaping_the_destination_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("version");
        write_zip(&src, &[("tool.exe", b"binary")]);

        for evil in ["../escape", r"..\..\Windows", "C:\\Windows"] {
            assert!(
                NativeArchive::new(None)
                    .extract_with(&src, &dest, ArchiveFormat::Zip, Some(evil))
                    .await
                    .is_err(),
                "{evil:?} must be refused"
            );
        }
    }

    /// Two archives into the same destination, the second with its own
    /// `extract_dir`: the first merge must not be undone by the second.
    #[tokio::test]
    async fn consecutive_extractions_accumulate_in_one_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("version");
        let first = tmp.path().join("first.zip");
        let second = tmp.path().join("second.zip");
        write_zip(&first, &[("one/a.txt", b"a")]);
        write_zip(&second, &[("two/b.txt", b"b")]);

        let archive = NativeArchive::new(None);
        archive
            .extract_with(&first, &dest, ArchiveFormat::Zip, Some("one"))
            .await
            .unwrap();
        archive
            .extract_with(&second, &dest, ArchiveFormat::Zip, Some("two"))
            .await
            .unwrap();

        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(dest.join("b.txt")).unwrap(), b"b");
    }

    /// The merge replaces a file that exists in both trees, the way
    /// `robocopy /e /move` does.
    #[test]
    fn merge_move_replaces_colliding_files() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from");
        let to = tmp.path().join("to");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("same.txt"), b"new").unwrap();
        std::fs::write(to.join("same.txt"), b"old").unwrap();

        merge_move(&from, &to).unwrap();

        assert_eq!(std::fs::read(to.join("same.txt")).unwrap(), b"new");
        assert!(!from.exists(), "the source directory is consumed");
    }

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

    /// `Invoke-Extraction` switches on the file name in the version directory,
    /// i.e. `url_filename(url)` — the `#/name` fragment wins over the remote
    /// name. This is the pair that decides how `Setup.exe#/dl.7z` is unpacked.
    #[test]
    fn detection_uses_the_renamed_target_not_the_remote_extension() {
        assert_eq!(detect_format("dl.7z"), Some(ArchiveFormat::SevenZ));
        assert_eq!(
            detect_format("Setup.exe"),
            None,
            "a bare .exe is payload unless the manifest sets innosetup"
        );
        assert_eq!(detect_format("app-win64.exe"), None);
    }

    #[test]
    fn nupkg_is_a_zip() {
        assert_eq!(detect_format("package.nupkg"), Some(ArchiveFormat::Zip));
    }

    /// Scoop copies both of these, and extracting either breaks a real package:
    /// `.cab` files belong next to the `.msi` that references them (`msival2`,
    /// `orca`, `visualc`), and `plantuml` ships a runnable `.jar`.
    #[test]
    fn cab_and_jar_are_payload_not_archives() {
        assert_eq!(detect_format("8e4755178e6b5bcba8d3538c3630b7a5.cab"), None);
        assert_eq!(detect_format("plantuml.jar"), None);
    }

    /// `Test-7zipRequirement` (lib/depends.ps1:134) routes these to 7-Zip, and
    /// no Rust crate reads them.
    #[test]
    fn formats_only_seven_zip_reads_are_marked_external() {
        for name in [
            "tool.zst",
            "archive.tar.zst",
            "tool.lzma",
            "tool.lzh",
            "image.iso",
            "image.img",
            "archive.7z.001",
        ] {
            assert_eq!(
                detect_format(name),
                Some(ArchiveFormat::External7z),
                "{name} needs 7-Zip"
            );
        }
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

    #[cfg(windows)]
    #[tokio::test]
    async fn compound_tar_gz_7z_fallback_unpacks_both_layers_and_honours_extract_dir() {
        let Some(helper) = find_7z(None) else {
            return;
        };

        let tmp = tempfile::tempdir().unwrap();
        let src_tar_gz = tmp.path().join("archive.tar.gz");

        // Create a real .tar.gz with package/bin/app.js
        {
            let tar_gz_file = tokio::fs::File::create(&src_tar_gz).await.unwrap();
            let enc = async_compression::tokio::write::GzipEncoder::new(tar_gz_file);
            let mut tar_builder = tokio_tar::Builder::new(enc);

            let data = b"console.log('hello from app');";
            let mut header = tokio_tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar_builder
                .append_data(&mut header, "package/bin/app.js", &data[..])
                .await
                .unwrap();
            let mut enc = tar_builder.into_inner().await.unwrap();
            tokio::io::AsyncWriteExt::shutdown(&mut enc).await.unwrap();
        }

        let dest = tmp.path().join("dest");
        let staged = StagedExtraction::new(&dest, Some("package")).unwrap();

        run_7z_stream_fallback(
            &helper,
            &src_tar_gz,
            &staged.staging,
            ArchiveFormat::TarGz,
            staged.extract_dir.as_deref(),
        )
        .unwrap();

        staged.finish().unwrap();

        assert_eq!(
            std::fs::read(dest.join("bin").join("app.js")).unwrap(),
            b"console.log('hello from app');"
        );
    }
}
