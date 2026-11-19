use std::path::{Path, PathBuf};

use rake_domain::arch::Arch;
use rake_domain::package::{InstallRecord, InstallState, Package, PackageSource, PackageStatus};

use crate::Result;
use crate::event::Event;
use crate::infra::archive::{
    ArchiveFormat, NativeArchive, detect_format, extract_innosetup, normalize_manifest_dir,
};
use crate::infra::fs;
use crate::infra::install_meta;
use crate::infra::script;
use crate::infra::shortcut::ShortcutEntry;
use crate::infra::{persist, shim, shortcut};
use crate::operations::download::DownloadedFile;
use crate::session::Session;

/// Re-exported so callers (`update`) share the single canonical type
/// rather than redefining a private `InstallInfo`.
pub type InstallInfo = InstallRecord;

pub use crate::infra::layout::{AppLayout, InstallRoots};

pub(crate) use super::activation::{
    apply_env, apply_persistence, apply_shims, apply_shortcuts, deactivate_links, install_psmodule,
    link_current, parse_shortcuts, reactivate_links, remove_env_add_paths, uninstall_psmodule,
};
pub(crate) use super::installer::{run_hook, run_installer};

/// Commit downloaded files — extract, link, shim, persist.
/// Does NOT download — caller is responsible for providing DownloadedFile list.
///
/// The steps run in the order `install_app` uses
/// (assets/scoop/lib/install.ps1:48-73): pre_install → installer → `current` →
/// shims → shortcuts → psmodule → env → persist → post_install → metadata.
/// `install.json` is written **last**, so a version directory that carries one
/// is a version directory that finished.
pub async fn install_packages(
    session: &Session,
    packages: &[Package],
    files: &[DownloadedFile],
    arch: Arch,
) -> Result<Vec<Package>> {
    let roots = InstallRoots::from_session(session);
    let tx = session.event_bus().core_sender();
    let mut installed = Vec::new();

    for pkg in packages {
        let _ = tx.try_send(Event::CommitStart(
            pkg.ident.clone(),
            pkg.version().to_string(),
        ));

        let pkg_files: Vec<&DownloadedFile> =
            files.iter().filter(|f| f.ident == pkg.ident).collect();

        let committed = install_one(session, pkg, &pkg_files, arch, &roots, &tx).await?;
        installed.push(committed);

        let _ = tx.try_send(Event::CommitDone(pkg.ident.clone()));
    }

    Ok(installed)
}

/// One package, in a transaction that is rolled back on any error.
async fn install_one(
    session: &Session,
    pkg: &Package,
    files: &[&DownloadedFile],
    arch: Arch,
    roots: &InstallRoots,
    tx: &flume::Sender<Event>,
) -> Result<Package> {
    let layout = roots.app_layout(pkg.name());
    let version_dir = layout.pkg_version_dir(pkg);

    if version_dir.exists() {
        if install_meta::is_installed(&version_dir) {
            return Err(crate::Error::Domain(
                rake_domain::Error::PackageAlreadyExists(pkg.name().to_owned()),
            ));
        }
        // A version directory without metadata is what an interrupted install
        // leaves behind. Scoop purges it (`ensure_none_failed`) rather than
        // installing on top of it.
        fs::remove_dir(&version_dir)?;
    }
    fs::ensure_dir(&version_dir)?;

    // IMPORTANT — Scoop ABI contract on `url`:
    //
    // Scoop interprets install.json.url as the location of the package MANIFEST,
    // not the downloaded archive. If this field is populated for bucket installs,
    // Scoop's manifest() function (lib/manifest.ps1) ignores bucket and attempts
    // to download the URL as JSON — producing the "Error parsing JSON at
    // <binary-asset-url>" symptom.
    //
    // Rule: url is set ONLY for PackageSource::File (URL-sourced installs). For
    // bucket installs and for None, url is null.
    let install_url = match &pkg.source {
        Some(PackageSource::File(_manifest_url)) => files.first().map(|f| f.url.clone()),
        _ => None,
    };

    let mut txn = InstallTransaction::new(
        &version_dir,
        &layout.app_dir,
        &roots.shims,
        &layout.persist_dir,
    );

    match install_steps(
        session,
        pkg,
        files,
        arch,
        roots,
        &mut txn,
        tx,
        install_url.as_deref(),
    )
    .await
    {
        Ok(()) => Ok(Package {
            ident: pkg.ident.clone(),
            manifest: pkg.manifest.clone(),
            source: pkg.source.clone(),
            status: PackageStatus::Installed(InstallState {
                version: pkg.version().to_owned(),
                bucket: Some(pkg.bucket().to_owned()),
                arch: arch.to_string(),
                held: false,
                url: install_url,
            }),
            // Just committed: the metadata is written by `finalize_installation`.
            install_failed: false,
        }),
        Err(e) => {
            txn.rollback();
            Err(e)
        }
    }
}

