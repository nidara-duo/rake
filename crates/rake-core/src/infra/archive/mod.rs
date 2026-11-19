mod detect;
mod inno;
mod msi;
mod sevenz;
mod staging;
mod tar;
#[cfg(test)]
mod tests;
mod zip;

use std::path::Path;

use async_trait::async_trait;

use crate::Result;

pub use detect::{detect_format, detect_format_for_url};
pub use inno::{extract_innosetup, extract_innosetup_with_script};
pub(crate) use staging::normalize_manifest_dir;

use self::staging::StagedExtraction;

/// Supported archive container and compression formats.
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
    /// Construct a native archive extractor, optionally searching `root` for helper tools.
    pub fn new(root: Option<std::path::PathBuf>) -> Self {
        Self { root }
    }

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
            ArchiveFormat::Zip => zip::extract_zip(src, dest, extract_dir, root),
            ArchiveFormat::SevenZ => sevenz::extract_7z(src, dest, extract_dir, root, false),
            ArchiveFormat::Rar | ArchiveFormat::External7z => {
                sevenz::extract_7z(src, dest, extract_dir, root, true)
            }
            ArchiveFormat::Msi => msi::extract_msi(src, dest, extract_dir, root),
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
                    Err(native_error) => match sevenz::find_7z(root) {
                        Some(helper) => {
                            tracing::warn!(
                                "{}: native extraction failed ({native_error}), retrying with 7-Zip",
                                src.display()
                            );
                            staged.reset()?;
                            sevenz::run_7z_stream_fallback(
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
        | ArchiveFormat::Tar => tar::extract_tar(src, dest).await,
        ArchiveFormat::Gz => tar::extract_gz(src, dest).await,
        ArchiveFormat::Xz => tar::extract_xz(src, dest).await,
        ArchiveFormat::Bz2 => tar::extract_bz2(src, dest).await,
        other => Err(crate::Error::Archive(format!(
            "extract_plain called with {other:?}"
        ))),
    }
}
