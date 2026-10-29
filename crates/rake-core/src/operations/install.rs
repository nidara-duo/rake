use std::path::{Path, PathBuf};
use std::process::Command;

use rake_domain::arch::Arch;
use rake_domain::manifest::Manifest;
use rake_domain::package::{InstallRecord, InstallState, Package, PackageSource, PackageStatus};

use crate::Result;
use crate::event::Event;
use crate::infra::archive::{
    ArchiveFormat, NativeArchive, detect_format, extract_innosetup, normalize_manifest_dir,
};
use crate::infra::fs;
use crate::infra::install_meta;
use crate::infra::shortcut::ShortcutEntry;
use crate::infra::{env, persist, script, shim, shortcut};
use crate::operations::download::{self, DownloadedFile};
use crate::session::Session;

/// Re-exported so callers (`update`) share the single canonical type
/// rather than redefining a private `InstallInfo`.
pub type InstallInfo = InstallRecord;

/// The directories one install batch writes to, resolved once.
///
/// Passed around as one value because every step below needs a different subset
/// of them, and a seven-argument parameter list on each is where a mix-up lives.
pub(crate) struct InstallRoots {
    /// `$SCOOP` — also the base the PowerShell library is loaded from.
    pub root: PathBuf,
    pub apps: PathBuf,
    pub shims: PathBuf,
    pub persist: PathBuf,
    pub modules: PathBuf,
    pub global: PathBuf,
}

impl InstallRoots {
    pub(crate) fn from_session(session: &Session) -> Self {
        let root = session
            .config()
            .root_path
            .as_ref()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("apps"));
        let global = session
            .config()
            .global_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\rake"));

        Self {
            apps: root.join("apps"),
            shims: root.join("shims"),
            persist: root.join("persist"),
            modules: root.join("modules"),
            root,
            global,
        }
    }
}

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
    let app_dir = roots.apps.join(pkg.name());
    let version_dir = app_dir.join(version_component(pkg));
    let persist_dir = roots.persist.join(pkg.name());

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

    let mut txn = InstallTransaction::new(&version_dir, &app_dir, &roots.shims, &persist_dir);

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
            lines,
            &dir,
            &version_dir,
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

/// The version directory name for a package — `nightly` for nightlies, which
/// reuse one directory instead of accumulating versions.
pub(crate) fn version_component(pkg: &Package) -> String {
    if pkg.is_nightly() {
        "nightly".to_owned()
    } else {
        pkg.version().to_owned()
    }
}

/// Run a manifest hook with scoop's PowerShell library and context variables.
#[allow(clippy::too_many_arguments)]
fn run_hook(
    lines: &rake_domain::one_or_many::OneOrMany<String>,
    ctx_dir: &Path,
    original_dir: &Path,
    persist_dir: &Path,
    version: &str,
    app: &str,
    arch: &str,
    fnames: &[String],
    roots: &InstallRoots,
) -> Result<()> {
    let ctx = script::HookContext::with_scoop_lib(
        ctx_dir,
        persist_dir,
        original_dir,
        version,
        &roots.root,
        &roots.global,
    )
    .with_app(app)
    .with_arch(arch)
    .with_fnames(fnames);
    let body: Vec<String> = lines.iter().cloned().collect();
    script::run_powershell_script(&body, &ctx)
}

