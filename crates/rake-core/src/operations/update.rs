use std::collections::HashSet;

use std::path::PathBuf;

use rake_domain::arch::Arch;
use rake_domain::package::Package;

use crate::Result;
use crate::event::{BucketState, Event};
use crate::infra::git::GitService;
use crate::infra::{fs, persist, script, shim, shortcut};
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

pub async fn bucket_update(session: &Session) -> Result<()> {
    let buckets = bucket::bucket_list(session)?;
    let git = crate::infra::git_libgit2::Git::new();
    let tx = session.event_bus().core_sender();

    for bucket in buckets {
        if bucket.is_held() {
            continue;
        }

        if !bucket.path().join(".git").exists() {
            continue;
        }

        let _ = tx.try_send(Event::BucketSyncProgress {
            name: bucket.name().to_owned(),
            state: BucketState::Started,
        });

        match git.pull(bucket.path()).await {
            Ok(_) => {
                let _ = tx.try_send(Event::BucketSyncProgress {
                    name: bucket.name().to_owned(),
                    state: BucketState::Succeeded,
                });
            }
            Err(e) => {
                let _ = tx.try_send(Event::BucketSyncProgress {
                    name: bucket.name().to_owned(),
                    state: BucketState::Failed(e.to_string()),
                });
            }
        }
    }

    let _ = tx.try_send(Event::BucketSyncDone);

    Ok(())
}