/// Everything the transaction created, so a failure can undo exactly that.
///
/// The version directory is created before this is constructed and removed by
/// [`InstallTransaction::rollback`]; nothing else is touched until it is
/// recorded here.
struct InstallTransaction {
    version_dir: PathBuf,
    app_dir: PathBuf,
    shims_dir: PathBuf,
    persist_dir: PathBuf,
    persist_entries: Vec<persist::PersistEntry>,
    created_shims: Vec<String>,
    created_shortcuts: Vec<ShortcutEntry>,
    created_module: Option<PathBuf>,
    linked_current: bool,
}

impl InstallTransaction {
    fn new(version_dir: &Path, app_dir: &Path, shims_dir: &Path, persist_dir: &Path) -> Self {
        Self {
            version_dir: version_dir.to_path_buf(),
            app_dir: app_dir.to_path_buf(),
            shims_dir: shims_dir.to_path_buf(),
            persist_dir: persist_dir.to_path_buf(),
            persist_entries: Vec::new(),
            created_shims: Vec::new(),
            created_shortcuts: Vec::new(),
            created_module: None,
            linked_current: false,
        }
    }

    /// Point `apps/<app>/current` at the version directory, or return the
    /// version directory itself when `no_junction` is set.
    ///
    /// The lock covers the junction swap alone: shim creation is several file
    /// writes and post_install can shell out to PowerShell for seconds, neither
    /// of which should be serialised.
    fn activate_current(&mut self, session: &Session) -> Result<PathBuf> {
        if session.config().no_junction.unwrap_or(false) {
            return Ok(self.version_dir.clone());
        }

        {
            let _guard = session.write_lock()?;
            link_current(&self.version_dir, &self.app_dir)?;
        }
        self.linked_current = true;
        Ok(self.app_dir.join("current"))
    }

    /// Undo everything this transaction created. Best effort: every step runs
    /// even if an earlier one failed, because a rollback that stops halfway
    /// leaves more behind than one that carries on.
    ///
    /// Persist links go first — they live inside the version directory, and
    /// removing the directory through them would delete the user's data.
    fn rollback(&mut self) {
        if !self.persist_entries.is_empty() {
            let _ = persist::unlink(&self.persist_entries, &self.version_dir);
        }
        for name in &self.created_shims {
            let _ = shim::remove_shim(name, &self.shims_dir);
        }
        if !self.created_shortcuts.is_empty() {
            let _ = shortcut::remove_shortcuts(&self.created_shortcuts, false);
        }
        if let Some(module) = &self.created_module {
            let _ = fs::remove_symlink(module);
        }
        if self.linked_current {
            // `link_current` removed any previous link before creating this one,
            // so whatever is there now points at *our* version directory.
            let _ = fs::remove_symlink(&self.app_dir.join("current"));
        }
        let _ = fs::remove_dir(&self.version_dir);
    }
}

#[allow(clippy::too_many_arguments)]
async fn install_steps(
    session: &Session,
    pkg: &Package,
    files: &[&DownloadedFile],
    arch: Arch,
    roots: &InstallRoots,
    txn: &mut InstallTransaction,
    tx: &flume::Sender<Event>,
    install_url: Option<&str>,
) -> Result<()> {
    let version_dir = txn.version_dir.clone();
    let persist_dir = txn.persist_dir.clone();
    let manifest = &pkg.manifest;
    let fnames: Vec<String> = files.iter().map(|f| f.target_filename.clone()).collect();
    let arch_str = arch.as_str();

    stage_and_extract(session, pkg, files, arch, &version_dir, tx).await?;

    // pre_install runs while the files are in the version directory but before
    // `current`, shims and persist exist (install.ps1:54).
    if let Some(lines) = manifest.resolve_pre_install(arch) {
        run_hook(
            script::HookPhase::PreInstall,
            lines,
            &version_dir,
            &version_dir,
            &persist_dir,
            pkg.version(),
            pkg.name(),
            arch_str,
            &fnames,
            roots,
        )?;
    }

    run_installer(
        manifest,
        arch,
        &version_dir,
        &persist_dir,
        pkg.version(),
        pkg.name(),
        &fnames,
        roots,
        false,
    )?;

    // `current` before shims/shortcuts/env, so all three reference the junction
    // rather than the version directory (install.ps1:58).
    let dir = txn.activate_current(session)?;

    txn.created_shims = apply_shims(manifest, &dir, &roots.shims, tx, arch)?;

    txn.created_shortcuts = apply_shortcuts(manifest, &dir, arch, tx)?;

    txn.created_module = install_psmodule(session, manifest, &dir, roots)?;

    apply_env(manifest, session, arch, &dir)?;

    // Scoop persists *after* env and still writes into the version directory
    // rather than the junction (install.ps1:66); both names resolve to the same
    // directory while the junction is in place.
    txn.persist_entries = apply_persistence(manifest, pkg.name(), &version_dir, &roots.persist)?;

    if let Some(lines) = manifest.resolve_post_install(arch) {
        run_hook(
            script::HookPhase::PostInstall,
            lines,
            &version_dir,
            &dir,
            &persist_dir,
            pkg.version(),
            pkg.name(),
            arch_str,
            &fnames,
            roots,
        )?;
    }

    // The metadata is the commit record: written last, under both spellings so
    // old and new Scoop can each read what Rake installed.
    {
        let _guard = session.write_lock()?;
        finalize_installation(pkg, &version_dir, arch, install_url)?;
    }

    Ok(())
}

