use std::path::PathBuf;

use rake_domain::arch::Arch;
use rake_domain::package::{InstallRecord, InstallState, Package, PackageStatus};

use crate::Result;
use crate::event::Event;
use crate::infra::archive::{
    ArchiveService, detect_format_for_url, extract_innosetup, extract_innosetup_with_script,
};
use crate::infra::fs;
use crate::infra::shortcut::ShortcutEntry;
use crate::infra::{persist, script, shim, shortcut};
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

        if version_dir.exists() {
            let install_json = version_dir.join("install.json");
            let manifest_json = version_dir.join("manifest.json");
            if install_json.exists() && manifest_json.exists() {
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

            // Treat manifests with `installer.script` as needing
            // InnoSetup extraction (Scoop uses `installer.script` as an
            // alternative to the `innosetup` boolean flag — zed is a
            // real example).
            let is_innosetup = pkg.manifest.innosetup == Some(true)
                || pkg
                    .manifest
                    .installer
                    .as_ref()
                    .and_then(|i| i.script.as_ref())
                    .is_some();

            if is_innosetup && extraction_format.is_none() {
                let _ = tx.try_send(Event::CommitProgress(format!(
                    "Extracting {} ...",
                    file.url.split('/').next_back().unwrap_or("file"),
                )));
                if let Some(script_lines) = pkg
                    .manifest
                    .installer
                    .as_ref()
                    .and_then(|i| i.script.as_ref())
                {
                    let lines: Vec<String> = script_lines.iter().cloned().collect();
                    extract_innosetup_with_script(
                        &lines,
                        &file.cache_path,
                        &target_dir,
                        session.config().root_path.as_deref(),
                    )?;
                } else {
                    let ed = pkg.manifest.resolve_extract_dir(arch);
                    let ed_str = ed.and_then(|ed| ed.as_slice().first().map(|s| s.as_str()));
                    extract_innosetup(
                        &file.cache_path,
                        &target_dir,
                        session.config().root_path.as_deref(),
                        ed_str,
                    )?;
                }
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
        apply_extract_dir(pkg, &version_dir, arch)?;

        // 3. Run pre_install
        if let Some(script_lines) = pkg.manifest.resolve_pre_install(arch) {
            let ctx = script::HookContext {
                version_dir: &version_dir,
                persist_dir: &persist_root.join(pkg.name()),
                original_dir: &version_dir,
                version: pkg.version(),
            };
            script::run_powershell_script(&script_lines.iter().cloned().collect::<Vec<_>>(), &ctx)?;
        }

        // 4. Apply persistence
        apply_persistence(pkg, &version_dir, &persist_root)?;

        // 5. Run post_install
        if let Some(script_lines) = pkg.manifest.resolve_post_install(arch) {
            let ctx = script::HookContext {
                version_dir: &version_dir,
                persist_dir: &persist_root.join(pkg.name()),
                original_dir: &version_dir,
                version: pkg.version(),
            };
            script::run_powershell_script(&script_lines.iter().cloned().collect::<Vec<_>>(), &ctx)?;
        }

        // 6. Create shims
        apply_shims(pkg, &version_dir, &shims_dir, &tx, arch)?;

        // 7. Create Start Menu shortcuts
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
                shortcut::create_shortcuts(&entries, &version_dir, false)?;
            }
        }

        // 8. Apply env
        apply_env(pkg, session, arch)?;

        let install_url = pkg_files.first().map(|f| f.url.clone());

        let _guard = session.write_lock()?;
        finalize_installation(
            session,
            pkg,
            &version_dir,
            &app_dir,
            arch,
            install_url.as_deref(),
        )?;
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

pub(crate) fn apply_env(pkg: &Package, session: &Session, arch: Arch) -> Result<()> {
    if let Some(env_set) = pkg.manifest.resolve_env_set(arch) {
        for (k, v) in env_set {
            session.env_service().set_env(k, v)?;
        }
    }
    if let Some(env_add_path) = pkg.manifest.resolve_env_add_path(arch) {
        for path in env_add_path.iter() {
            session.env_service().add_path(path)?;
        }
    }
    Ok(())
}

pub(crate) fn finalize_installation(
    _session: &Session,
    pkg: &Package,
    version_dir: &std::path::Path,
    app_dir: &std::path::Path,
    arch: Arch,
    url: Option<&str>,
) -> Result<()> {
    let install_info = InstallRecord {
        version: pkg.version().to_owned(),
        bucket: Some(pkg.bucket().to_owned()),
        arch: arch.to_string(),
        held: false,
        url: url.map(str::to_owned),
    };

    let install_json = version_dir.join("install.json");
    std::fs::write(&install_json, serde_json::to_string_pretty(&install_info)?)?;

    let manifest_json = version_dir.join("manifest.json");
    std::fs::write(&manifest_json, serde_json::to_string_pretty(&pkg.manifest)?)?;

    let current_link = app_dir.join("current");
    fs::remove_symlink(&current_link)?;

    #[cfg(windows)]
    fs::create_junction(version_dir, &current_link)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(version_dir, &current_link)?;

    Ok(())
}

fn apply_extract_dir(pkg: &Package, version_dir: &std::path::Path, arch: Arch) -> Result<()> {
    let manifest = &pkg.manifest;

    let dirs: Vec<String> = match manifest.resolve_extract_dir(arch) {
        Some(ed) => ed.clone().into_vec(),
        None => return Ok(()),
    };

    for subdir in &dirs {
        let src = version_dir.join(subdir);
        if !src.exists() {
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
