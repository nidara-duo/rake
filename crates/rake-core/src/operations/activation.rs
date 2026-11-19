use std::path::{Path, PathBuf};

use rake_domain::arch::Arch;
use rake_domain::manifest::Manifest;

use crate::Result;
use crate::event::Event;
use crate::infra::fs;
use crate::infra::layout::InstallRoots;
use crate::infra::shortcut::ShortcutEntry;
use crate::infra::{env, persist, shim, shortcut};
use crate::session::Session;

pub(crate) fn apply_persistence(
    manifest: &Manifest,
    app_name: &str,
    version_dir: &Path,
    persist_root: &Path,
) -> Result<Vec<persist::PersistEntry>> {
    let Some(persist_val) = manifest.persist.as_ref() else {
        return Ok(Vec::new());
    };
    let entries = persist::parse_persist(persist_val);
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    persist::apply(&entries, version_dir, &persist_root.join(app_name))?;
    Ok(entries)
}

/// Create the manifest's `bin` shims and return the names that were written, so
/// a failed install can take them away again.
pub(crate) fn apply_shims(
    manifest: &Manifest,
    dir: &Path,
    shims_dir: &Path,
    tx: &flume::Sender<Event>,
    arch: Arch,
) -> Result<Vec<String>> {
    let Some(bin_val) = manifest.resolve_bin(arch) else {
        return Ok(Vec::new());
    };
    let entries = shim::parse_bin(bin_val);
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    for entry in &entries {
        let _ = tx.try_send(Event::CommitProgress(format!(
            "Creating shim for '{}'.",
            entry.name
        )));
    }
    fs::ensure_dir(shims_dir)?;
    shim::create_shims(&entries, dir, shims_dir)
}

/// Create the manifest's Start Menu shortcuts, returning them for rollback.
pub(crate) fn apply_shortcuts(
    manifest: &Manifest,
    dir: &Path,
    arch: Arch,
    tx: &flume::Sender<Event>,
) -> Result<Vec<ShortcutEntry>> {
    let Some(list) = manifest.resolve_shortcuts(arch) else {
        return Ok(Vec::new());
    };
    let entries = parse_shortcuts(list);
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let warnings = shortcut::create_shortcuts(&entries, dir, false)?;
    for w in warnings {
        let _ = tx.try_send(Event::CommitProgress(format!("⚠ {}", w)));
    }
    Ok(entries)
}

/// `[target, name, arguments, icon]` from a manifest's `shortcuts` array.
pub(crate) fn parse_shortcuts(list: &[Vec<String>]) -> Vec<ShortcutEntry> {
    list.iter()
        .map(|s| ShortcutEntry {
            target: s.first().cloned().unwrap_or_default(),
            name: s.get(1).cloned().unwrap_or_default(),
            arguments: s.get(2).cloned(),
            icon: s.get(3).cloned(),
        })
        .filter(|e| !e.target.is_empty() && !e.name.is_empty())
        .collect()
}

/// Link a PowerShell module into `modules/<name>` and make sure that directory
/// is on `PSModulePath` (lib/psmodules.ps1:1).
pub(crate) fn install_psmodule(
    session: &Session,
    manifest: &Manifest,
    dir: &Path,
    roots: &InstallRoots,
) -> Result<Option<PathBuf>> {
    let Some(module) = manifest.resolve_psmodule() else {
        return Ok(None);
    };
    let Some(name) = module
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    else {
        return Err(crate::Error::Custom(
            "Invalid manifest: The 'name' property is missing from 'psmodule'.".to_owned(),
        ));
    };
    // The name is manifest input and becomes a directory under `modules/`.
    fs::validate_relative_path("psmodule name", name)?;

    fs::ensure_dir(&roots.modules)?;
    let link = roots.modules.join(name);
    if link.exists() || link.is_symlink() {
        fs::remove_symlink(&link)?;
    }
    fs::create_junction(dir, &link)?;

    ensure_psmodule_path(session, &roots.modules)?;
    Ok(Some(link))
}