/// Run `installer` / `uninstaller`, exactly as `Invoke-Installer` does
/// (assets/scoop/lib/install.ps1:84).
///
/// Order matters: the executable runs first and the hook script second. A
/// `.ps1` executable is invoked inside PowerShell and is never deleted, because
/// Scoop's `Remove-Item` sits in the `else` branch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_installer(
    manifest: &Manifest,
    arch: Arch,
    dir: &Path,
    persist_dir: &Path,
    version: &str,
    app: &str,
    fnames: &[String],
    roots: &InstallRoots,
    uninstall: bool,
) -> Result<()> {
    let (title, file, args, script_lines, keep) = if uninstall {
        let Some(spec) = manifest.resolve_uninstaller(arch) else {
            return Ok(());
        };
        (
            "Uninstaller",
            spec.file.as_deref(),
            spec.args.as_ref(),
            spec.script.as_ref(),
            false,
        )
    } else {
        let Some(spec) = manifest.resolve_installer(arch) else {
            return Ok(());
        };
        (
            "Installer",
            spec.file.as_deref(),
            spec.args.as_ref(),
            spec.script.as_ref(),
            spec.keep.unwrap_or(false),
        )
    };

    if file.is_some() || args.is_some() {
        // The filename is either explicit (`installer.file`) or the name the
        // first URL downloads to (`url_filename @(url $manifest $architecture)`).
        let name = file.map(str::to_owned).or_else(|| {
            manifest
                .resolve_urls(arch)
                .first()
                .map(|url| download::target_filename(url))
        });

        if let Some(name) = name {
            let prog = dir.join(&name);
            if !is_in_dir(dir, &prog) {
                return Err(crate::Error::Custom(format!(
                    "Error in manifest: {title} {} is outside the app directory.",
                    prog.display()
                )));
            }
            if !prog.is_file() {
                return Err(crate::Error::Custom(format!(
                    "{title} {} is missing.",
                    prog.display()
                )));
            }

            let fn_args = substitute_args(args, dir, version);
            if name.to_lowercase().ends_with(".ps1") {
                // `& $progName @fnArgs` runs in-process, where scoop's library is
                // already loaded — so the installer can call its functions.
                run_ps1_installer(
                    &prog,
                    &fn_args,
                    dir,
                    persist_dir,
                    version,
                    app,
                    arch.as_str(),
                    fnames,
                    roots,
                )?;
            } else {
                let status = Command::new(&prog).args(&fn_args).status()?;
                if !status.success() {
                    return Err(crate::Error::Custom(if uninstall {
                        "Uninstallation aborted.".to_owned()
                    } else {
                        "Installation aborted. You might need to run 'rake uninstall' \
                         before trying again."
                            .to_owned()
                    }));
                }
                // Scoop keeps the installer only when `installer.keep` is true;
                // the uninstaller spec has no such flag, so it is always removed.
                if !keep {
                    std::fs::remove_file(&prog)?;
                }
            }
        }
    }

    if let Some(lines) = script_lines {
        let ctx = script::HookContext::with_scoop_lib(
            dir,
            persist_dir,
            dir,
            version,
            &roots.root,
            &roots.global,
        )
        .with_app(app)
        .with_arch(arch.as_str())
        .with_fnames(fnames);
        let body: Vec<String> = lines.iter().cloned().collect();
        script::run_powershell_script(&body, &ctx)?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_ps1_installer(
    prog: &Path,
    args: &[String],
    dir: &Path,
    persist_dir: &Path,
    version: &str,
    app: &str,
    arch: &str,
    fnames: &[String],
    roots: &InstallRoots,
) -> Result<()> {
    let mut line = format!(
        "& {}",
        script::quote_powershell_string(&prog.to_string_lossy())
    );
    for arg in args {
        line.push(' ');
        line.push_str(&script::quote_powershell_string(arg));
    }

    let ctx = script::HookContext::with_scoop_lib(
        dir,
        persist_dir,
        dir,
        version,
        &roots.root,
        &roots.global,
    )
    .with_app(app)
    .with_arch(arch)
    .with_fnames(fnames);
    script::run_powershell_script(&[line], &ctx)
}

/// `substitute` from lib/core.ps1:1286 for the three keys `Invoke-Installer`
/// supplies. `$global` is always `False`: rake has no global installs, and
/// PowerShell renders the boolean that way.
fn substitute_args(
    args: Option<&rake_domain::one_or_many::OneOrMany<String>>,
    dir: &Path,
    version: &str,
) -> Vec<String> {
    let Some(args) = args else {
        return Vec::new();
    };
    let dir = dir.to_string_lossy();

    args.as_slice()
        .iter()
        .map(|arg| {
            arg.replace("$dir", &dir)
                .replace("$global", "False")
                .replace("$version", version)
        })
        .collect()
}

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