/// Stage every artifact in the version directory, then extract the containers.
///
/// Mirrors Scoop's `Invoke-ScoopDownload` (lib/download.ps1:23): each cached
/// file is copied to `<version_dir>\<url_filename>` before anything is
/// extracted. The copy is not redundant — an `.msi` that references external
/// cabinets (`msival2`, `orca`, `visualc`) needs the `.cab` files sitting next
/// to it, and the cache does not keep them together.
async fn stage_and_extract(
    session: &Session,
    pkg: &Package,
    files: &[&DownloadedFile],
    arch: Arch,
    version_dir: &Path,
    tx: &flume::Sender<Event>,
) -> Result<()> {
    for file in files {
        let dest = version_dir.join(&file.target_filename);
        if let Some(parent) = dest.parent() {
            fs::ensure_dir(parent)?;
        }
        std::fs::copy(&file.cache_path, &dest)?;
    }

    let extract_dirs: Vec<String> = pkg
        .manifest
        .resolve_extract_dir(arch)
        .map(|dirs| dirs.as_slice().to_vec())
        .unwrap_or_default();
    let extract_tos: Vec<String> = pkg
        .manifest
        .resolve_extract_to(arch)
        .map(|dirs| dirs.as_slice().to_vec())
        .unwrap_or_default();
    let innosetup = pkg.manifest.innosetup == Some(true);
    let archive = NativeArchive::new(session.config().root_path.clone());

    // `extract_dir` / `extract_to` pair with the *extracted* archives, not with
    // every URL: `Invoke-Extraction` advances a separate `$extracted` counter
    // only when a file is actually unpacked (lib/decompress.ps1:20,58). A
    // non-archive in the middle of `url` therefore does not consume an entry.
    let mut extracted = 0usize;
    for file in files {
        let staged = version_dir.join(&file.target_filename);
        let Some(plan) = extraction_plan(&file.target_filename, innosetup) else {
            continue;
        };

        let extract_dir = extract_dirs.get(extracted).map(String::as_str);
        let target_dir = match normalize_manifest_dir(
            "extract_to",
            extract_tos.get(extracted).map(String::as_str),
        )? {
            Some(sub) => version_dir.join(sub),
            None => version_dir.to_path_buf(),
        };
        fs::ensure_dir(&target_dir)?;

        let _ = tx.try_send(Event::CommitProgress(format!(
            "Extracting {} ...",
            file.target_filename,
        )));

        match plan {
            Extraction::Inno => extract_innosetup(
                &staged,
                &target_dir,
                session.config().root_path.as_deref(),
                extract_dir,
            )?,
            Extraction::Archive(format) => {
                archive
                    .extract_with(&staged, &target_dir, format, extract_dir)
                    .await?
            }
        }

        // Scoop passes `-Removal` to every extractor: once unpacked, the staged
        // archive must not end up in the app directory.
        std::fs::remove_file(&staged)?;
        extracted += 1;
    }

    Ok(())
}

/// How a staged download has to be handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Extraction {
    /// InnoSetup installer, unpacked with `innounp`.
    Inno,
    /// A real archive, unpacked by [`NativeArchive`].
    Archive(ArchiveFormat),
}

/// Decide whether a staged file is a container, from its name on disk.
///
/// The name is `DownloadedFile::target_filename`, which already honours Scoop's
/// `#/dl.7z` fragment, because that is what `Invoke-Extraction` switches on
/// (lib/decompress.ps1:25). A bare `.exe` is only an installer when the
/// manifest says `innosetup`; otherwise it is payload and stays where it was
/// copied.
fn extraction_plan(filename: &str, innosetup: bool) -> Option<Extraction> {
    if let Some(format) = detect_format(filename) {
        return Some(Extraction::Archive(format));
    }

    if innosetup && filename.to_lowercase().ends_with(".exe") {
        return Some(Extraction::Inno);
    }

    None
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
