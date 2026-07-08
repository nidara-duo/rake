use std::path::PathBuf;

use rake_domain::arch::Arch;
use rake_domain::package::InstallRecord;

use crate::Result;
use crate::infra::fs;
use crate::infra::shortcut::ShortcutEntry;
use crate::infra::{persist, shim, shortcut};
use crate::session::Session;

pub fn reset_packages(
    session: &Session,
    packages: &[String],
) -> Result<Vec<(String, String, PathBuf)>> {
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

    let is_wildcard = packages
        .iter()
        .any(|p| p == "*" || p == "-a" || p == "--all");
    let mut reset = Vec::new();
    let shims_dir = root.join("shims");
    let persist_root = root.join("persist");

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

        // Re-link current junction
        let current_link = app_dir.join("current");
        if current_link.exists() || current_link.is_symlink() {
            fs::remove_symlink(&current_link)?;
        }
        fs::create_junction(&version_dir, &current_link)?;

        // Re-create shims & shortcuts
        let manifest_path = current_link.join("manifest.json");
        if let Ok(content) = std::fs::read_to_string(&manifest_path)
            && let Ok(manifest) = serde_json::from_str::<rake_domain::manifest::Manifest>(&content)
        {
            let arch = load_arch(&version_dir);

            // Re-create shims
            if let Some(bin_val) = manifest.resolve_bin(arch) {
                let entries = shim::parse_bin(bin_val);
                if !entries.is_empty() {
                    fs::ensure_dir(&shims_dir)?;
                    shim::create_shims(&entries, &version_dir, &shims_dir)?;
                }
            }

            // Re-create shortcuts
            if let Some(shortcut_list) = manifest.resolve_shortcuts(arch) {
                let entries: Vec<ShortcutEntry> = shortcut_list
                    .iter()
                    .map(|s| ShortcutEntry {
                        target: s.first().cloned().unwrap_or_default(),
                        name: s.get(1).cloned().unwrap_or_default(),
                        arguments: s.get(2).cloned(),
                        icon: s.get(3).cloned(),
                    })
                    .filter(|e| !e.target.is_empty() && !e.name.is_empty())
                    .collect();
                if !entries.is_empty() {
                    let _ = shortcut::remove_shortcuts(&entries, false);
                    let _ = shortcut::create_shortcuts(&entries, &version_dir, false);
                }
            }

            // Re-apply persist
            if let Some(ref persist_val) = manifest.persist {
                let entries = persist::parse_persist(persist_val);
                let pkg_persist_dir = persist_root.join(&name);
                if !entries.is_empty() {
                    persist::unlink(&entries, &current_link)?;
                    persist::apply(&entries, &version_dir, &pkg_persist_dir)?;
                }
            }

            // Re-apply env vars
            if let Some(env_set) = manifest.resolve_env_set(arch) {
                for k in env_set.keys() {
                    session.env_service().remove_env(k)?;
                }
                for (k, v) in env_set {
                    session.env_service().set_env(k, v)?;
                }
            }
            if let Some(env_add_path) = manifest.resolve_env_add_path(arch) {
                for path in env_add_path.iter() {
                    session.env_service().remove_path(path)?;
                    session.env_service().add_path(path)?;
                }
            }
        }

        reset.push((name, version.unwrap_or_default(), version_dir));
    }

    Ok(reset)
}

fn load_arch(version_dir: &std::path::Path) -> Arch {
    let path = version_dir.join("install.json");
    if let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(info) = serde_json::from_str::<InstallRecord>(&content)
    {
        match info.arch.to_lowercase().as_str() {
            "x86_64" | "amd64" | "x64" | "64bit" => Arch::Amd64,
            "x86" | "i386" | "i686" | "32bit" => Arch::Ia32,
            "aarch64" | "arm64" => Arch::Aarch64,
            _ => Arch::Amd64,
        }
    } else {
        Arch::Amd64
    }
}

fn resolve_installed_version(app_dir: &std::path::Path) -> Option<String> {
    let manifest_path = app_dir.join("current").join("manifest.json");
    if let Ok(content) = std::fs::read_to_string(&manifest_path)
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
