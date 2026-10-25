use std::path::Path;

use rake_domain::arch::Arch;
use rake_domain::manifest::Manifest;
use rake_domain::package::{Package, PackageStatus};

use crate::Result;
use crate::event::{BucketState, Event};
use crate::infra::git::GitService;
use crate::infra::{fs, install_meta, persist, script, shim, shortcut};
use crate::operations::bucket;
use crate::operations::download::DownloadedFile;
use crate::operations::install;
use crate::session::Session;

#[derive(Debug)]
pub struct UpdateSpec {
    pub installed: Package,
    pub candidate: Package,
    pub arch: Arch,
    pub downloaded: Vec<DownloadedFile>,
}

#[cfg(test)]
mod bucket_sync_tests {
    use super::*;
    use crate::bucket::Bucket;
    use std::path::Path;

    /// Build a bucket rooted at `<root>/buckets/<name>` without touching the network.
    fn bucket_at(root: &Path, name: &str, git: bool) -> Bucket {
        let path = root.join("buckets").join(name);
        std::fs::create_dir_all(&path).unwrap();
        if git {
            std::fs::create_dir_all(path.join(".git")).unwrap();
        }
        Bucket::from(&path).unwrap()
    }

    fn held_bucket(root: &Path, name: &str) -> Bucket {
        let b = bucket_at(root, name, true);
        // The marker is `.hold`; `hold` without the dot does nothing, which this helper
        // found out the hard way.
        std::fs::write(b.path().join(".hold"), b"").unwrap();
        Bucket::from(b.path()).unwrap()
    }

    /// A bucket with a repository is the only thing worth fetching.
    #[test]
    fn a_real_bucket_is_not_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let b = bucket_at(tmp.path(), "main", true);
        assert_eq!(skip_reason(&b), None);
    }

    /// A held bucket used to be skipped with no message at all, so `rake update` reported
    /// success over a tree it had deliberately left stale.
    #[test]
    fn a_held_bucket_is_reported_as_held() {
        let tmp = tempfile::tempdir().unwrap();
        let b = held_bucket(tmp.path(), "main");
        assert!(b.is_held(), "premise: the bucket is held");
        assert_eq!(skip_reason(&b), Some(BucketSyncOutcome::Held));
    }

    /// A directory in `buckets/` that is not a repository was also skipped silently.
    #[test]
    fn a_directory_without_git_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let b = bucket_at(tmp.path(), "fake", false);
        assert_eq!(skip_reason(&b), Some(BucketSyncOutcome::NotARepository));
    }

    /// A worktree or submodule has `.git` as a *file*. Skipping those would silently
    /// exclude a legitimate layout.
    #[test]
    fn a_git_file_counts_as_a_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let b = bucket_at(tmp.path(), "worktree", false);
        std::fs::write(b.path().join(".git"), "gitdir: ../elsewhere").unwrap();
        assert_eq!(
            skip_reason(&b),
            None,
            ".git as a file is how a worktree records its repository"
        );
    }

    fn report(entries: &[(&str, BucketSyncOutcome)]) -> BucketSyncReport {
        let mut r = BucketSyncReport::default();
        for (name, outcome) in entries {
            r.push(*name, outcome.clone());
        }
        r
    }

    #[test]
    fn counts_split_the_outcomes() {
        let r = report(&[
            ("a", BucketSyncOutcome::Updated),
            ("b", BucketSyncOutcome::Updated),
            ("c", BucketSyncOutcome::Held),
            ("d", BucketSyncOutcome::NotARepository),
            ("e", BucketSyncOutcome::Failed("no such host".into())),
        ]);
        assert_eq!(r.updated(), 2);
        assert_eq!(r.skipped(), 2);
        assert_eq!(r.failed(), 1);
        assert!(!r.all_succeeded());
    }

    /// The line the old code printed unconditionally.
    #[test]
    fn a_single_failure_makes_the_run_unsuccessful() {
        let r = report(&[
            ("a", BucketSyncOutcome::Updated),
            ("b", BucketSyncOutcome::Failed("x".into())),
        ]);
        assert!(!r.all_succeeded());
    }

    #[test]
    fn skips_alone_are_not_a_failure() {
        let r = report(&[
            ("a", BucketSyncOutcome::Held),
            ("b", BucketSyncOutcome::NotARepository),
        ]);
        assert!(r.all_succeeded(), "nothing went wrong, so nothing failed");
        assert_eq!(r.skipped(), 2);
    }

    /// No buckets at all is fine — there was nothing to be wrong about. This is the case
    /// the CLI renders as "Nothing was updated".
    #[test]
    fn an_empty_report_succeeds() {
        let r = BucketSyncReport::default();
        assert!(r.all_succeeded());
        assert_eq!(r.updated(), 0);
    }

    /// `is_held` reads a file whose absence means not held, so the classification must not
    /// depend on anything else being present.
    #[test]
    fn a_fresh_bucket_is_not_held() {
        let tmp = tempfile::tempdir().unwrap();
        let b = bucket_at(tmp.path(), "main", true);
        assert!(!b.is_held());
    }
}