/// Remove the module link an install created, leaving the version directory
/// alone (lib/psmodules.ps1:27).
///
/// The `modules` directory itself stays: it is shared by every module, and
/// Scoop leaves it in place. Only when it has become empty is the directory
/// dropped from `PSModulePath` again — a path entry pointing at a directory
/// with nothing in it is dead weight that no other app will ever remove.
pub(crate) fn uninstall_psmodule(
    session: &Session,
    manifest: &Manifest,
    roots: &InstallRoots,
) -> Result<Option<PathBuf>> {
    let Some(module) = manifest.resolve_psmodule() else {
        return Ok(None);
    };
    let Some(name) = module
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    else {
        return Ok(None);
    };
    fs::validate_relative_path("psmodule name", name)?;

    let link = roots.modules.join(name);
    if !link.exists() && !link.is_symlink() {
        return Ok(None);
    }
    fs::remove_symlink(&link)?;

    let modules_empty = std::fs::read_dir(&roots.modules)
        .map(|entries| entries.filter_map(|e| e.ok()).next().is_none())
        .unwrap_or(false);
    if modules_empty {
        forget_psmodule_path(session, &roots.modules)?;
    }

    Ok(Some(link))
}

/// Add the modules directory to the persisted user `PSModulePath`.
///
/// Scoop prepends it and only when the value does not already mention it
/// (`psmodules.ps1:44`). The value is read from the registry, not from the
/// process, for the same reason PATH is: the process copy is the merged value
/// and writing it back would fold the machine-wide entries into the user's own.
fn ensure_psmodule_path(session: &Session, modules_dir: &Path) -> Result<()> {
    let dir = modules_dir.to_string_lossy().to_string();
    let current = env::read_user_env("PSModulePath")?.unwrap_or_else(|| {
        // Scoop's fallback when the variable does not exist yet. Expanded here
        // rather than stored as `%USERPROFILE%`, because the value is written
        // back as a plain string.
        std::env::var("USERPROFILE")
            .map(|home| format!(r"{home}\Documents\WindowsPowerShell\Modules"))
            .unwrap_or_default()
    });

    if current.to_lowercase().contains(&dir.to_lowercase()) {
        return Ok(());
    }

    let value = if current.is_empty() {
        dir
    } else {
        format!("{dir};{current}")
    };
    session.env_service().set_env("PSModulePath", &value)
}

/// Drop the modules directory from the persisted user `PSModulePath`.
///
/// The entry was prepended by [`ensure_psmodule_path`], so it is the first
/// `;`-separated element in the ordinary case; the position is not relied on,
/// and every occurrence is removed. An entry that is not there is not an error:
/// uninstalling a package must not fail because the user already cleaned up.
fn forget_psmodule_path(session: &Session, modules_dir: &Path) -> Result<()> {
    let dir = modules_dir.to_string_lossy().to_string();
    let Some(current) = env::read_user_env("PSModulePath")? else {
        return Ok(());
    };

    let kept: Vec<&str> = current
        .split(';')
        .filter(|entry| !entry.is_empty() && !entry.eq_ignore_ascii_case(&dir))
        .collect();
    let original: Vec<&str> = current.split(';').filter(|e| !e.is_empty()).collect();

    if kept.len() == original.len() {
        return Ok(());
    }

    if kept.is_empty() {
        session.env_service().remove_env("PSModulePath")
    } else {
        session
            .env_service()
            .set_env("PSModulePath", &kept.join(";"))
    }
}

/// Mirror of scoop's `is_in_dir` (lib/core.ps1:695).
///
/// Answers whether `check` is `dir` itself or lives inside it. The trailing separator
/// is made explicit before the prefix test — without it `C:\apps\bc` would pass as
/// being inside `C:\apps\b`.
pub(crate) fn is_in_dir(dir: &Path, check: &Path) -> bool {
    let dir = dir.as_os_str().to_string_lossy().into_owned();
    let check = check.as_os_str().to_string_lossy().into_owned();

    if dir.to_lowercase() == check.to_lowercase() {
        return true;
    }

    let prefix = if dir.ends_with(['\\', '/']) {
        dir.to_lowercase()
    } else {
        format!("{}\\", dir.to_lowercase())
    };
    check.to_lowercase().starts_with(&prefix)
}

