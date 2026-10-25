use crate::Result;
use crate::infra::fs;
use crate::infra::{install_meta, persist, script, shim, shortcut};
use crate::operations::install;
use crate::session::Session;

#[derive(Debug, Clone)]
pub struct Uninstalled {
    pub name: String,
    pub version: String,
    /// What could not be removed, with the reason.
    ///
    /// The removal used to discard its error with `let _ =` and announce the app as
    /// uninstalled regardless, so files that were still on disk — the app was running,
    /// a handle was open — were reported as gone. Scoop stops and says
    /// "Couldn't remove '<dir>'; it may be in use." instead.
    pub failed: Vec<String>,
}

pub fn uninstall_packages(
    session: &Session,
    names: &[String],
    purge: bool,
) -> Result<Vec<Uninstalled>> {
    let _guard = session.write_lock()?;
    let roots = install::InstallRoots::from_session(session);
    let apps_root = roots.apps.clone();
    if !apps_root.exists() {
        return Ok(Vec::new());
    }

    let is_wildcard = names.iter().any(|p| p == "*" || p == "-a" || p == "--all");
    let mut result = Vec::new();
    let shims_dir = roots.shims.clone();
    let persist_root = roots.persist.clone();

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
        // The architecture the app was installed for, not the machine's — the
        // manifest's architecture blocks were resolved with it.
        let arch = install_meta::installed_arch(&version_dir);
        let pkg_persist_dir = persist_root.join(&name);
        let version_str = version.as_deref().unwrap_or("");

        // The order below is Scoop's `uninstall_app`
        // (libexec/scoop-uninstall.ps1): the hooks and the package's own
        // uninstaller run while everything is still in place, the links and
        // shims are taken down next, and the directory is removed last.

        // 1. pre_uninstall, best effort: a notification hook must not be able to
        //    block an uninstall.
        if let Some(ref m) = manifest
            && let Some(script_lines) = m.resolve_pre_uninstall(arch)
        {
            let ctx =
                script::HookContext::new(&version_dir, &pkg_persist_dir, &version_dir, version_str);
            let _ = script::run_powershell_script(
                &script_lines.iter().cloned().collect::<Vec<_>>(),
                &ctx,
            );
        }

        // 2. The package's own uninstaller. It runs before the persisted data is
        //    unlinked, because it is usually the only thing that knows how to
        //    clean up what it wrote there.
        if let Some(ref m) = manifest {
            install::run_installer(
                m,
                arch,
                &version_dir,
                &pkg_persist_dir,
                version_str,
                &roots,
                true,
            )?;
        }

        // 3. Unlink persist junctions/hardlinks **before** anything deletes a
        //    directory. A junction still in place would make the recursive delete
        //    walk straight into `persist/<app>` and take the user's data with it.
        //
        //    Both the `current` link and every version directory are covered: an
        //    update leaves old versions behind, and each of them can carry its own
        //    links.
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

        // 4. Environment: `env_set` keys and the `env_add_path` entries, resolved
        //    through the same helper the installer used so the removed entry is
        //    exactly the one that was added.
        if let Some(ref m) = manifest {
            if let Some(env_set) = m.resolve_env_set(arch) {
                for k in env_set.keys() {
                    session.env_service().remove_env(k)?;
                }
            }
            install::remove_env_add_paths(m, arch, &app_dir.join("current"));
        }

        // 5. The PowerShell module link, before the version directory it points
        //    at is removed — a junction left behind would dangle.
        if let Some(ref m) = manifest {
            install::uninstall_psmodule(session, m, &roots)?;
        }

        // 6. Shims. `remove_shims` clears every wrapper spelling the installer
        //    can produce (`.exe`/`.shim` for exe targets, `.cmd` and the bare
        //    POSIX script for batch/jar/py targets, `.ps1` for scripts).
        if let Some(ref m) = manifest
            && let Some(bin_val) = m.resolve_bin(arch)
        {
            let entries = shim::parse_bin(bin_val);
            shim::remove_shims(&entries, &shims_dir)?;
        }

        // 7. Start Menu shortcuts.
        if let Some(ref m) = manifest
            && let Some(shortcut_list) = m.resolve_shortcuts(arch)
        {
            let entries = install::parse_shortcuts(shortcut_list);
            if !entries.is_empty() {
                let _ = shortcut::remove_shortcuts(&entries, false);
            }
        }

        // 8. Remove the `current` junction itself, then every version directory.
        let current_link = app_dir.join("current");
        if current_link.exists() || current_link.is_symlink() {
            fs::remove_symlink(&current_link)?;
        }

        let mut failed = Vec::new();
        if let Ok(version_entries) = std::fs::read_dir(&app_dir) {
            for ve in version_entries.flatten() {
                let path = ve.path();
                if path.is_dir()
                    && let Err(e) = fs::remove_dir(&path)
                {
                    failed.push(format!("{}: {e}", path.display()));
                }
            }
        }

        // 9. Remove the app directory itself. It can legitimately refuse when
        //    something is left inside, so only a genuine failure is worth
        //    reporting — and by this point a failure has already been recorded
        //    above.
        if let Err(e) = fs::remove_dir(&app_dir)
            && failed.is_empty()
        {
            failed.push(format!("{}: {e}", app_dir.display()));
        }

        // 10. post_uninstall, best effort, after the app is gone but while the
        //     persisted data is still there.
        if let Some(ref m) = manifest
            && let Some(script_lines) = m.resolve_post_uninstall(arch)
            && pkg_persist_dir.exists()
        {
            let ctx =
                script::HookContext::new(&version_dir, &pkg_persist_dir, &version_dir, version_str);
            let _ = script::run_powershell_script(
                &script_lines.iter().cloned().collect::<Vec<_>>(),
                &ctx,
            );
        }

        // 11. `--purge` is the explicit request to destroy the persisted data.
        if purge && pkg_persist_dir.exists() {
            fs::remove_dir(&pkg_persist_dir)?;
        }

        result.push(Uninstalled {
            name,
            version: version.unwrap_or_default(),
            failed,
        });
    }

    Ok(result)
}

