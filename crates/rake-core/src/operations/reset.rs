use std::path::PathBuf;

use crate::Result;
use crate::infra::install_meta;
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

        let layout = roots.app_layout(&name);
        let Some(version_dir) = layout.resolve_installed_version_dir(None) else {
            continue;
        };
        if !version_dir.exists() {
            continue;
        }

        let version_name = version_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let dir = layout.active_dir(no_junction, version_name);

        let manifest = install_meta::read_installed_manifest(&version_dir)
            .ok()
            .flatten()
            .or_else(|| install_meta::read_installed_manifest(&dir).ok().flatten());
        let version = manifest
            .as_ref()
            .map(|m| m.version().to_owned())
            .unwrap_or_else(|| version_name.to_owned());

        // Re-link `current` and rebuild shims, shortcuts, module link, env
        // and persist strictly through the shared activation helper.
        if let Some(manifest) = manifest {
            let arch = install_meta::installed_arch(&version_dir);
            install::reactivate_links(
                session,
                &manifest,
                &name,
                arch,
                &layout.app_dir,
                &version_dir,
                &roots,
                &tx,
                true,
            )?;
        } else if !no_junction {
            install::link_current(&version_dir, &layout.app_dir)?;
        }

        reset.push((name, version, version_dir));
    }

    Ok(reset)
}
