use rake_domain::package::InstallRecord;
use std::collections::HashMap;

use rake_domain::manifest::Manifest;
use rake_domain::package::{InstallState, Package, PackageIdent, PackageSource, PackageStatus};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::Result;
use crate::bucket::Bucket;
use crate::infra::install_meta;
use crate::session::Session;

/// Scoop's own `scoop` app is excluded from `installed_apps` in lib/core.ps1,
/// so that `rake list` and `scoop list` agree.
const SELF_APP: &str = "scoop";

/// Backward-compatibility shim: for install.json files written by old rake
/// builds that lacked a `bucket` field, try to extract the bucket name from
/// a URL that happened to contain a `buckets/<name>/...` path segment.
///
/// Newly-written install.json files always have an explicit `bucket` field,
/// so this function is only reached for pre-existing installs.
fn extract_bucket_from_url(url: &str) -> Option<String> {
    let path = std::path::Path::new(url);
    let components: Vec<&str> = path.iter().filter_map(|c| c.to_str()).collect();
    let pos = components.iter().position(|c| *c == "buckets")?;
    components.get(pos + 1).map(|s| s.to_string())
}

pub(crate) fn query_installed_inner(session: &Session) -> Result<Vec<Package>> {
    let root = session.config().root_path.as_ref().map(|p| p.join("apps"));

    let root = match root {
        Some(p) if p.exists() => p,
        _ => return Ok(vec![]),
    };

    let entries: Vec<_> = std::fs::read_dir(&root)
        .map_err(crate::Error::Io)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| e.file_name().to_string_lossy() != SELF_APP)
        .collect();

    let results: Vec<Package> = entries
        .par_iter()
        .filter_map(|entry| {
            let app_dir = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let current_dir = app_dir.join("current");

            // Both file spellings are accepted (see infra::install_meta): recent
            // Scoop writes `scoop-install.json` / `scoop-manifest.json`, older
            // builds wrote `install.json` / `manifest.json`. Reading only the
            // legacy names made every recently-installed app disappear from
            // `list`, `status`, `uninstall`, `hold` and `cleanup`.
            let install_info: Option<InstallRecord> =
                match install_meta::read_install_record(&current_dir) {
                    Ok(record) => record,
                    Err(e) => {
                        tracing::warn!("{}: unreadable install record: {e}", name);
                        None
                    }
                };

            let manifest: Option<Manifest> =
                match install_meta::read_installed_manifest(&current_dir) {
                    Ok(manifest) => manifest,
                    Err(e) => {
                        tracing::warn!("{}: unreadable installed manifest: {e}", name);
                        None
                    }
                };

            // A missing or corrupt manifest must not hide an app that is
            // installed on disk. Fall back to the version directory name, the
            // way Scoop's `Select-CurrentVersion` does.
            let manifest = match manifest {
                Some(m) => m,
                None => Manifest {
                    version: install_meta::fallback_version(&app_dir, &current_dir)
                        .unwrap_or_else(|| "unknown".to_owned()),
                    description: None,
                    homepage: None,
                    license: None,
                    url: None,
                    hash: None,
                    architecture: None,
                    depends: None,
                    bin: None,
                    extract_dir: None,
                    extract_to: None,
                    persist: None,
                    env_add_path: None,
                    env_set: None,
                    shortcuts: None,
                    innosetup: None,
                    checkver: None,
                    autoupdate: None,
                    pre_install: None,
                    post_install: None,
                    pre_uninstall: None,
                    post_uninstall: None,
                    installer: None,
                    uninstaller: None,
                    cookie: None,
                    notes: None,
                    suggest: None,
                },
            };

            let version = manifest.version().to_owned();

            let (arch, held, url) = install_info
                .as_ref()
                .map(|i| (i.arch.clone(), i.held, i.url.clone()))
                .unwrap_or_else(|| ("64bit".to_owned(), false, None));

            let bucket = install_info
                .as_ref()
                .and_then(|i| i.bucket.clone())
                .or_else(|| url.as_ref().and_then(|u| extract_bucket_from_url(u)));

            let ident =
                PackageIdent::new(bucket.clone().unwrap_or_else(|| "__unknown__".into()), name);
            let status = PackageStatus::Installed(InstallState {
                version,
                bucket,
                arch,
                held,
                url,
            });

            // source: None because install.json does not persist the
            // PackageSource variant (Bucket vs File).  The bucket name is
            // stored separately in the `bucket` field.  If URL-sourced
            // installs are implemented in the future, a `source` field
            // should be added to InstallRecord to recover this.
            Some(Package::new(ident, manifest, None, status))
        })
        .collect();

    Ok(results)
}