pub(crate) fn apply_env(
    manifest: &Manifest,
    session: &Session,
    arch: Arch,
    dir: &Path,
) -> Result<()> {
    if let Some(env_set) = manifest.resolve_env_set(arch) {
        for (k, v) in env_set {
            session.env_service().set_env(k, v)?;
        }
    }
    for path in resolve_env_add_paths(manifest, arch, dir) {
        // Not `EnvService::add_path`: that one reads the *process* PATH and writes the
        // result into HKCU\Environment\PATH, folding every machine-wide entry into the
        // user's own value on each run and downgrading REG_EXPAND_SZ to REG_SZ.
        env::add_user_path(&path)?;
    }
    Ok(())
}

/// Resolve a manifest's `env_add_path` entries against the app directory.
///
/// Scoop resolves each entry as `Join-Path $dir $_ | Get-AbsolutePath` and keeps only
/// the results `is_in_dir` accepts (lib/install.ps1:316). Handing the raw manifest
/// string to the PATH instead — as this used to — added a literal `bin` rather than
/// `<app>/current/bin`.
///
/// `Path::join` replaces the entire path when `entry` is absolute, so an absolute entry
/// lands outside `dir` and is dropped here instead of putting an arbitrary directory on
/// PATH. (PowerShell's `Join-Path` concatenates instead, so scoop relies entirely on
/// the `is_in_dir` filter there; Rust's behaviour is the stricter of the two.)
///
/// Shared by installation and removal so the two cannot drift. Adding
/// `<app>/current/bin` while removing the literal `bin` would leave the entry on PATH
/// forever, which is what the previous split implementation did.
pub(crate) fn resolve_env_add_paths(manifest: &Manifest, arch: Arch, dir: &Path) -> Vec<PathBuf> {
    let Some(env_add_path) = manifest.resolve_env_add_path(arch) else {
        return Vec::new();
    };
    env_add_path
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            // Scoop manifests such as `nodejs` use `"."` to put `<app>/current` itself
            // on PATH (`Join-Path $dir "." | Get-AbsolutePath` == `$dir`).
            let stripped = entry
                .strip_prefix("./")
                .or_else(|| entry.strip_prefix(".\\"))
                .unwrap_or(entry);
            if stripped == "." || stripped.is_empty() {
                return Some(dir.to_path_buf());
            }
            // The name comes from the manifest and is joined onto `dir`, so it has to clear
            // the same guard as `bin` and `persist`: `..` or an absolute path in it would
            // otherwise place a PATH entry anywhere on the disk.
            crate::infra::fs::validate_relative_path("env_add_path", stripped)
                .ok()
                .map(|()| dir.join(stripped))
        })
        .filter(|joined| is_in_dir(dir, joined))
        .collect()
}

/// Drop a manifest's `env_add_path` entries from the persisted user PATH.
///
/// Best effort, like scoop: a missing PATH entry is not an error worth failing an
/// uninstall over.
pub(crate) fn remove_env_add_paths(manifest: &Manifest, arch: Arch, dir: &Path) {
    for path in resolve_env_add_paths(manifest, arch, dir) {
        let _ = env::remove_user_path(&path);
    }
}

/// Create the `current` junction pointing at the freshly installed version directory.
///
/// Split out of `finalize_installation` because scoop creates the link *before* writing
/// shims, shortcuts and PATH (lib/install.ps1:58) and then rebinds `$dir` to it, so all
/// three reference the junction. See the call site for the full rationale.
pub(crate) fn link_current(version_dir: &Path, app_dir: &Path) -> Result<()> {
    let current_link = app_dir.join("current");
    fs::remove_symlink(&current_link)?;

    #[cfg(windows)]
    fs::create_junction(version_dir, &current_link)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(version_dir, &current_link)?;

    Ok(())
}

