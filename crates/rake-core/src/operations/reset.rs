use std::path::PathBuf;

use crate::Result;
use crate::infra::{install_meta, persist};
use crate::operations::install;
use crate::session::Session;

pub fn reset_packages(
    session: &Session,
    packages: &[String],
) -> Result<Vec<(String, String, PathBuf)>> {
    let _guard = session.write_lock()?;
    let roots = install::InstallRoots::from_session(session);
    let apps_root = roots.apps.clone();
    if !apps_root.exists() {
        return Ok(Vec::new());
    }

    let is_wildcard = packages
        .iter()
        .any(|p| p == "*" || p == "-a" || p == "--all");
    let mut reset = Vec::new();
    let shims_dir = roots.shims.clone();
    let persist_root = roots.persist.clone();
    let tx = session.event_bus().core_sender();
    let no_junction = session.config().no_junction.unwrap_or(false);

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

        let matched = is_wildcard || packages.iter().any(|p| name.eq_ignore_ascii_case(p));
        if !matched {
            continue;
        }

        let app_dir = apps_root.join(&name);
        let version = resolve_installed_version(&app_dir);
        let version_dir = match &version {
            Some(v) => app_dir.join(v),
            None => continue,
        };
        if !version_dir.exists() {
            continue;
        }

        // Re-link `current` first: everything below resolves against it, exactly
        // as `install_app` does (lib/install.ps1:58). `link_current` replaces any
        // stale link and honours `no_junction`.
        install::link_current(&version_dir, &app_dir)?;
        let dir = if no_junction {
            version_dir.clone()
        } else {
            app_dir.join("current")
        };

        // The manifest is read through the link, so a reset of an app whose
        // metadata lives under the version directory still works.
        if let Ok(Some(manifest)) = install_meta::read_installed_manifest(&dir) {
            let arch = install_meta::installed_arch(&version_dir);

            // Re-create shims, shortcuts, the module link and environment
            // entries through the same helpers the installer uses. Keeping a
            // second implementation here is how reset drifts away from install.
            let _ = install::apply_shims(&manifest, &dir, &shims_dir, &tx, arch)?;
            let _ = install::apply_shortcuts(&manifest, &dir, arch, &tx)?;
            let _ = install::install_psmodule(session, &manifest, &dir, &roots)?;
            install::apply_env(&manifest, session, arch, &dir)?;

            // Persist: unlink first, then re-apply. `persist::apply` moves a
            // shipped copy aside to `<name>.original` when the target already
            // exists, so applying without unlinking would rename the *junction*
            // and leave a `.original` link behind on every reset.
            if let Some(ref persist_val) = manifest.persist {
                let entries = persist::parse_persist(persist_val);
                if !entries.is_empty() {
                    persist::unlink(&entries, &version_dir)?;
                    persist::apply(&entries, &version_dir, &persist_root.join(&name))?;
                }
            }
        }

        reset.push((name, version.unwrap_or_default(), version_dir));
    }

    Ok(reset)
}

fn resolve_installed_version(app_dir: &std::path::Path) -> Option<String> {
    let current_dir = app_dir.join("current");
    let manifest_path = crate::infra::install_meta::INSTALLED_MANIFEST;
    let manifest_path = if current_dir.join(manifest_path).is_file() {
        current_dir.join(manifest_path)
    } else {
        current_dir.join(crate::infra::install_meta::INSTALLED_MANIFEST_LEGACY)
    };
    if let Ok(content) = crate::infra::json::read_to_string(&manifest_path)
        && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(ver) = manifest.get("version").and_then(|v| v.as_str())
    {
        let ver_dir = app_dir.join(ver);
        if ver_dir.exists() {
            return Some(ver.to_owned());
        }
    }

    let entries = std::fs::read_dir(app_dir).ok()?;
    let mut dirs: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "current" { None } else { Some(name) }
        })
        .collect();

    dirs.sort();
    dirs.last().cloned()
}
