use std::path::Path;

use crate::Result;

#[derive(Debug, Clone)]
pub struct PersistEntry {
    pub source: String,
    pub target: String,
}

pub fn parse_persist(
    persist: &rake_domain::one_or_many::OneOrMany<rake_domain::one_or_many::OneOrMany<String>>,
) -> Vec<PersistEntry> {
    let mut entries = Vec::new();
    for item in persist.iter() {
        let parts: Vec<&String> = item.iter().collect();
        if parts.is_empty() {
            continue;
        }
        let source = parts[0].clone();
        let target = parts
            .get(1)
            .map(|s| (*s).clone())
            .unwrap_or_else(|| source.clone());
        entries.push(PersistEntry { source, target });
    }
    entries
}

/// Apply persist: create junctions (dirs) or hardlinks (files) from app_dir/<source> ← persist_dir/<target>.
pub fn apply(entries: &[PersistEntry], app_dir: &Path, persist_dir: &Path) -> Result<()> {
    for entry in entries {
        let source = app_dir.join(&entry.source);
        let target = persist_dir.join(&entry.target);

        if target.exists() {
            // Data exists from previous install — link it back
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            let _ = crate::infra::fs::remove_symlink(&source);
            link_path(&target, &source)?;
        } else if source.exists() {
            // First install — move existing data to persist, then link
            if let Some(parent) = target.parent() {
                crate::infra::fs::ensure_dir(parent)?;
            }
            std::fs::rename(&source, &target)?;
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            link_path(&target, &source)?;
        } else {
            // Neither exists — create target dir, link empty dir
            crate::infra::fs::ensure_dir(&target)?;
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            link_path(&target, &source)?;
        }
    }
    Ok(())
}

/// Unlink persist junctions/symlinks/hardlinks (remove whatever is at source).
pub fn unlink(entries: &[PersistEntry], app_dir: &Path) -> Result<()> {
    for entry in entries {
        let source = app_dir.join(&entry.source);
        if source.exists() || source.is_symlink() {
            crate::infra::fs::remove_symlink(&source)?;
        }
    }
    Ok(())
}

/// Create a junction (dir) or hardlink (file) from target → source.
fn link_path(target: &Path, source: &Path) -> Result<()> {
    if target.is_dir() {
        crate::infra::fs::create_junction(target, source)?;
    } else {
        std::fs::hard_link(target, source)?;
    }
    Ok(())
}
