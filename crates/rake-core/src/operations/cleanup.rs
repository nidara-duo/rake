use std::path::{Path, PathBuf};

use crate::Result;
use crate::infra::fs;
use crate::infra::persist;
use crate::operations::cache;
use crate::session::Session;

#[derive(Debug, Clone, Copy)]
pub enum CleanupOption {
    Cache,
}

#[derive(Debug, Clone)]
pub struct CleanupResult {
    pub name: String,
    pub removed_versions: Vec<String>,
    /// Versions that could not be removed, with the reason.
    ///
    /// The previous code ignored the result of the removal and pushed the version into
    /// `removed_versions` regardless, so a version directory that could not be deleted —
    /// a running application, a file held open — was reported as removed. Scoop fails
    /// loudly instead (`Remove-Item -ErrorAction Stop`, and "it may be in use").
    pub failed_versions: Vec<(String, String)>,
}

pub fn cleanup_packages(
    session: &Session,
    names: &[String],
    options: &[CleanupOption],
) -> Result<Vec<CleanupResult>> {
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

    let clean_cache = options.iter().any(|o| matches!(o, CleanupOption::Cache));

    let is_wildcard = names.iter().any(|p| p == "*" || p == "-a" || p == "--all");
    let mut results = Vec::new();

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
        let current_link = app_dir.join("current");

        let current_version = if current_link.exists() || current_link.is_symlink() {
            std::fs::read_link(&current_link)
                .ok()
                .and_then(|p| p.file_name().and_then(|s| s.to_str()).map(|s| s.to_owned()))
        } else {
            None
        };

        if clean_cache {
            // Keeps the download for the version in use, as scoop does.
            cache::cache_remove_except(session, &name, current_version.as_deref())?;
        }

        let mut removed_versions = Vec::new();
        let mut failed_versions = Vec::new();
        if let Ok(version_entries) = std::fs::read_dir(&app_dir) {
            for ve in version_entries.flatten() {
                let vname = ve.file_name().to_string_lossy().to_string();
                if vname == "current" || Some(&vname) == current_version.as_ref() {
                    continue;
                }

                // version directory
                let vdir = ve.path();
                if vdir.is_dir() {
                    // Scoop logic: unlink persist data before removing dir
                    let manifest = load_manifest(&app_dir.join(&vname));
                    if let Some(ref m) = manifest
                        && let Some(ref persist_val) = m.persist
                    {
                        let entries = persist::parse_persist(persist_val);
                        let _ = persist::unlink(&entries, &vdir);
                    }

                    match fs::remove_dir(&vdir) {
                        Ok(()) => removed_versions.push(vname),
                        // Scoop gives up on the rest of this app and moves on to the
                        // next one, so stop here but keep the failure visible.
                        Err(e) => {
                            failed_versions.push((vname, e.to_string()));
                            break;
                        }
                    }
                }
            }
        }

        if !removed_versions.is_empty() || !failed_versions.is_empty() {
            results.push(CleanupResult {
                name,
                removed_versions,
                failed_versions,
            });
        }
    }

    if clean_cache {
        cache::cache_remove_partials(session)?;
    }

    Ok(results)
}