pub async fn update_packages(session: &Session, specs: &[UpdateSpec]) -> Result<Vec<Package>> {
    let mut updated = Vec::new();
    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("apps"));
    let shims_dir = root.join("shims");
    let persist_root = root.join("persist");
    let tx = session.event_bus().core_sender();

    for spec in specs {
        let _ = tx.try_send(Event::UpdateStart(
            spec.candidate.ident.clone(),
            spec.installed.version().to_owned(),
            spec.candidate.version().to_owned(),
        ));

        let app_dir = root.join("apps").join(spec.installed.name());
        let old_version_str = if spec.installed.is_nightly() {
            "nightly".to_owned()
        } else {
            spec.installed.version().to_owned()
        };
        let old_version_dir = app_dir.join(&old_version_str);
        let old_manifest = spec.installed.manifest.clone();
        let pkg_persist_dir = persist_root.join(spec.installed.name());

        let same_version = spec.installed.version() == spec.candidate.version();
        let temp_backup_dir = if same_version && old_version_dir.exists() {
            let backup = app_dir.join(format!("{old_version_str}_updating_old"));
            let _ = std::fs::rename(&old_version_dir, &backup);
            Some(backup)
        } else {
            None
        };
        let old_version_dir_for_scripts = temp_backup_dir.as_ref().unwrap_or(&old_version_dir);

        // --- STEP A: install the NEW version first. If this fails the old install is
        // UNTOUCHED (not one file of it has been modified), so the package is simply
        // skipped and the batch continues. ---
        let _ = tx.try_send(Event::UpdateProgress(
            "Installing new version...".to_owned(),
        ));

        let install_result = install::install_packages(
            session,
            std::slice::from_ref(&spec.candidate),
            &spec.downloaded,
            spec.arch,
        )
        .await;

        if let Err(e) = &install_result {
            if let Some(ref backup) = temp_backup_dir {
                let _ = std::fs::rename(backup, &old_version_dir);
            }
            let _ = tx.try_send(Event::UpdateProgress(format!(
                "Failed to install new version, keeping old version intact: {e}"
            )));
            continue;
        }

        let mut installed_new = install_result?;

        let installed_new = match installed_new.pop() {
            Some(pkg) => pkg,
            None => {
                let _ = tx.try_send(Event::UpdateProgress(
                    "Install produced no package, keeping old version intact.".to_owned(),
                ));
                if let Some(ref backup) = temp_backup_dir {
                    let _ = std::fs::rename(backup, &old_version_dir);
                }
                continue;
            }
        };

        // --- STEP B: the point of no return has been passed — the new version is ALIVE
        // and `current` already points at it (install_packages does this via
        // link_current). It is now safe to tear down whatever the old version left
        // behind that the new one did not take over. ---

        // B1. pre_uninstall script of the OLD version (best effort, never blocks)
        if let Some(script_lines) = old_manifest.resolve_pre_uninstall(spec.arch) {
            let ctx = script::HookContext::new(
                old_version_dir_for_scripts,
                &pkg_persist_dir,
                old_version_dir_for_scripts,
                spec.installed.version(),
            );
            let _ = script::run_powershell_script(
                &script_lines.iter().cloned().collect::<Vec<_>>(),
                &ctx,
            );
        }

        // B2. Drop ORPHANED shims: those the old manifest declared and the new one does
        // not. When the new version ships the same binary, install_packages has already
        // rewritten the shim with its new target, so there is nothing left to do.
        if let Some(old_bin_val) = old_manifest.resolve_bin(spec.arch) {
            let old_entries = shim::parse_bin(old_bin_val);
            let new_bin_val = installed_new.manifest.resolve_bin(spec.arch);
            let new_names: HashSet<String> = new_bin_val
                .map(|b| {
                    shim::parse_bin(b)
                        .iter()
                        .map(|e| e.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into_iter()
                .collect();
            let orphaned: Vec<_> = old_entries
                .into_iter()
                .filter(|e| !new_names.contains(&e.name))
                .collect();
            if !orphaned.is_empty() {
                let _ = shim::remove_shims(&orphaned, &shims_dir);
            }
        }

        // B3. Drop environment variables of the old version that the new one lacks.
        if let Some(old_env) = old_manifest.resolve_env_set(spec.arch) {
            let new_env = installed_new.manifest.resolve_env_set(spec.arch);
            for k in old_env.keys() {
                let still_present = new_env.is_some_and(|m| m.contains_key(k));
                if !still_present {
                    let _ = session.env_service().remove_env(k);
                }
            }
        }
        // B3b. Drop from PATH whatever the old manifest added.
        //
        // Goes through install's shared resolver so the entry removed is exactly the
        // one that was added. The previous code took the raw manifest string ("bin")
        // and tried to remove that from PATH, which can never match: installation puts
        // the resolved "<apps>/<name>/current/bin" there instead.
        install::remove_env_add_paths(&old_manifest, spec.arch, &app_dir.join("current"));

        // B4. Drop the old version's shortcuts (install_packages has already created the
        // new version's: same name overwrites, a different name is simply added, and only
        // the leftover one still needs removing).
        if let Some(old_shortcuts) = old_manifest.resolve_shortcuts(spec.arch) {
            let entries: Vec<shortcut::ShortcutEntry> = old_shortcuts
                .iter()
                .map(|s| shortcut::ShortcutEntry {
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

        // B5. Unlink the old version's persist entries. The persist root is shared
        // across versions, so the stored data is left alone; only the junction pointing
        // into the old version directory has to go before that directory is removed.
        if let Some(ref persist_val) = old_manifest.persist {
            let entries = persist::parse_persist(persist_val);
            let _ = persist::unlink(&entries, old_version_dir_for_scripts);
        }

        // B6. post_uninstall script of the OLD version (best effort)
        if let Some(script_lines) = old_manifest.resolve_post_uninstall(spec.arch) {
            let ctx = script::HookContext::new(
                old_version_dir_for_scripts,
                &pkg_persist_dir,
                old_version_dir_for_scripts,
                spec.installed.version(),
            );
            let _ = script::run_powershell_script(
                &script_lines.iter().cloned().collect::<Vec<_>>(),
                &ctx,
            );
        }

        // B7. Remove the old version directory (or temp_backup_dir, if one was made).
        if let Some(ref backup) = temp_backup_dir {
            let _ = std::fs::rename(backup, app_dir.join(format!("{old_version_str}_old")));
        } else if old_version_dir.exists() {
            let _ = fs::remove_dir(&old_version_dir);
        }

        let _ = tx.try_send(Event::UpdateDone(installed_new.ident.clone()));
        updated.push(installed_new);
    }

    Ok(updated)
}