/// What happened to one bucket during `rake update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BucketSyncOutcome {
    /// Fetched and fast-forwarded.
    Updated,
    /// Held on purpose, so deliberately left stale.
    Held,
    /// Present in the buckets directory but not a git repository.
    NotARepository,
    /// The fetch failed; the reason is libgit2's or git's.
    Failed(String),
}

impl BucketSyncOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, BucketSyncOutcome::Failed(_))
    }

    pub fn is_skip(&self) -> bool {
        matches!(
            self,
            BucketSyncOutcome::Held | BucketSyncOutcome::NotARepository
        )
    }
}

/// One line per bucket, in the order they were considered.
///
/// Returned rather than only pushed as progress events because those are rendered by
/// `indicatif`, which hides itself entirely when the output is not a terminal. Piping or
/// redirecting `rake update` used to print nothing at all about what happened — no bucket
/// name, no failure — and then "Everything is up to date!" regardless.
#[derive(Debug, Clone, Default)]
pub struct BucketSyncReport {
    pub outcomes: Vec<(String, BucketSyncOutcome)>,
}

impl BucketSyncReport {
    pub fn push(&mut self, name: impl Into<String>, outcome: BucketSyncOutcome) {
        self.outcomes.push((name.into(), outcome));
    }

    pub fn failed(&self) -> usize {
        self.count(BucketSyncOutcome::is_failure)
    }

    pub fn skipped(&self) -> usize {
        self.count(BucketSyncOutcome::is_skip)
    }

    pub fn updated(&self) -> usize {
        self.count(|o| *o == BucketSyncOutcome::Updated)
    }

    fn count(&self, predicate: impl Fn(&BucketSyncOutcome) -> bool) -> usize {
        self.outcomes.iter().filter(|(_, o)| predicate(o)).count()
    }

    /// True when nothing failed. A report with no buckets at all counts as fine — there
    /// was nothing to be wrong.
    pub fn all_succeeded(&self) -> bool {
        self.failed() == 0
    }
}

/// Why this bucket will not be fetched, decided before any I/O.
///
/// Pure apart from the `.git` check, which is a `stat`, so it can be tested against a
/// temporary directory with no network and no git binary. Held buckets and
/// non-repositories used to be skipped with no message at all — the command reported
/// success over a tree it had deliberately left stale.
fn skip_reason(bucket: &crate::bucket::Bucket) -> Option<BucketSyncOutcome> {
    if bucket.is_held() {
        return Some(BucketSyncOutcome::Held);
    }
    // `.git` is a directory normally and a *file* in a worktree or submodule, both of
    // which `exists` covers.
    if !bucket.path().join(".git").exists() {
        return Some(BucketSyncOutcome::NotARepository);
    }
    None
}