fn resolve_version(app_dir: &std::path::Path) -> Option<String> {
    let current_dir = app_dir.join("current");
    let manifest_path = crate::infra::install_meta::INSTALLED_MANIFEST;
    let manifest_path = if current_dir.join(manifest_path).is_file() {
        current_dir.join(manifest_path)
    } else {
        current_dir.join(crate::infra::install_meta::INSTALLED_MANIFEST_LEGACY)
    };
    if let Ok(content) = crate::infra::json::read_to_string(&manifest_path)
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
    crate::infra::install_meta::read_installed_manifest(version_dir)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::config::Config;
    use tempfile::tempdir;

    /// Installs one app with the given manifest and returns the roots.
    ///
    /// The manifest deliberately never declares `shortcuts` or `env_add_path`: shortcuts
    /// would be created in the real Start Menu and PATH is written to the real registry.
    /// The environment here goes through a recorder instead.
    fn install(root: &std::path::Path, name: &str, version: &str, manifest: &str) {
        let app_dir = root.join("apps").join(name);
        let version_dir = app_dir.join(version);
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("tool.exe"), b"x").unwrap();
        std::fs::write(
            version_dir.join(crate::infra::install_meta::INSTALLED_MANIFEST),
            manifest,
        )
        .unwrap();
        fs::create_junction(&version_dir, &app_dir.join("current")).unwrap();
    }

    fn session(root: &std::path::Path) -> (Session, crate::session::RecordingEnvService) {
        Session::from_config_recording_env(Config {
            root_path: Some(root.to_path_buf()),
            ..Default::default()
        })
    }

    /// The distinction that matters most: a normal uninstall removes the application but
    /// keeps the user's persisted files.
    #[test]
    fn uninstall_removes_the_app_but_keeps_persisted_data() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(
            root,
            "demo",
            "1.0.0",
            r#"{"version":"1.0.0","persist":"config"}"#,
        );

        let persist_dir = root.join("persist").join("demo");
        std::fs::create_dir_all(persist_dir.join("config")).unwrap();
        std::fs::write(persist_dir.join("config").join("user.cfg"), b"mine").unwrap();
        fs::create_junction(
            &persist_dir.join("config"),
            &root.join("apps/demo/current/config"),
        )
        .unwrap();

        let result = uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "demo");
        assert_eq!(result[0].version, "1.0.0");
        assert!(
            !root.join("apps/demo").exists(),
            "the app directory should be gone"
        );
        assert!(
            persist_dir.join("config").join("user.cfg").is_file(),
            "user data must survive a plain uninstall"
        );
        assert_eq!(
            std::fs::read(persist_dir.join("config").join("user.cfg")).unwrap(),
            b"mine"
        );
    }

    /// `--purge` is the explicit request to destroy the data, and it must actually do so.
    #[test]
    fn purge_removes_persisted_data() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(
            root,
            "demo",
            "1.0.0",
            r#"{"version":"1.0.0","persist":"config"}"#,
        );

        let persist_dir = root.join("persist").join("demo");
        std::fs::create_dir_all(persist_dir.join("config")).unwrap();
        std::fs::write(persist_dir.join("config").join("user.cfg"), b"mine").unwrap();

        uninstall_packages(&s, &["demo".to_owned()], true).unwrap();

        assert!(
            !persist_dir.exists(),
            "purge should remove the persisted data"
        );
    }

    /// A junction left in place would make the recursive delete walk straight into
    /// `persist/`, so it has to be taken out first.
    #[test]
    fn uninstall_unlinks_persist_before_deleting_the_version_directory() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(
            root,
            "demo",
            "1.0.0",
            r#"{"version":"1.0.0","persist":"config"}"#,
        );

        let persist_dir = root.join("persist").join("demo");
        std::fs::create_dir_all(persist_dir.join("config")).unwrap();
        std::fs::write(persist_dir.join("config").join("user.cfg"), b"mine").unwrap();
        fs::create_junction(
            &persist_dir.join("config"),
            &root.join("apps/demo/current/config"),
        )
        .unwrap();

        uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        // Not only must the data survive, the persist entry must still be reachable.
        assert_eq!(
            std::fs::read(persist_dir.join("config").join("user.cfg")).unwrap(),
            b"mine"
        );
        assert!(
            !root.join("apps").join("demo").exists(),
            "no dangling reference may be left behind"
        );
    }

    /// Shims outlive the app directory unless they are removed, and a leftover shim
    /// points at a path that no longer exists.
    #[test]
    fn uninstall_removes_the_shims() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(
            root,
            "demo",
            "1.0.0",
            r#"{"version":"1.0.0","bin":"tool.exe"}"#,
        );

        let shims_dir = root.join("shims");
        std::fs::create_dir_all(&shims_dir).unwrap();
        std::fs::write(shims_dir.join("tool.shim"), b"shim").unwrap();
        std::fs::write(shims_dir.join("other.shim"), b"unrelated").unwrap();

        uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        assert!(
            !shims_dir.join("tool.shim").exists(),
            "the app's shim should be removed"
        );
        assert!(
            shims_dir.join("other.shim").exists(),
            "another app's shim must be left alone"
        );
    }

    /// Every version directory goes, not just the current one.
    #[test]
    fn uninstall_removes_old_versions_too() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        let app_dir = root.join("apps").join("demo");
        for version in ["1.0.0", "2.0.0"] {
            std::fs::create_dir_all(app_dir.join(version)).unwrap();
            std::fs::write(
                app_dir
                    .join(version)
                    .join(crate::infra::install_meta::INSTALLED_MANIFEST),
                format!(r#"{{"version":"{version}"}}"#),
            )
            .unwrap();
        }
        fs::create_junction(&app_dir.join("2.0.0"), &app_dir.join("current")).unwrap();

        uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        assert!(!app_dir.exists(), "the whole app directory should be gone");
    }

    /// Environment variables are removed through the service, which in tests is a
    /// recorder — so this asserts the intent without touching the user's registry.
    #[test]
    fn environment_variables_are_removed_via_the_env_service() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, env) = session(root);
        install(
            root,
            "demo",
            "1.0.0",
            r#"{"version":"1.0.0","env_set":{"DEMO_ONE":"1","DEMO_TWO":"2"}}"#,
        );

        uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        let mut removed = env.removed.lock().unwrap().clone();
        removed.sort();
        assert_eq!(removed, vec!["DEMO_ONE".to_owned(), "DEMO_TWO".to_owned()]);
    }

    /// An app with no manifest under `current` has no version to uninstall, so it is
    /// skipped rather than guessed at.
    #[test]
    fn app_without_a_manifest_is_skipped() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        let app_dir = root.join("apps").join("demo");
        std::fs::create_dir_all(&app_dir).unwrap();

        let result = uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        assert!(
            result.is_empty(),
            "nothing could be resolved, so nothing done"
        );
        assert!(app_dir.exists(), "and nothing was deleted");
    }

    /// Uninstalling one app must not touch another, or the wildcard.
    #[test]
    fn only_the_named_app_is_uninstalled() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(root, "demo", "1.0.0", r#"{"version":"1.0.0"}"#);
        install(root, "other", "1.0.0", r#"{"version":"1.0.0"}"#);

        uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        assert!(!root.join("apps/demo").exists());
        assert!(root.join("apps/other/current").exists());
    }

    /// Scoop itself is left alone.
    #[test]
    fn scoop_is_never_uninstalled() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(root, "scoop", "0.4.0", r#"{"version":"0.4.0"}"#);

        let result = uninstall_packages(&s, &["*".to_owned()], false).unwrap();

        assert!(result.is_empty());
        assert!(root.join("apps/scoop").exists());
    }

    /// A version directory that cannot be deleted must be reported, not announced as a
    /// clean uninstall. Same defect as in cleanup: the error was discarded with `let _ =`
    /// and the app was reported as uninstalled while its files were still on disk.
    #[cfg(windows)]
    #[test]
    fn a_version_that_cannot_be_removed_is_reported_rather_than_swallowed() {
        use std::os::windows::fs::OpenOptionsExt;

        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(root, "demo", "1.0.0", r#"{"version":"1.0.0"}"#);

        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(root.join("apps/demo/1.0.0/tool.exe"))
            .unwrap();

        let result = uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        assert_eq!(result.len(), 1);
        assert!(
            !result[0].failed.is_empty(),
            "the leftover file must be reported: {result:?}"
        );
        assert!(
            root.join("apps/demo/1.0.0/tool.exe").exists(),
            "and it is indeed still on disk"
        );

        drop(held);
    }

    /// The ordinary case has nothing to report — a clean uninstall must not invent
    /// warnings.
    #[test]
    fn a_clean_uninstall_reports_no_failures() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(root, "demo", "1.0.0", r#"{"version":"1.0.0"}"#);

        let result = uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        assert!(result[0].failed.is_empty(), "got {:?}", result[0].failed);
        assert!(!root.join("apps/demo").exists());
    }

    /// The package's own uninstaller has to run **before** the directory is
    /// removed, and with `$dir` substituted — it is the only thing that knows how
    /// to clean up whatever the installer set up.
    ///
    /// The marker is written into `persist/` on purpose: the version directory is
    /// gone by the time the test can look, so a marker left there would prove
    /// nothing.
    #[test]
    fn the_packages_own_uninstaller_runs_before_the_directory_is_removed() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (s, _env) = session(root);
        install(
            root,
            "demo",
            "1.0.0",
            r#"{"version":"1.0.0","uninstaller":{"file":"uninstall.bat","args":["$dir"]}}"#,
        );

        let version_dir = root.join("apps").join("demo").join("1.0.0");
        let persist_dir = root.join("persist").join("demo");
        std::fs::create_dir_all(&persist_dir).unwrap();
        let marker = persist_dir.join("uninstalled.txt");

        std::fs::write(
            version_dir.join("uninstall.bat"),
            format!("@echo off\r\necho %1 > \"{}\"\r\n", marker.display()),
        )
        .unwrap();

        uninstall_packages(&s, &["demo".to_owned()], false).unwrap();

        assert!(marker.is_file(), "the package's uninstaller must run");
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim_end(),
            version_dir.display().to_string(),
            "$dir must be substituted with the version directory"
        );
    }
}