/// Tear down the active links of an installed version (`persist`, shims,
/// shortcuts, PowerShell module, environment entries, and the `current` junction),
/// while keeping `version_dir` itself intact on disk.
pub(crate) fn deactivate_links(
    session: &Session,
    manifest: &Manifest,
    arch: Arch,
    app_dir: &Path,
    version_dir: &Path,
    roots: &InstallRoots,
    tx: &flume::Sender<Event>,
) -> Result<()> {
    // 1. Unlink persist before anything touches the directory.
    if let Some(ref persist_val) = manifest.persist {
        let entries = persist::parse_persist(persist_val);
        let _ = persist::unlink(&entries, version_dir);
    }

    // 2. Shims, shortcuts and the module link owned by this version.
    if let Some(bin_val) = manifest.resolve_bin(arch) {
        let entries = shim::parse_bin(bin_val);
        let _ = shim::remove_shims(&entries, &roots.shims);
    }
    if let Some(list) = manifest.resolve_shortcuts(arch) {
        let entries = parse_shortcuts(list);
        if !entries.is_empty() {
            let _ = shortcut::remove_shortcuts(&entries, false);
        }
    }
    let _ = uninstall_psmodule(session, manifest, roots);

    // 3. Environment entries added by the manifest.
    if let Some(env_set) = manifest.resolve_env_set(arch) {
        for key in env_set.keys() {
            let _ = session.env_service().remove_env(key);
        }
    }
    remove_env_add_paths(manifest, arch, &app_dir.join("current"));

    // 4. `current` itself.
    if !session.config().no_junction.unwrap_or(false) {
        let _ = tx.try_send(Event::UpdateProgress("Unlinking current...".to_owned()));
        fs::remove_symlink(&app_dir.join("current"))?;
    }

    Ok(())
}

/// Re-activate links for an existing version directory (`current` junction,
/// shims, shortcuts, PowerShell module, environment entries, and `persist`).
///
/// When `strict` is true (used by `reset`), any failure aborts immediately.
/// When `strict` is false (used when rolling back a failed update), step failures
/// after linking `current` are best-effort so as much of the old version as
/// possible is restored.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reactivate_links(
    session: &Session,
    manifest: &Manifest,
    app_name: &str,
    arch: Arch,
    app_dir: &Path,
    version_dir: &Path,
    roots: &InstallRoots,
    tx: &flume::Sender<Event>,
    strict: bool,
) -> Result<()> {
    let no_junction = session.config().no_junction.unwrap_or(false);
    if !no_junction {
        link_current(version_dir, app_dir)?;
    }
    let dir = if no_junction {
        version_dir.to_path_buf()
    } else {
        app_dir.join("current")
    };

    if strict {
        let _ = apply_shims(manifest, &dir, &roots.shims, tx, arch)?;
        let _ = apply_shortcuts(manifest, &dir, arch, tx)?;
        let _ = install_psmodule(session, manifest, &dir, roots)?;
        apply_env(manifest, session, arch, &dir)?;
    } else {
        let _ = apply_shims(manifest, &dir, &roots.shims, tx, arch);
        let _ = apply_shortcuts(manifest, &dir, arch, tx);
        let _ = install_psmodule(session, manifest, &dir, roots);
        let _ = apply_env(manifest, session, arch, &dir);
    }

    // Persist: unlink first, then re-apply. `persist::apply` moves a shipped
    // copy aside to `<name>.original` when the target already exists, so
    // applying without unlinking would rename the *junction* and leave a
    // `.original` link behind on every reset.
    if let Some(ref persist_val) = manifest.persist {
        let entries = persist::parse_persist(persist_val);
        if !entries.is_empty() {
            let persist_dir = roots.persist.join(app_name);
            if strict {
                persist::unlink(&entries, version_dir)?;
                persist::apply(&entries, version_dir, &persist_dir)?;
            } else {
                let _ = persist::unlink(&entries, version_dir);
                let _ = persist::apply(&entries, version_dir, &persist_dir);
            }
        }
    }

    Ok(())
}