pub fn query_installed(session: &Session) -> Result<Vec<Package>> {
    let _guard = session.read_lock()?;
    query_installed_inner(session)
}

pub(crate) fn query_synced_inner(session: &Session) -> Result<Vec<Package>> {
    let buckets_dir = session
        .config()
        .root_path
        .as_ref()
        .map(|p| p.join("buckets"));

    let buckets_dir = match buckets_dir {
        Some(p) if p.exists() => p,
        _ => return Ok(vec![]),
    };

    let buckets: Vec<_> = std::fs::read_dir(&buckets_dir)
        .map_err(crate::Error::Io)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    let packages = buckets
        .into_par_iter()
        .flat_map(|entry| {
            let bucket_name = entry.file_name().to_string_lossy().to_string();
            let bucket_dir = entry.path().join("bucket");

            if !bucket_dir.exists() {
                return Vec::new();
            }

            let mut manifests: Vec<_> = WalkDir::new(&bucket_dir)
                .max_depth(2)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                .collect();

            manifests.sort_by_key(|e| e.path().to_owned());

            manifests
                .into_iter()
                .filter_map(|entry| {
                    let manifest_path = entry.path();
                    if let Ok(content) = crate::infra::json::read_to_string(manifest_path)
                        && let Ok(manifest) = serde_json::from_str::<Manifest>(&content)
                    {
                        let file_stem = manifest_path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let ident = PackageIdent::new(bucket_name.clone(), file_stem);
                        Some(Package::new(
                            ident,
                            manifest,
                            Some(PackageSource::Bucket(bucket_name.clone())),
                            PackageStatus::NotInstalled,
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect();

    Ok(packages)
}

pub fn query_synced(session: &Session) -> Result<Vec<Package>> {
    let _guard = session.read_lock()?;
    query_synced_inner(session)
}

pub fn query_synced_matching(
    session: &Session,
    queries: &[String],
    explicit: bool,
    with_description: bool,
) -> Result<Vec<Package>> {
    let _guard = session.read_lock()?;
    query_synced_matching_inner(session, queries, explicit, with_description)
}

pub(crate) fn query_synced_matching_inner(
    session: &Session,
    queries: &[String],
    explicit: bool,
    with_description: bool,
) -> Result<Vec<Package>> {
    let buckets_dir = session
        .config()
        .root_path
        .as_ref()
        .map(|p| p.join("buckets"));

    let buckets_dir = match buckets_dir {
        Some(p) if p.exists() => p,
        _ => return Ok(vec![]),
    };

    let buckets: Vec<_> = std::fs::read_dir(&buckets_dir)
        .map_err(crate::Error::Io)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    let queries_lower: Vec<_> = queries.iter().map(|q| q.to_ascii_lowercase()).collect();

    let packages = buckets
        .into_par_iter()
        .flat_map(|entry| {
            let bucket_name = entry.file_name().to_string_lossy().to_string();
            let bucket = match Bucket::from(&entry.path()) {
                Ok(b) => b,
                Err(_) => return Vec::new(),
            };

            let mut manifest_paths = bucket.manifest_paths();
            manifest_paths.sort();

            if with_description {
                manifest_paths
                    .into_iter()
                    .filter_map(|manifest_path| {
                        if let Ok(content) = crate::infra::json::read_to_string(&manifest_path)
                            && let Ok(manifest) = serde_json::from_str::<Manifest>(&content)
                        {
                            let file_stem = manifest_path
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let ident = PackageIdent::new(bucket_name.clone(), file_stem);
                            Some(Package::new(
                                ident,
                                manifest,
                                Some(PackageSource::Bucket(bucket_name.clone())),
                                PackageStatus::NotInstalled,
                            ))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                manifest_paths
                    .into_iter()
                    .filter(|manifest_path| {
                        let file_stem = manifest_path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let file_stem_lower = file_stem.to_ascii_lowercase();
                        queries_lower.iter().any(|q| {
                            if explicit {
                                file_stem_lower == q.as_str()
                            } else {
                                file_stem_lower.contains(q.as_str())
                            }
                        })
                    })
                    .filter_map(|manifest_path| {
                        if let Ok(content) = crate::infra::json::read_to_string(&manifest_path)
                            && let Ok(manifest) = serde_json::from_str::<Manifest>(&content)
                        {
                            let file_stem = manifest_path
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let ident = PackageIdent::new(bucket_name.clone(), file_stem);
                            Some(Package::new(
                                ident,
                                manifest,
                                Some(PackageSource::Bucket(bucket_name.clone())),
                                PackageStatus::NotInstalled,
                            ))
                        } else {
                            None
                        }
                    })
                    .collect()
            }
        })
        .collect();

    Ok(packages)
}

/// Whether an app directory represents a healthy install.
///
/// Mirrors Scoop's `failed()` in lib/core.ps1:
/// ```text
/// $hasCurrent = (get_config NO_JUNCTION) -or (Test-Path "$appPath\current")
/// return (Test-Path $appPath) -and !($hasCurrent -and (installed $app))
/// ```
/// So an app fails when its directory exists but either there is no
/// `current` junction, or `current` does not resolve to a version. Under
/// `NO_JUNCTION` the `current` check is skipped, because that layout never
/// creates a junction.
///
/// The directory listing comes from `installed_apps`, which deliberately
/// includes broken installs — that is the only way a failed install can be
/// reported at all.
pub fn is_failed_install(app_dir: &std::path::Path, no_junction: bool) -> bool {
    if !app_dir.exists() {
        return false;
    }

    let current_dir = app_dir.join("current");
    let has_current = no_junction || current_dir.exists();

    // `installed` == Select-CurrentVersion is non-null, which reads the version
    // from the manifest and otherwise falls back to the newest version dir.
    let installed = install_meta::read_installed_manifest(&current_dir)
        .ok()
        .flatten()
        .is_some_and(|m| !m.version().is_empty())
        || install_meta::fallback_version(app_dir, &current_dir).is_some();

    !(has_current && installed)
}

/// App directories present under `<root>/apps`, excluding `scoop`.
///
/// This is the disk-level view (`installed_apps` in etalon
/// lib/core.ps1:417-422), as opposed to [`query_installed`] which only returns
/// apps whose metadata could be read. `status` needs this broader view to spot
/// failed installs.
pub fn installed_app_dirs(session: &Session) -> Result<Vec<std::path::PathBuf>> {
    let root = session.config().root_path.as_ref().map(|p| p.join("apps"));

    let root = match root {
        Some(p) if p.exists() => p,
        _ => return Ok(vec![]),
    };

    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
        .map_err(crate::Error::Io)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| e.file_name().to_string_lossy() != SELF_APP)
        .map(|e| e.path())
        .collect();

    dirs.sort();
    Ok(dirs)
}

pub struct Snapshot {
    pub installed: Vec<Package>,
    pub synced: Vec<Package>,
    pub held_buckets: Vec<String>,
}

pub fn collect_snapshot(session: &Session) -> Result<Snapshot> {
    let _guard = session.read_lock()?;
    let installed = query_installed_inner(session)?;
    let synced = query_synced_inner(session)?;
    let held_buckets = crate::operations::bucket::bucket_held_names_inner(session)?;
    Ok(Snapshot {
        installed,
        synced,
        held_buckets,
    })
}

pub fn latest_versions_for_installed(
    session: &Session,
    installed: &[Package],
) -> Result<HashMap<String, String>> {
    use rayon::prelude::*;

    let buckets = crate::operations::bucket::bucket_list(session)?;
    let bucket_by_name: HashMap<&str, &crate::bucket::Bucket> =
        buckets.iter().map(|b| (b.name(), b)).collect();

    let results: Vec<(String, Option<String>)> = installed
        .par_iter()
        .map(|pkg| {
            let name_lower = pkg.name().to_ascii_lowercase();

            let recorded_bucket = match &pkg.status {
                PackageStatus::Installed(state) => state.bucket.as_deref(),
                _ => None,
            };

            let version = recorded_bucket
                .and_then(|b| bucket_by_name.get(b))
                .and_then(|b| b.load_manifest(pkg.name()))
                .map(|m| m.version().to_owned())
                .or_else(|| {
                    buckets
                        .iter()
                        .find_map(|b| b.load_manifest(pkg.name()))
                        .map(|m| m.version().to_owned())
                });

            (name_lower, version)
        })
        .collect();

    Ok(results
        .into_iter()
        .filter_map(|(n, v)| v.map(|v| (n, v)))
        .collect())
}

pub fn find_synced_by_names(session: &Session, names: &[&str]) -> Result<Vec<Package>> {
    let _guard = session.read_lock()?;
    let buckets = crate::operations::bucket::bucket_list(session)?;
    let mut found = Vec::new();

    for name in names {
        for bucket in &buckets {
            if let Some(manifest) = bucket.load_manifest(name) {
                let ident = PackageIdent::new(bucket.name().to_owned(), (*name).to_owned());
                found.push(Package::new(
                    ident,
                    manifest,
                    Some(PackageSource::Bucket(bucket.name().to_owned())),
                    PackageStatus::NotInstalled,
                ));
                break;
            }
        }
    }

    Ok(found)
}

pub fn find_all_synced_by_name(session: &Session, name: &str) -> Result<Vec<Package>> {
    let _guard = session.read_lock()?;
    let buckets = crate::operations::bucket::bucket_list(session)?;
    let mut found = Vec::new();

    for bucket in &buckets {
        if let Some(manifest) = bucket.load_manifest(name) {
            let ident = PackageIdent::new(bucket.name().to_owned(), name.to_owned());
            found.push(Package::new(
                ident,
                manifest,
                Some(PackageSource::Bucket(bucket.name().to_owned())),
                PackageStatus::NotInstalled,
            ));
        }
    }

    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::config::Config;
    use tempfile::tempdir;

    fn make_session(root: std::path::PathBuf) -> Session {
        let config = Config {
            root_path: Some(root),
            ..Default::default()
        };
        Session::from_config(config)
    }

    /// Write an installed app into `<root>/apps/<name>/<version>` plus a
    /// `current` junction target, using the given metadata file names.
    fn write_installed_app(
        root: &std::path::Path,
        name: &str,
        version: &str,
        manifest_file: &str,
        record_file: &str,
    ) {
        let version_dir = root.join("apps").join(name).join(version);
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join(manifest_file),
            format!(r#"{{"version":"{version}","description":"d"}}"#),
        )
        .unwrap();
        std::fs::write(
            version_dir.join(record_file),
            format!(
                r#"{{"version":"{version}","bucket":"main","architecture":"64bit","hold":false}}"#
            ),
        )
        .unwrap();

        // `current` is a junction on Windows; a plain directory copy of the
        // metadata is enough for the query path and keeps the test portable.
        let current = root.join("apps").join(name).join("current");
        std::fs::create_dir_all(&current).unwrap();
        for file in [manifest_file, record_file] {
            std::fs::copy(version_dir.join(file), current.join(file)).unwrap();
        }
    }

    #[test]
    fn query_installed_reads_current_scoop_metadata_names() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_installed_app(
            &root,
            "nu",
            "0.116.1",
            install_meta::INSTALLED_MANIFEST,
            install_meta::INSTALL_RECORD,
        );

        let session = make_session(root);
        let installed = query_installed(&session).unwrap();
        assert_eq!(installed.len(), 1, "app must not be hidden");
        assert_eq!(installed[0].name(), "nu");
        assert_eq!(installed[0].version(), "0.116.1");
    }

    #[test]
    fn query_installed_reads_legacy_metadata_names() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_installed_app(
            &root,
            "brave",
            "1.96.59",
            install_meta::INSTALLED_MANIFEST_LEGACY,
            install_meta::INSTALL_RECORD_LEGACY,
        );

        let session = make_session(root);
        let installed = query_installed(&session).unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name(), "brave");
    }

    #[test]
    fn query_installed_reads_both_spellings_side_by_side() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_installed_app(
            &root,
            "gh",
            "2.102.0",
            install_meta::INSTALLED_MANIFEST,
            install_meta::INSTALL_RECORD,
        );
        write_installed_app(
            &root,
            "7zip",
            "26.03",
            install_meta::INSTALLED_MANIFEST_LEGACY,
            install_meta::INSTALL_RECORD_LEGACY,
        );

        let session = make_session(root);
        let mut names: Vec<String> = query_installed(&session)
            .unwrap()
            .iter()
            .map(|p| p.name().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["7zip".to_owned(), "gh".to_owned()]);
    }

    #[test]
    fn query_installed_keeps_app_whose_manifest_is_missing() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // Only an install record, no manifest: the app is installed but its
        // metadata is gone. It must still be listed, with the version taken
        // from the version directory.
        let version_dir = root.join("apps").join("zed").join("1.22.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join(install_meta::INSTALL_RECORD),
            r#"{"version":"1.22.0","bucket":"main"}"#,
        )
        .unwrap();
        let current = root.join("apps").join("zed").join("current");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::copy(
            version_dir.join(install_meta::INSTALL_RECORD),
            current.join(install_meta::INSTALL_RECORD),
        )
        .unwrap();

        let session = make_session(root);
        let installed = query_installed(&session).unwrap();
        assert_eq!(installed.len(), 1, "installed app must not disappear");
        assert_eq!(installed[0].name(), "zed");
    }

    /// Create `<root>/apps/<name>/<version>` with metadata and a `current` dir.
    fn write_app_with_current(root: &std::path::Path, name: &str, version: &str) {
        let version_dir = root.join("apps").join(name).join(version);
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join(install_meta::INSTALLED_MANIFEST),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .unwrap();
        std::fs::write(
            version_dir.join(install_meta::INSTALL_RECORD),
            format!(r#"{{"version":"{version}","bucket":"main"}}"#),
        )
        .unwrap();

        let current = root.join("apps").join(name).join("current");
        std::fs::create_dir_all(&current).unwrap();
        for file in [
            install_meta::INSTALLED_MANIFEST,
            install_meta::INSTALL_RECORD,
        ] {
            std::fs::copy(version_dir.join(file), current.join(file)).unwrap();
        }
    }

    #[test]
    fn healthy_app_is_not_failed() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_app_with_current(&root, "git", "2.50.0");

        let app_dir = root.join("apps").join("git");
        assert!(!is_failed_install(&app_dir, false));
        assert!(!is_failed_install(&app_dir, true));
    }

    #[test]
    fn app_without_current_is_failed() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // A version dir exists but the junction was never created: this is the
        // state an interrupted install leaves behind.
        let version_dir = root.join("apps").join("brave").join("1.96.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join(install_meta::INSTALLED_MANIFEST),
            r#"{"version":"1.96.0"}"#,
        )
        .unwrap();

        let app_dir = root.join("apps").join("brave");
        assert!(is_failed_install(&app_dir, false));
    }

    #[test]
    fn app_without_current_is_not_failed_under_no_junction() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let version_dir = root.join("apps").join("brave").join("1.96.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join(install_meta::INSTALLED_MANIFEST),
            r#"{"version":"1.96.0"}"#,
        )
        .unwrap();

        // NO_JUNCTION layouts never create `current`, so Scoop skips that check.
        let app_dir = root.join("apps").join("brave");
        assert!(!is_failed_install(&app_dir, true));
    }

    #[test]
    fn missing_app_directory_is_not_failed() {
        let dir = tempdir().unwrap();
        assert!(!is_failed_install(&dir.path().join("nope"), false));
    }

    #[test]
    fn installed_app_dirs_lists_broken_apps_and_excludes_scoop() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_app_with_current(&root, "git", "2.50.0");
        // broken: no `current`
        std::fs::create_dir_all(root.join("apps").join("broken")).unwrap();
        // must be excluded
        std::fs::create_dir_all(root.join("apps").join("scoop").join("current")).unwrap();

        let session = make_session(root.clone());
        let names: Vec<String> = installed_app_dirs(&session)
            .unwrap()
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert_eq!(names, vec!["broken".to_owned(), "git".to_owned()]);

        assert!(is_failed_install(&root.join("apps").join("broken"), false));
        assert!(!is_failed_install(&root.join("apps").join("git"), false));
    }

    #[test]
    fn query_installed_excludes_scoop_itself() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_installed_app(
            &root,
            "scoop",
            "1.0.0",
            install_meta::INSTALLED_MANIFEST,
            install_meta::INSTALL_RECORD,
        );
        write_installed_app(
            &root,
            "git",
            "2.50.0",
            install_meta::INSTALLED_MANIFEST,
            install_meta::INSTALL_RECORD,
        );

        let session = make_session(root);
        let mut names: Vec<String> = query_installed(&session)
            .unwrap()
            .iter()
            .map(|p| p.name().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["git".to_owned()]);
    }

    #[test]
    fn query_installed_reads_bucket_field_from_record() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_installed_app(
            &root,
            "pnpm",
            "12.9.1",
            install_meta::INSTALLED_MANIFEST,
            install_meta::INSTALL_RECORD,
        );

        let session = make_session(root);
        let installed = query_installed(&session).unwrap();
        match &installed[0].status {
            PackageStatus::Installed(state) => {
                assert_eq!(state.bucket.as_deref(), Some("main"));
                assert_eq!(state.arch, "64bit");
            }
            other => panic!("expected Installed, got {other:?}"),
        }
    }

    #[test]
    fn test_query_synced_matching_substring() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let buckets_dir = root.join("buckets");
        std::fs::create_dir_all(buckets_dir.join("main").join("bucket")).unwrap();

        std::fs::write(
            buckets_dir
                .join("main")
                .join("bucket")
                .join("packagea.json"),
            r#"{"version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            buckets_dir
                .join("main")
                .join("bucket")
                .join("packageb.json"),
            r#"{"version":"2.0.0"}"#,
        )
        .unwrap();

        let session = make_session(root);
        let results =
            query_synced_matching(&session, &["ackagea".to_string()], false, false).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name(), "packagea");
    }

    #[test]
    fn test_query_synced_matching_explicit() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let buckets_dir = root.join("buckets");
        std::fs::create_dir_all(buckets_dir.join("main").join("bucket")).unwrap();

        std::fs::write(
            buckets_dir
                .join("main")
                .join("bucket")
                .join("packagea.json"),
            r#"{"version":"1.0.0"}"#,
        )
        .unwrap();

        let session = make_session(root);
        let results =
            query_synced_matching(&session, &["packagea".to_string()], true, false).unwrap();
        assert_eq!(results.len(), 1);

        let results2 =
            query_synced_matching(&session, &["PackageA".to_string()], true, false).unwrap();
        assert_eq!(results2.len(), 1);

        let results3 =
            query_synced_matching(&session, &["PackageB".to_string()], true, false).unwrap();
        assert_eq!(results3.len(), 0);
    }

    #[test]
    fn test_query_synced_matching_description() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let buckets_dir = root.join("buckets");
        std::fs::create_dir_all(buckets_dir.join("main").join("bucket")).unwrap();

        std::fs::write(
            buckets_dir
                .join("main")
                .join("bucket")
                .join("packagea.json"),
            r#"{"version":"1.0.0","description":"A great package"}"#,
        )
        .unwrap();

        let session = make_session(root);
        let results = query_synced_matching(&session, &["great".to_string()], false, true).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name(), "packagea");
    }

    #[test]
    fn test_query_synced_matching_skips_non_matching_without_description() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let buckets_dir = root.join("buckets");
        std::fs::create_dir_all(buckets_dir.join("main").join("bucket")).unwrap();

        std::fs::write(
            buckets_dir
                .join("main")
                .join("bucket")
                .join("packagea.json"),
            r#"{"version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            buckets_dir
                .join("main")
                .join("bucket")
                .join("packageb.json"),
            r#"{"version":"2.0.0"}"#,
        )
        .unwrap();

        let session = make_session(root);
        let results =
            query_synced_matching(&session, &["packageb".to_string()], false, false).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name(), "packageb");
    }
}
