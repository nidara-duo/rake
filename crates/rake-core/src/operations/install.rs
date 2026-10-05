use std::path::{Path, PathBuf};

use rake_domain::arch::Arch;
use rake_domain::manifest::Manifest;
use rake_domain::package::{InstallRecord, InstallState, Package, PackageSource, PackageStatus};

use crate::Result;
use crate::event::Event;
use crate::infra::archive::{ArchiveService, detect_format_for_url, extract_innosetup};
use crate::infra::fs;
use crate::infra::install_meta;
use crate::infra::shortcut::ShortcutEntry;
use crate::infra::{env, persist, script, shim, shortcut};
use crate::operations::download::DownloadedFile;
use crate::session::Session;

/// Re-exported so callers (`update`) share the single canonical type
/// rather than redefining a private `InstallInfo`.
pub type InstallInfo = InstallRecord;

/// Commit downloaded files — extract, link, shim, persist.
/// Does NOT download — caller is responsible for providing DownloadedFile list.
pub async fn install_packages(
    session: &Session,
    packages: &[Package],
    files: &[DownloadedFile],
    arch: Arch,
) -> Result<Vec<Package>> {
    let mut installed = Vec::new();
    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("apps"));
    let shims_dir = root.join("shims");
    let persist_root = root.join("persist");
    let global_root = session
        .config()
        .global_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\rake"));
    let tx = session.event_bus().core_sender();

    for pkg in packages {
        let _ = tx.try_send(Event::CommitStart(
            pkg.ident.clone(),
            pkg.version().to_string(),
        ));

        let pkg_files: Vec<&DownloadedFile> =
            files.iter().filter(|f| f.ident == pkg.ident).collect();

        let version_dir = apps_version_dir(session, pkg);
        let app_dir = apps_dir(session, pkg);
        let pkg_persist_dir = persist_root.join(pkg.name());

        if version_dir.exists() {
            if install_meta::is_installed(&version_dir) {
                return Err(crate::Error::Domain(
                    rake_domain::Error::PackageAlreadyExists(pkg.name().to_owned()),
                ));
            }
            // Partial install — purge and retry
            let _ = fs::remove_dir(&version_dir);
        }

        // 1. Extract / copy files
        let extract_to = pkg
            .manifest
            .resolve_extract_to(arch)
            .map(|et| et.as_slice().to_vec());
        for (i, file) in pkg_files.iter().enumerate() {
            let target_dir = match extract_to.as_ref().and_then(|et| et.get(i)) {
                Some(sub) => version_dir.join(sub),
                None => version_dir.clone(),
            };

            // Resolve archive format from the manifest URL, not from the
            // cache file's own path — the URL may carry a `#/name.ext`
            // fragment declaring the true format (e.g. a self-extracting
            // `.7z.exe`), which cache-path-based detection cannot see.
            let extraction_format = detect_format_for_url(&file.url);

            // InnoSetup extraction is gated on the `innosetup` flag ALONE.
            //
            // `installer.script` is not an alternative marker for it. Scoop treats that
            // field as a PowerShell hook run after extraction: `lib/decompress.ps1`
            // selects InnoSetup solely via `if ($Manifest.innosetup)` for `.exe`
            // downloads, and `Expand-InnoArchive` never hands a script to innounp
            // (its argument list is fixed: -x -d<dest> <path> -y -c<dir>).
            //
            // Coupling the two sent plain PowerShell installer scripts into innounp.
            // In the main bucket that misrouted 37 of the 38 manifests that carry
            // `installer.script`, leaving only the single one that also sets
            // `innosetup` working by accident.
            let is_innosetup = pkg.manifest.innosetup == Some(true);

            if is_innosetup && extraction_format.is_none() {
                let _ = tx.try_send(Event::CommitProgress(format!(
                    "Extracting {} ...",
                    file.url.split('/').next_back().unwrap_or("file"),
                )));
                let ed = pkg.manifest.resolve_extract_dir(arch);
                let ed_str = ed.and_then(|ed| ed.as_slice().first().map(|s| s.as_str()));
                extract_innosetup(
                    &file.cache_path,
                    &target_dir,
                    session.config().root_path.as_deref(),
                    ed_str,
                )?;
            } else if let Some(format) = extraction_format {
                let archive =
                    crate::infra::archive::NativeArchive::new(session.config().root_path.clone());
                archive
                    .extract(&file.cache_path, &target_dir, format)
                    .await?;
            } else {
                let dest = target_dir.join(file.url.split('/').next_back().unwrap_or("file"));
                fs::ensure_dir(&target_dir)?;
                std::fs::copy(&file.cache_path, &dest)?;
            }
        }

        // 2. Apply extract_dir
        apply_extract_dir(pkg, &version_dir, arch, &tx)?;

        // 3. Run pre_install
        if let Some(script_lines) = pkg.manifest.resolve_pre_install(arch) {
            let ctx = script::HookContext::new(
                &version_dir,
                &pkg_persist_dir,
                &version_dir,
                pkg.version(),
            );
            script::run_powershell_script(&script_lines.iter().cloned().collect::<Vec<_>>(), &ctx)?;
        }

        // 4. Run the installer hook. `installer.script` is a PowerShell script that
        // runs after extraction, reached in scoop via `Invoke-HookScript -HookType
        // 'installer'` (lib/install.ps1:159-167) — still before `current` exists, so
        // `$dir` is the version directory, same as scoop.
        if let Some(script_lines) = pkg
            .manifest
            .resolve_installer(arch)
            .and_then(|i| i.script.as_ref())
        {
            // `installer.script` bodies call scoop's own functions, so the library has
            // to be loaded. The other hooks do not, and paying ~0.7s each for it is
            // not worth it.
            let ctx = script::HookContext::with_scoop_lib(
                &version_dir,
                &pkg_persist_dir,
                &version_dir,
                pkg.version(),
                &root,
                &global_root,
            );
            script::run_powershell_script(&script_lines.iter().cloned().collect::<Vec<_>>(), &ctx)?;
        }

        // Create the `current` junction BEFORE shims, shortcuts and PATH, mirroring
        // scoop's lib/install.ps1:58-63. Everything below references the junction and
        // not the version directory: that indirection is the whole point of the
        // junction, since a later update only has to repoint `current` for shims,
        // shortcuts and PATH to follow. Creating it last pointed every shim at
        // `apps/<name>/<version>`, so the link bought nothing, broke scoop
        // interoperability, and deleting an old version directory stranded its shims.
        //
        // The lock covers the junction swap alone and is released before the steps
        // below: shim creation is several file writes, and post_install can shell out
        // to PowerShell for seconds, neither of which should be serialised.
        {
            let _guard = session.write_lock()?;
            link_current(&version_dir, &app_dir)?;
        }
        let current_dir = app_dir.join("current");

        // 5. Create shims
        apply_shims(pkg, &current_dir, &shims_dir, &tx, arch)?;

        // 6. Create Start Menu shortcuts
        if let Some(shortcut_list) = pkg.manifest.resolve_shortcuts(arch) {
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
                let warnings = shortcut::create_shortcuts(&entries, &current_dir, false)?;
                for w in warnings {
                    let _ = tx.try_send(Event::CommitProgress(format!("⚠ {}", w)));
                }
            }
        }

        // 7. Apply env
        apply_env(pkg, session, arch, &current_dir)?;

        // 8. Apply persistence.
        //
        // Scoop persists after shims/shortcuts/env (lib/install.ps1:66), and still
        // writes into the *version* directory rather than the junction.
        apply_persistence(pkg, &version_dir, &persist_root)?;

        // 9. Run post_install
        if let Some(script_lines) = pkg.manifest.resolve_post_install(arch) {
            let ctx = script::HookContext::new(
                &version_dir,
                &pkg_persist_dir,
                &version_dir,
                pkg.version(),
            );
            script::run_powershell_script(&script_lines.iter().cloned().collect::<Vec<_>>(), &ctx)?;
        }

        // IMPORTANT — Scoop ABI contract on `url`:
        //
        // Scoop interprets install.json.url as the location of the
        // package MANIFEST, not the downloaded archive.
        //
        // If this field is populated for bucket installs, Scoop's
        // manifest() function (lib/manifest.ps1) ignores bucket and
        // attempts to download the URL as JSON — producing the
        // "Error parsing JSON at <binary-asset-url>" symptom.
        //
        // Rule: url is set ONLY for PackageSource::File (URL-sourced
        // installs).  For bucket installs and for None, url is null.
        let install_url = match &pkg.source {
            Some(PackageSource::File(_manifest_url)) => {
                // TODO: Scoop expects the manifest URL itself here, not
                // the download asset URL.  Use _manifest_url once the
                // File-sourced install path is fully implemented.
                pkg_files.first().map(|f| f.url.clone())
            }
            _ => None,
        };

        let _guard = session.write_lock()?;
        finalize_installation(pkg, &version_dir, arch, install_url.as_deref())?;
        drop(_guard);

        let state = InstallState {
            version: pkg.version().to_owned(),
            bucket: Some(pkg.bucket().to_owned()),
            arch: arch.to_string(),
            held: false,
            url: install_url,
        };

        installed.push(Package {
            ident: pkg.ident.clone(),
            manifest: pkg.manifest.clone(),
            source: pkg.source.clone(),
            status: PackageStatus::Installed(state),
        });

        let _ = tx.try_send(Event::CommitDone(pkg.ident.clone()));
    }

    Ok(installed)
}

