use std::path::PathBuf;

use rake_domain::arch::Arch;
use rake_domain::package::InstallRecord;

use crate::Result;
use crate::infra::fs;
use crate::infra::shortcut::ShortcutEntry;
use crate::infra::{persist, script, shim, shortcut};
use crate::session::Session;

pub struct Uninstalled {
    pub name: String,
    pub version: String,
}

pub fn uninstall_packages(
    session: &Session,
    names: &[String],
    purge: bool,
) -> Result<Vec<Uninstalled>> {
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

    let is_wildcard = names.iter().any(|p| p == "*" || p == "-a" || p == "--all");
    let mut result = Vec::new();
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

        let matched = is_wildcard || names.iter().any(|p| name.eq_ignore_ascii_case(p));
        if !matched {
            continue;
        }

        let app_dir = apps_root.join(&name);
        let version = resolve_version(&app_dir);
        let version_dir = match &version {
            Some(v) => app_dir.join(v),
            None => continue,
        };
        let manifest = load_manifest(&version_dir);
        let arch = load_arch(&version_dir);

        // 1. Run pre_uninstall script
        if let Some(ref m) = manifest
            && let Some(script_lines) = m.resolve_pre_uninstall(arch)
        {
            let ctx = script::HookContext {
                version_dir: &version_dir,
                persist_dir: &persist_root.join(&name),
                original_dir: &version_dir,
                version: version.as_deref().unwrap_or(""),
            };
            let _ = script::run_powershell_script(
                &script_lines.iter().cloned().collect::<Vec<_>>(),
                &ctx,
            );
        }

        // 2. Remove shims
        if let Some(ref m) = manifest
            && let Some(bin_val) = m.resolve_bin(arch)
        {
            let entries = shim::parse_bin(bin_val);
            shim::remove_shims(&entries, &shims_dir)?;
        }

        // 3. Remove env vars
        if let Some(ref m) = manifest {
            if let Some(env_set) = m.resolve_env_set(arch) {
                for k in env_set.keys() {
                    session.env_service().remove_env(k)?;
                }
            }
            if let Some(env_add_path) = m.resolve_env_add_path(arch) {
                for path in env_add_path.iter() {
                    session.env_service().remove_path(path)?;
                }
            }
        }

        // 4. Remove shortcuts
        if let Some(ref m) = manifest
            && let Some(shortcut_list) = m.resolve_shortcuts(arch)
        {
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
            }
        }

        // 5. Unlink persist junctions
        if let Some(ref m) = manifest
            && let Some(ref persist_val) = m.persist
        {
            let entries = persist::parse_persist(persist_val);
            persist::unlink(&entries, &app_dir.join("current"))?;
            if let Ok(version_entries) = std::fs::read_dir(&app_dir) {
                for ve in version_entries.flatten() {
                    let vpath = ve.path();
                    if vpath.is_dir()
                        && vpath.file_name().and_then(|s| s.to_str()) != Some("current")
                    {
                        persist::unlink(&entries, &vpath)?;
                    }
                }
            }
        }

        // 6. Remove current junction
        let current_link = app_dir.join("current");
        if current_link.exists() || current_link.is_symlink() {
            fs::remove_symlink(&current_link)?;
        }

        // 7. Remove all version directories
        if let Ok(version_entries) = std::fs::read_dir(&app_dir) {
            for ve in version_entries.flatten() {
                let path = ve.path();
                if path.is_dir() {
                    let _ = fs::remove_dir(&path);
                }
            }
        }

        // 8. Remove app directory itself
        let _ = fs::remove_dir(&app_dir);

        // 9. Run post_uninstall script (after removal, but persist dir still exists)
        if let Some(ref m) = manifest
            && let Some(script_lines) = m.resolve_post_uninstall(arch)
        {
            let persist_dir = persist_root.join(&name);
            if persist_dir.exists() {
                // Keep persist_dir alive for script to use
                let ctx = script::HookContext {
                    version_dir: &version_dir,
                    persist_dir: &persist_dir,
                    original_dir: &version_dir,
                    version: version.as_deref().unwrap_or(""),
                };
                let _ = script::run_powershell_script(
                    &script_lines.iter().cloned().collect::<Vec<_>>(),
                    &ctx,
                );
            }
        }

        // 10. Purge persisted data
        if purge {
            let pkg_persist_dir = persist_root.join(&name);
            if pkg_persist_dir.exists() {
                fs::remove_dir(&pkg_persist_dir)?;
            }
        }

        result.push(Uninstalled {
            name,
            version: version.unwrap_or_default(),
        });
    }

    Ok(result)
}

fn resolve_version(app_dir: &std::path::Path) -> Option<String> {
    let manifest_path = app_dir.join("current").join("manifest.json");
    if let Ok(content) = std::fs::read_to_string(&manifest_path)
        && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(ver) = manifest.get("version").and_then(|v| v.as_str())
    {
        return Some(ver.to_owned());
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

fn load_manifest(version_dir: &std::path::Path) -> Option<rake_domain::manifest::Manifest> {
    let path = version_dir.join("manifest.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
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
