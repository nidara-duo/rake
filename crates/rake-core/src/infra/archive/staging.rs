use std::path::{Path, PathBuf};

use crate::Result;

/// One extraction destination, plus the `extract_dir` merge that has to follow.
///
/// When `extract_dir` is set the archive is unpacked into `<dest>\_tmp` and the
/// contents of `_tmp\<extract_dir>` are merged into `dest` afterwards. Without
/// it the archive goes straight into `dest`.
pub(super) struct StagedExtraction {
    pub(super) staging: PathBuf,
    pub(super) dest: PathBuf,
    pub(super) extract_dir: Option<String>,
}

impl StagedExtraction {
    pub(super) fn new(dest: &Path, extract_dir: Option<&str>) -> Result<Self> {
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
    pub(super) fn reset(&self) -> Result<()> {
        if self.extract_dir.is_some() {
            crate::infra::fs::remove_dir(&self.staging)?;
            crate::infra::fs::ensure_dir(&self.staging)?;
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<()> {
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