pub async fn bucket_update(session: &Session) -> Result<BucketSyncReport> {
    let buckets = bucket::bucket_list(session)?;
    let git = crate::infra::git_libgit2::Git::new();
    let tx = session.event_bus().core_sender();
    let mut report = BucketSyncReport::default();

    for bucket in &buckets {
        if let Some(reason) = skip_reason(bucket) {
            report.push(bucket.name(), reason);
            continue;
        }

        let _ = tx.try_send(Event::BucketSyncProgress {
            name: bucket.name().to_owned(),
            state: BucketState::Started,
        });

        let outcome = match git.pull(bucket.path()).await {
            Ok(_) => {
                let _ = tx.try_send(Event::BucketSyncProgress {
                    name: bucket.name().to_owned(),
                    state: BucketState::Succeeded,
                });
                BucketSyncOutcome::Updated
            }
            Err(e) => {
                let _ = tx.try_send(Event::BucketSyncProgress {
                    name: bucket.name().to_owned(),
                    state: BucketState::Failed(e.to_string()),
                });
                BucketSyncOutcome::Failed(e.to_string())
            }
        };

        report.push(bucket.name(), outcome);
    }

    let _ = tx.try_send(Event::BucketSyncDone);

    Ok(report)
}

pub async fn update_packages(session: &Session, specs: &[UpdateSpec]) -> Result<Vec<Package>> {
    let roots = install::InstallRoots::from_session(session);
    let tx = session.event_bus().core_sender();
    let mut updated = Vec::new();

    for spec in specs {
        let _ = tx.try_send(Event::UpdateStart(
            spec.candidate.ident.clone(),
            spec.installed.version().to_owned(),
            spec.candidate.version().to_owned(),
        ));

        // Everything is already downloaded (the caller drops specs whose
        // download failed), so nothing here can leave the old version touched
        // because of a network problem.
        match update_one(session, spec, &roots, &tx).await {
            Ok(pkg) => {
                let _ = tx.try_send(Event::UpdateDone(pkg.ident.clone()));
                updated.push(pkg);
            }
            Err(e) => {
                // The old version has been put back by `update_one`; one package
                // failing must not abandon the rest of the batch.
                let _ = tx.try_send(Event::UpdateProgress(format!(
                    "Failed to update '{}', keeping the old version: {e}",
                    spec.candidate.name()
                )));
            }
        }
    }

    Ok(updated)
}