fn apps_dir(session: &Session, pkg: &Package) -> PathBuf {
    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("apps"));

    root.join("apps").join(pkg.name())
}

fn apps_version_dir(session: &Session, pkg: &Package) -> PathBuf {
    apps_dir(session, pkg).join(version_component(pkg))
}

fn version_component(pkg: &Package) -> String {
    if pkg.is_nightly() {
        "nightly".to_owned()
    } else {
        pkg.version().to_owned()
    }
}

pub(crate) fn apply_persistence(
    pkg: &Package,
    version_dir: &std::path::Path,
    persist_root: &std::path::Path,
) -> Result<()> {
    if let Some(ref persist_val) = pkg.manifest.persist {
        let entries = persist::parse_persist(persist_val);
        let pkg_persist_dir = persist_root.join(pkg.name());
        if !entries.is_empty() {
            persist::apply(&entries, version_dir, &pkg_persist_dir)?;
        }
    }
    Ok(())
}

pub(crate) fn apply_shims(
    pkg: &Package,
    version_dir: &std::path::Path,
    shims_dir: &std::path::Path,
    tx: &flume::Sender<Event>,
    arch: Arch,
) -> Result<()> {
    if let Some(bin_val) = pkg.manifest.resolve_bin(arch) {
        let entries = shim::parse_bin(bin_val);
        for entry in &entries {
            let _ = tx.try_send(Event::CommitProgress(format!(
                "Creating shim for '{}'.",
                entry.name
            )));
        }
        if !entries.is_empty() {
            fs::ensure_dir(shims_dir)?;
            shim::create_shims(&entries, version_dir, shims_dir)?;
        }
    }
    Ok(())
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

pub(crate) fn apply_env(pkg: &Package, session: &Session, arch: Arch, dir: &Path) -> Result<()> {
    if let Some(env_set) = pkg.manifest.resolve_env_set(arch) {
        for (k, v) in env_set {
            session.env_service().set_env(k, v)?;
        }
    }
    for path in resolve_env_add_paths(&pkg.manifest, arch, dir) {
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
        .filter(|entry| !entry.trim().is_empty())
        // The name comes from the manifest and is joined onto `dir`, so it has to clear
        // the same guard as `bin` and `persist`: `..` or an absolute path in it would
        // otherwise place a PATH entry anywhere on the disk. `is_in_dir` below cannot
        // catch this — a joined path with unresolved `..` still starts with `dir` as a
        // string, so `..\..\..\..\Startup` passes the prefix test while resolving to
        // `C:\Startup`.
        .filter(|entry| crate::infra::fs::validate_relative_path("env_add_path", entry).is_ok())
        .map(|entry| dir.join(entry))
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

pub(crate) fn finalize_installation(
    pkg: &Package,
    version_dir: &Path,
    arch: Arch,
    url: Option<&str>,
) -> Result<()> {
    let install_info = &InstallRecord {
        version: pkg.version().to_owned(),
        bucket: Some(pkg.bucket().to_owned()),
        arch: arch.to_string(),
        held: false,
        url: url.map(str::to_owned),
    };

    // Written under both spellings so that old and new Scoop can each read
    // what Rake installed (see infra::install_meta).
    install_meta::write_install_record(version_dir, install_info)?;
    install_meta::write_installed_manifest(version_dir, &pkg.manifest)?;

    Ok(())
}

fn apply_extract_dir(
    pkg: &Package,
    version_dir: &std::path::Path,
    arch: Arch,
    tx: &flume::Sender<Event>,
) -> Result<()> {
    let manifest = &pkg.manifest;

    let dirs: Vec<String> = match manifest.resolve_extract_dir(arch) {
        Some(ed) => ed.clone().into_vec(),
        None => return Ok(()),
    };

    for subdir in &dirs {
        let src = version_dir.join(subdir);
        if !src.exists() {
            let _ = tx.try_send(Event::CommitProgress(format!(
                "⚠ extract_dir '{}' not found under {} — archive layout may not match manifest expectations",
                subdir,
                version_dir.display()
            )));
            continue;
        }

        let tmp = version_dir.join(".extract_tmp");
        fs::ensure_dir(&tmp)?;

        for entry in walkdir::WalkDir::new(&src) {
            let entry = entry.map_err(std::io::Error::other)?;
            let relative = entry.path().strip_prefix(&src).unwrap();
            let target = tmp.join(relative);

            if entry.file_type().is_dir() {
                fs::ensure_dir(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::ensure_dir(parent)?;
                }
                std::fs::copy(entry.path(), &target)?;
            }
        }

        fs::remove_dir(&src)?;

        for entry in walkdir::WalkDir::new(&tmp) {
            let entry = entry.map_err(std::io::Error::other)?;
            let relative = entry.path().strip_prefix(&tmp).unwrap();
            let target = version_dir.join(relative);

            if entry.file_type().is_dir() {
                fs::ensure_dir(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::ensure_dir(parent)?;
                }
                std::fs::copy(entry.path(), &target)?;
            }
        }

        fs::remove_dir(&tmp)?;

        // Clean up empty ancestor directories left behind by extraction
        let mut parent = src.parent();
        while let Some(p) = parent {
            if p == version_dir {
                break;
            }
            let _ = std::fs::remove_dir(p);
            parent = p.parent();
        }
    }

    Ok(())
}
