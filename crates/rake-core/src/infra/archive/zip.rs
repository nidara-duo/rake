use std::path::Path;

use super::sevenz::{find_7z, run_7z};
use super::staging::StagedExtraction;
use crate::Result;

pub(super) fn extract_zip(
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