/// Update one package: deactivate the old version, install the new one, then
/// carry the old version's `hold` flag over.
///
/// Deactivation happens **before** the install, not after. It is what makes the
/// install honest: the old shims, shortcuts and `current` are gone before the
/// new version claims them, so a failure cannot leave two versions fighting over
/// the same names. The old version directory itself is kept, and its `install.json`
/// with it.
async fn update_one(
    session: &Session,
    spec: &UpdateSpec,
    roots: &install::InstallRoots,
    tx: &flume::Sender<Event>,
) -> Result<Package> {
    let app_dir = roots.apps.join(spec.installed.name());
    let persist_dir = roots.persist.join(spec.installed.name());
    let old_version = install::version_component(&spec.installed);
    let new_version = install::version_component(&spec.candidate);
    let old_dir = app_dir.join(&old_version);
    let new_dir = app_dir.join(&new_version);

    // `hold` and the manifest come from what is actually on disk. The bucket
    // copy (`spec.installed.manifest`) may already describe the new version by
    // the time an update runs, and deactivation has to undo what was installed.
    let old_record = install_meta::read_install_record(&old_dir).ok().flatten();
    let held = old_record.as_ref().is_some_and(|r| r.held);
    let old_manifest = install_meta::read_installed_manifest(&old_dir)
        .ok()
        .flatten()
        .unwrap_or_else(|| spec.installed.manifest.clone());

    // A same-version reinstall (`--force`, or any nightly) installs into the
    // directory the old version occupies, so that directory is moved aside and
    // put back if the install fails.
    let backup = if old_dir == new_dir && old_dir.exists() {
        let backup = app_dir.join(format!("{old_version}.updating"));
        fs::remove_dir(&backup)?;
        std::fs::rename(&old_dir, &backup)?;
        Some(backup)
    } else {
        None
    };
    let active_old_dir = backup.as_deref().unwrap_or(&old_dir).to_path_buf();

    let _ = tx.try_send(Event::UpdateProgress(
        "Deactivating old version...".to_owned(),
    ));
    if let Err(e) = deactivate_old(
        session,
        &old_manifest,
        spec,
        &active_old_dir,
        &app_dir,
        &persist_dir,
        roots,
        tx,
    ) {
        // Deactivation stops at the first real failure, so the old version may
        // still be fully reachable — but it may also have lost its link or its
        // shims, so put everything back rather than guess.
        restore_old_version(
            session,
            &old_manifest,
            spec,
            backup.as_deref(),
            &old_dir,
            &app_dir,
            roots,
            tx,
        );
        return Err(e);
    }

    let _ = tx.try_send(Event::UpdateProgress(
        "Installing new version...".to_owned(),
    ));
    let installed_new = match install::install_packages(
        session,
        std::slice::from_ref(&spec.candidate),
        &spec.downloaded,
        spec.arch,
    )
    .await
    {
        Ok(mut packages) => packages.pop(),
        Err(e) => {
            // The install rolled itself back; now put the old version back the
            // way it was, including the link and the shims that deactivation
            // removed.
            restore_old_version(
                session,
                &old_manifest,
                spec,
                backup.as_deref(),
                &old_dir,
                &app_dir,
                roots,
                tx,
            );
            return Err(e);
        }
    };

    let Some(mut installed_new) = installed_new else {
        restore_old_version(
            session,
            &old_manifest,
            spec,
            backup.as_deref(),
            &old_dir,
            &app_dir,
            roots,
            tx,
        );
        return Err(crate::Error::Custom(format!(
            "installing '{}' produced no package",
            spec.candidate.name()
        )));
    };

    // Same-version install succeeded: the moved-aside copy is now stale.
    if let Some(backup) = backup {
        let _ = fs::remove_dir(&backup);
    }

    // `hold` survives an update, including a forced reinstall of the same
    // version. Written through the record the install just produced so `bucket`,
    // `architecture` and `url` stay exactly as installed.
    if held {
        if let Ok(Some(mut record)) = install_meta::read_install_record(&new_dir) {
            record.held = true;
            install_meta::write_install_record(&new_dir, &record)?;
        }
        if let PackageStatus::Installed(state) = &mut installed_new.status {
            state.held = true;
        }
    }

    Ok(installed_new)
}

/// Take the old version out of service, leaving its directory in place.
///
/// Mirrors `uninstall_app` (assets/scoop/libexec/scoop-uninstall.ps1), except
/// that the version directory and its metadata are kept: the point of an update
/// is that the previous version is still there if the new one turns out worse.
#[allow(clippy::too_many_arguments)]
fn deactivate_old(
    session: &Session,
    manifest: &Manifest,
    spec: &UpdateSpec,
    version_dir: &Path,
    app_dir: &Path,
    persist_dir: &Path,
    roots: &install::InstallRoots,
    tx: &flume::Sender<Event>,
) -> Result<()> {
    let arch = spec.arch;

    // 1. pre_uninstall, best effort: Scoop does not fail an uninstall because a
    //    notification hook could not run.
    if let Some(lines) = manifest.resolve_pre_uninstall(arch) {
        let ctx = script::HookContext::new(
            version_dir,
            persist_dir,
            version_dir,
            spec.installed.version(),
        );
        let body: Vec<String> = lines.iter().cloned().collect();
        let _ = script::run_powershell_script(&body, &ctx);
    }

    // 2. The package's own uninstaller. A failure here aborts the update: the
    //    old version may have half-removed itself, and installing on top of that
    //    is worse than not updating.
    install::run_installer(
        manifest,
        arch,
        version_dir,
        persist_dir,
        spec.installed.version(),
        roots,
        true,
    )?;

    // 3. Unlink persist before anything else touches the directory. The links
    //    live inside it, and following one would delete the user's data.
    if let Some(ref persist_val) = manifest.persist {
        let entries = persist::parse_persist(persist_val);
        let _ = persist::unlink(&entries, version_dir);
    }

    // 4. Shims, shortcuts and the module link the old version owns.
    if let Some(bin_val) = manifest.resolve_bin(arch) {
        let entries = shim::parse_bin(bin_val);
        let _ = shim::remove_shims(&entries, &roots.shims);
    }
    if let Some(list) = manifest.resolve_shortcuts(arch) {
        let entries = install::parse_shortcuts(list);
        if !entries.is_empty() {
            let _ = shortcut::remove_shortcuts(&entries, false);
        }
    }
    let _ = install::uninstall_psmodule(manifest, roots);

    // 5. Environment entries the old version added. The new install re-adds its
    //    own; leaving the old ones behind is how a PATH entry for a version that
    //    no longer exists survives forever.
    if let Some(env_set) = manifest.resolve_env_set(arch) {
        for key in env_set.keys() {
            let _ = session.env_service().remove_env(key);
        }
    }
    install::remove_env_add_paths(manifest, arch, &app_dir.join("current"));

    // 6. `current` itself. The new install creates it again.
    if !session.config().no_junction.unwrap_or(false) {
        let _ = tx.try_send(Event::UpdateProgress("Unlinking current...".to_owned()));
        fs::remove_symlink(&app_dir.join("current"))?;
    }

    // 7. post_uninstall, best effort. It still sees the version directory.
    if let Some(lines) = manifest.resolve_post_uninstall(arch) {
        let ctx = script::HookContext::new(
            version_dir,
            persist_dir,
            version_dir,
            spec.installed.version(),
        );
        let body: Vec<String> = lines.iter().cloned().collect();
        let _ = script::run_powershell_script(&body, &ctx);
    }

    Ok(())
}

