use std::collections::HashSet;

use std::path::PathBuf;

use rake_domain::arch::Arch;
use rake_domain::package::Package;

use crate::Result;
use crate::event::{BucketState, Event};
use crate::infra::git::{ExternalGit, GitService};
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
    let git = ExternalGit::new();
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

        let same_version = spec.installed.version() == spec.candidate.version();
        let temp_backup_dir = if same_version && old_version_dir.exists() {
            let backup = app_dir.join(format!("{old_version_str}_updating_old"));
            let _ = std::fs::rename(&old_version_dir, &backup);
            Some(backup)
        } else {
            None
        };
        let old_version_dir_for_scripts = temp_backup_dir.as_ref().unwrap_or(&old_version_dir);

        // --- ШАГ A: установить НОВУЮ версию первой. Если это упадёт — старая
        // установка НЕ ТРОНУТА (ни один файл старой версии ещё не тронут),
        // просто пропускаем этот пакет и идём дальше по батчу. ---
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

        // --- ШАГ B: точка невозврата пройдена — новая версия УЖЕ живая,
        // current уже указывает на неё (это делает install_packages внутри
        // finalize_installation). Теперь безопасно снести артефакты старой
        // версии, которые новая версия не переиспользовала. ---

        // B1. pre_uninstall script СТАРОЙ версии (best effort, не блокирует)
        if let Some(script_lines) = old_manifest.resolve_pre_uninstall(spec.arch) {
            let ctx = script::HookContext {
                version_dir: old_version_dir_for_scripts,
                persist_dir: &persist_root.join(spec.installed.name()),
                original_dir: old_version_dir_for_scripts,
                version: spec.installed.version(),
            };
            let _ = script::run_powershell_script(
                &script_lines.iter().cloned().collect::<Vec<_>>(),
                &ctx,
            );
        }

        // B2. Убрать ОРФАН-шимы: те, что были в старом манифесте, но которых
        // НЕТ в новом манифесте (если бинарник новой версии тот же — install_packages
        // уже перезаписал файл шима новым таргетом, трогать не нужно и не надо).
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

        // B3. Убрать env-переменные старой версии, которых нет в новой.
        if let Some(old_env) = old_manifest.resolve_env_set(spec.arch) {
            let new_env = installed_new.manifest.resolve_env_set(spec.arch);
            for k in old_env.keys() {
                let still_present = new_env.is_some_and(|m| m.contains_key(k));
                if !still_present {
                    let _ = session.env_service().remove_env(k);
                }
            }
        }
        if let Some(old_add_path) = old_manifest.resolve_env_add_path(spec.arch) {
            for path in old_add_path.iter() {
                let _ = session.env_service().remove_path(path);
            }
        }

        // B4. Убрать шорткаты старой версии (шорткаты новой версии уже создал
        // install_packages, если имя совпадает — перезаписал; если нет — просто
        // добавил новый, старый нужно снести отдельно).
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

        // B5. Unlink persist старой версии (persist_root общий между версиями,
        // трогать сами данные не нужно — только снять junction со старой
        // директории, которую мы сейчас удалим).
        if let Some(ref persist_val) = old_manifest.persist {
            let entries = persist::parse_persist(persist_val);
            let _ = persist::unlink(&entries, old_version_dir_for_scripts);
        }

        // B6. post_uninstall script СТАРОЙ версии (best effort)
        if let Some(script_lines) = old_manifest.resolve_post_uninstall(spec.arch) {
            let ctx = script::HookContext {
                version_dir: old_version_dir_for_scripts,
                persist_dir: &persist_root.join(spec.installed.name()),
                original_dir: old_version_dir_for_scripts,
                version: spec.installed.version(),
            };
            let _ = script::run_powershell_script(
                &script_lines.iter().cloned().collect::<Vec<_>>(),
                &ctx,
            );
        }

        // B7. Удалить директорию старой версии (или temp_backup_dir если был).
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