fn load_manifest(version_dir: &Path) -> Option<rake_domain::manifest::Manifest> {
    crate::infra::install_meta::read_installed_manifest(version_dir)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::config::Config;
    use tempfile::tempdir;

    fn make_session(root: &Path) -> Session {
        Session::from_config(Config {
            root_path: Some(root.to_path_buf()),
            ..Default::default()
        })
    }

    /// Install two versions and point `current` at the newer one, the way
    /// `link_current` does.
    fn install_app(root: &Path, name: &str, versions: &[(&str, &str)]) {
        let app_dir = root.join("apps").join(name);
        std::fs::create_dir_all(&app_dir).unwrap();
        for (version, manifest) in versions {
            let version_dir = app_dir.join(version);
            std::fs::create_dir_all(&version_dir).unwrap();
            std::fs::write(version_dir.join("tool.exe"), b"x").unwrap();
            std::fs::write(
                version_dir.join(crate::infra::install_meta::INSTALLED_MANIFEST),
                manifest,
            )
            .unwrap();
        }
        let current_version = versions.last().unwrap().0;
        crate::infra::fs::create_junction(&app_dir.join(current_version), &app_dir.join("current"))
            .unwrap();
    }

    const MANIFEST: &str = r#"{"version":"1.0.0"}"#;
    const MANIFEST_2: &str = r#"{"version":"2.0.0"}"#;

    /// The whole point of cleanup: the version `current` points at survives, and only
    /// the older ones go.
    ///
    /// This is the case worth being careful about, because the version is found by
    /// reading the `current` junction. If that lookup silently failed, the current
    /// version directory would be deleted while `current` still pointed at it, and
    /// the app would stop working with no error reported anywhere.
    #[test]
    fn cleanup_keeps_the_current_version_and_removes_older_ones() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        install_app(
            tmp.path(),
            "demo",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );

        let app_dir = tmp.path().join("apps").join("demo");
        let results = cleanup_packages(&session, &["demo".to_owned()], &[]).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].removed_versions, vec!["1.0.0".to_owned()]);

        assert!(
            !app_dir.join("1.0.0").exists(),
            "the old version should be gone"
        );
        assert!(
            app_dir.join("2.0.0").exists(),
            "the version current points at must survive"
        );
        assert!(
            app_dir.join("current").join("tool.exe").is_file(),
            "current must still resolve to real files"
        );
    }

    /// Pin the mechanism itself: the current version is identified by reading the
    /// `current` junction, so this asserts that read actually works on a junction
    /// rather than assuming it.
    #[test]
    fn current_junction_is_readable_on_this_platform() {
        let tmp = tempdir().unwrap();
        install_app(
            tmp.path(),
            "demo",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );
        let app_dir = tmp.path().join("apps").join("demo");

        let target =
            std::fs::read_link(app_dir.join("current")).expect("read_link must resolve a junction");
        assert_eq!(
            target.file_name().and_then(|s| s.to_str()),
            Some("2.0.0"),
            "the current version is found by this lookup, so it has to work"
        );
    }

    /// With a single version there is nothing to clean, and the app must be left
    /// exactly as it was.
    #[test]
    fn single_version_app_is_left_alone() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        install_app(tmp.path(), "demo", &[("1.0.0", MANIFEST)]);

        let results = cleanup_packages(&session, &["demo".to_owned()], &[]).unwrap();

        assert!(
            results.is_empty(),
            "nothing was removed, so nothing should be reported"
        );
        assert!(tmp.path().join("apps/demo/1.0.0").is_dir());
    }

    /// An app with no `current` link at all — a failed or interrupted install. There is
    /// no version to protect, so every version directory is a candidate.
    #[test]
    fn app_without_a_current_link_has_all_versions_removed() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        let app_dir = tmp.path().join("apps").join("demo");
        for version in ["1.0.0", "2.0.0"] {
            std::fs::create_dir_all(app_dir.join(version)).unwrap();
        }

        let results = cleanup_packages(&session, &["demo".to_owned()], &[]).unwrap();

        let mut removed = results[0].removed_versions.clone();
        removed.sort();
        assert_eq!(removed, vec!["1.0.0".to_owned(), "2.0.0".to_owned()]);
    }

    #[test]
    fn cleanup_is_case_insensitive_on_the_app_name() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        install_app(
            tmp.path(),
            "Demo",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );

        let results = cleanup_packages(&session, &["demo".to_owned()], &[]).unwrap();

        assert_eq!(
            results.len(),
            1,
            "scoop matches app names case-insensitively"
        );
        assert!(!tmp.path().join("apps/Demo/1.0.0").exists());
    }

    /// Only the named apps are touched.
    #[test]
    fn other_apps_are_untouched() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        install_app(
            tmp.path(),
            "demo",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );
        install_app(
            tmp.path(),
            "other",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );

        cleanup_packages(&session, &["demo".to_owned()], &[]).unwrap();

        assert!(tmp.path().join("apps/other/1.0.0").is_dir());
        assert!(tmp.path().join("apps/other/2.0.0").is_dir());
    }

    /// Scoop itself is skipped, so cleaning everything cannot pull the ground out from
    /// under the manager that is doing the cleaning.
    #[test]
    fn scoop_itself_is_never_cleaned() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        install_app(
            tmp.path(),
            "scoop",
            &[("0.3.1", MANIFEST), ("0.4.0", MANIFEST_2)],
        );

        let results = cleanup_packages(&session, &["*".to_owned()], &[]).unwrap();

        assert!(
            results.is_empty(),
            "scoop must not appear in the cleanup report"
        );
        assert!(tmp.path().join("apps/scoop/0.3.1").is_dir());
    }

    /// The persisted data of an old version must be unlinked before the directory is
    /// removed, or the recursive delete would follow the junction straight into
    /// `persist/` and destroy the user's files.
    ///
    /// This is the data-loss case: `persist/<app>/config/user.cfg` has to still be
    /// there afterwards.
    #[test]
    fn persisted_data_survives_cleaning_an_old_version() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        let app_dir = tmp.path().join("apps").join("demo");
        let persist_dir = tmp.path().join("persist").join("demo");

        let manifest = r#"{"version":"1.0.0","persist":"config"}"#;
        install_app(
            tmp.path(),
            "demo",
            &[("1.0.0", manifest), ("2.0.0", r#"{"version":"2.0.0"}"#)],
        );

        // The old version has a persist junction pointing into the persist store.
        std::fs::create_dir_all(persist_dir.join("config")).unwrap();
        std::fs::write(persist_dir.join("config").join("user.cfg"), b"mine").unwrap();
        crate::infra::fs::create_junction(
            &persist_dir.join("config"),
            &app_dir.join("1.0.0").join("config"),
        )
        .unwrap();

        cleanup_packages(&session, &["demo".to_owned()], &[]).unwrap();

        assert!(
            !app_dir.join("1.0.0").exists(),
            "the old version should be gone"
        );
        assert!(
            persist_dir.join("config").join("user.cfg").is_file(),
            "user data must survive: a recursive delete must not follow the junction"
        );
        assert_eq!(
            std::fs::read(persist_dir.join("config").join("user.cfg")).unwrap(),
            b"mine"
        );
    }

    /// A missing `apps` directory is not an error, and there is nothing to report.
    #[test]
    fn empty_root_reports_nothing() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        assert!(
            cleanup_packages(&session, &["demo".to_owned()], &[])
                .unwrap()
                .is_empty()
        );
    }

    /// An app name that is not installed is reported as nothing removed rather than
    /// failing.
    #[test]
    fn unknown_app_is_not_an_error() {
        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        install_app(
            tmp.path(),
            "demo",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );

        let results = cleanup_packages(&session, &["nosuchapp".to_owned()], &[]).unwrap();

        assert!(results.is_empty());
    }

    /// A version directory that cannot be removed must not be reported as removed.
    ///
    /// Reproduced by holding an exclusive handle on a file inside the old version
    /// directory, which is the same situation as an application still running from that
    /// version. `File::open` is not enough for this: it shares delete access, so the
    /// removal would succeed. The old code discarded the error and pushed the version
    /// into `removed_versions` regardless, so the CLI printed a green tick for a
    /// directory that was still on disk.
    #[cfg(windows)]
    #[test]
    fn a_version_that_cannot_be_removed_is_reported_as_failed() {
        use std::os::windows::fs::OpenOptionsExt;

        let tmp = tempdir().unwrap();
        let session = make_session(tmp.path());
        install_app(
            tmp.path(),
            "demo",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );
        let app_dir = tmp.path().join("apps").join("demo");

        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(app_dir.join("1.0.0").join("tool.exe"))
            .unwrap();

        let results = cleanup_packages(&session, &["demo".to_owned()], &[]).unwrap();

        assert!(
            results[0].removed_versions.is_empty(),
            "nothing was actually removed, so nothing may be reported as removed: {:?}",
            results[0]
        );
        assert_eq!(
            results[0].failed_versions.len(),
            1,
            "the failure must be surfaced: {:?}",
            results[0]
        );
        assert_eq!(results[0].failed_versions[0].0, "1.0.0");
        assert!(
            app_dir.join("1.0.0").exists(),
            "and the directory is of course still there"
        );

        drop(held);
    }

    /// `-k` must not throw away the download for the version still installed.
    ///
    /// The old code called `cache_remove(session, "*")`, which emptied the whole cache
    /// before looking at any app. Scoop keeps the current version's entry
    /// (libexec/scoop-cleanup.ps1:33).
    #[test]
    fn cache_cleanup_keeps_the_current_versions_download() {
        let tmp = tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let session = Session::from_config(Config {
            root_path: Some(tmp.path().to_path_buf()),
            cache_path: Some(cache_dir.clone()),
            ..Default::default()
        });
        install_app(
            tmp.path(),
            "demo",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );

        for version in ["1.0.0", "2.0.0"] {
            std::fs::write(cache_dir.join(format!("demo#{version}#tool.zip")), b"x").unwrap();
        }
        std::fs::write(cache_dir.join("demo#2.0.0#tool.zip.download"), b"partial").unwrap();

        cleanup_packages(&session, &["demo".to_owned()], &[CleanupOption::Cache]).unwrap();

        assert!(
            !cache_dir.join("demo#1.0.0#tool.zip").exists(),
            "the stale version's download should be dropped"
        );
        assert!(
            cache_dir.join("demo#2.0.0#tool.zip").exists(),
            "the current version's download must be kept"
        );
        assert!(
            !cache_dir.join("demo#2.0.0#tool.zip.download").exists(),
            "an interrupted download should be swept"
        );
    }

    /// Another app's cache is not collateral damage of `-k` on one app.
    #[test]
    fn cache_cleanup_is_scoped_to_the_named_apps() {
        let tmp = tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let session = Session::from_config(Config {
            root_path: Some(tmp.path().to_path_buf()),
            cache_path: Some(cache_dir.clone()),
            ..Default::default()
        });
        install_app(
            tmp.path(),
            "demo",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );
        install_app(
            tmp.path(),
            "other",
            &[("1.0.0", MANIFEST), ("2.0.0", MANIFEST_2)],
        );

        for app in ["demo", "other"] {
            for version in ["1.0.0", "2.0.0"] {
                std::fs::write(cache_dir.join(format!("{app}#{version}#tool.zip")), b"x").unwrap();
            }
        }

        cleanup_packages(&session, &["demo".to_owned()], &[CleanupOption::Cache]).unwrap();

        assert!(cache_dir.join("other#1.0.0#tool.zip").exists());
        assert!(cache_dir.join("other#2.0.0#tool.zip").exists());
    }
}