/// Put the old version back into service after a failed install.
///
/// Deactivation removed everything that made the old version reachable, so
/// undoing it means recreating exactly those things — and nothing that would
/// touch the new version directory, which the failed install already removed.
#[allow(clippy::too_many_arguments)]
fn reactivate_old(
    session: &Session,
    manifest: &Manifest,
    spec: &UpdateSpec,
    version_dir: &Path,
    app_dir: &Path,
    roots: &install::InstallRoots,
    tx: &flume::Sender<Event>,
) -> Result<()> {
    let arch = spec.arch;
    let no_junction = session.config().no_junction.unwrap_or(false);

    if !no_junction {
        install::link_current(version_dir, app_dir)?;
    }
    let dir = if no_junction {
        version_dir.to_path_buf()
    } else {
        app_dir.join("current")
    };

    let _ = install::apply_shims(manifest, &dir, &roots.shims, tx, arch);
    let _ = install::apply_shortcuts(manifest, &dir, arch, tx);
    let _ = install::install_psmodule(session, manifest, &dir, roots);
    let _ = install::apply_env(manifest, session, arch, &dir);
    let _ =
        install::apply_persistence(manifest, spec.installed.name(), version_dir, &roots.persist);

    Ok(())
}

/// Move a same-version backup back into place, if there was one.
fn restore_backup(backup: Option<&Path>, version_dir: &Path) {
    if let Some(backup) = backup {
        // The failed install removed the version directory it created.
        let _ = fs::remove_dir(version_dir);
        let _ = std::fs::rename(backup, version_dir);
    }
}

/// Undo a partial update: put the old version directory back where it belongs
/// and make it reachable again.
///
/// Best effort throughout — the caller already has an error to report, and a
/// recovery step that fails must not replace it with a less useful one.
#[allow(clippy::too_many_arguments)]
fn restore_old_version(
    session: &Session,
    manifest: &Manifest,
    spec: &UpdateSpec,
    backup: Option<&Path>,
    old_dir: &Path,
    app_dir: &Path,
    roots: &install::InstallRoots,
    tx: &flume::Sender<Event>,
) {
    // The backup, when there is one, is renamed back to `old_dir` here — so
    // reactivation must be handed `old_dir`, not the `.updating` path.
    restore_backup(backup, old_dir);
    if let Err(e) = reactivate_old(session, manifest, spec, old_dir, app_dir, roots, tx) {
        let _ = tx.try_send(Event::UpdateProgress(format!(
            "Could not reactivate the old version: {e}"
        )));
    }
}
