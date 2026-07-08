use std::path::{Path, PathBuf};

use crate::Result;
use crate::infra::fs;
use crate::infra::persist;
use crate::operations::cache;
use crate::session::Session;

#[derive(Debug, Clone, Copy)]
pub enum CleanupOption {
    Cache,
}

pub struct CleanupResult {
    pub name: String,
    pub removed_versions: Vec<String>,
}

pub fn cleanup_packages(
    session: &Session,
    names: &[String],
    options: &[CleanupOption],
) -> Result<Vec<CleanupResult>> {
    let _guard = session.write_lock()?;
    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("apps"));

    let apps_root = root.join("apps");
    if !apps_root.exists() {
        return Ok(Vec::new());
    }

    if options.iter().any(|o| matches!(o, CleanupOption::Cache)) {
        // Based on scoop: cleanup cache
        // Note: Full cache cleanup logic might need to be refined if it
        // should only clean up for specific apps if names are provided,
        // but Scoop's implementation handles cache globally or per-app context.
        // For now, let's keep it simple.
        cache::cache_remove(session, "*")?;
    }

    let is_wildcard = names.iter().any(|p| p == "*" || p == "-a" || p == "--all");
    let mut results = Vec::new();

    let entries: Vec<_> = std::fs::read_dir(&apps_root)
        .map_err(crate::Error::Io)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "scoop" {
            continue;
        }

        let matched = is_wildcard || names.iter().any(|p| name.eq_ignore_ascii_case(p));
        if !matched {
            continue;
        }

        let app_dir = apps_root.join(&name);
        let current_link = app_dir.join("current");

        let current_version = if current_link.exists() || current_link.is_symlink() {
            std::fs::read_link(&current_link)
                .ok()
                .and_then(|p| p.file_name().and_then(|s| s.to_str()).map(|s| s.to_owned()))
        } else {
            None
        };

        let mut removed_versions = Vec::new();
        if let Ok(version_entries) = std::fs::read_dir(&app_dir) {
            for ve in version_entries.flatten() {
                let vname = ve.file_name().to_string_lossy().to_string();
                if vname == "current" || Some(&vname) == current_version.as_ref() {
                    continue;
                }

                // version directory
                let vdir = ve.path();
                if vdir.is_dir() {
                    // Scoop logic: unlink persist data before removing dir
                    let manifest = load_manifest(&app_dir.join(&vname));
                    if let Some(ref m) = manifest
                        && let Some(ref persist_val) = m.persist
                    {
                        let entries = persist::parse_persist(persist_val);
                        let _ = persist::unlink(&entries, &vdir);
                    }

                    let _ = fs::remove_dir(&vdir);
                    removed_versions.push(vname);
                }
            }
        }

        if !removed_versions.is_empty() {
            results.push(CleanupResult {
                name,
                removed_versions,
            });
        }
    }

    Ok(results)
}

fn load_manifest(version_dir: &Path) -> Option<rake_domain::manifest::Manifest> {
    let path = version_dir.join("manifest.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}
