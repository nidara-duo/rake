use rake_domain::manifest::Manifest;
use rake_domain::package::PackageStatus;
use serde::Serialize;

use crate::Result;
use crate::bucket::Bucket;
use crate::operations::query;
use crate::session::Session;

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub entries: Vec<StatusEntry>,
    /// True when at least one bucket is behind its remote.
    pub buckets_outdated: bool,
    /// Buckets whose freshness could not be determined (no remote, network
    /// failure, detached HEAD). Kept separate from `buckets_outdated` so a
    /// failed check is never reported as "everything is fine".
    pub buckets_unknown: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusEntry {
    pub name: String,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub missing_dependencies: Vec<String>,
    pub flags: Vec<StatusInfoFlag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum StatusInfoFlag {
    Outdated,
    InstallFailed,
    Held,
    /// The manifest is gone from every bucket — the app is orphaned.
    ManifestRemoved,
    /// The manifest moved into `<bucket>/deprecated/`.
    Deprecated,
    MissingDependencies,
}

impl StatusInfoFlag {
    pub fn as_str(&self) -> &'static str {
        match self {
            StatusInfoFlag::Outdated => "outdated",
            StatusInfoFlag::InstallFailed => "install_failed",
            StatusInfoFlag::Held => "held",
            StatusInfoFlag::ManifestRemoved => "manifest_removed",
            StatusInfoFlag::Deprecated => "deprecated",
            StatusInfoFlag::MissingDependencies => "missing_deps",
        }
    }
}

/// How far behind its remote a bucket is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketFreshness {
    /// Local HEAD matches what the remote advertises.
    UpToDate,
    /// The remote has commits the local checkout does not have.
    Outdated,
    /// Could not be determined.
    Unknown,
}

pub async fn check_bucket_freshness(session: &Session, bucket: &Bucket) -> BucketFreshness {
    let path = bucket.path();

    if !path.join(".git").exists() {
        return BucketFreshness::Unknown;
    }

    let Ok(repo) = git2::Repository::open(path) else {
        return BucketFreshness::Unknown;
    };
    let Ok(head) = repo.head() else {
        return BucketFreshness::Unknown;
    };
    let Some(head_id) = head.target() else {
        return BucketFreshness::Unknown;
    };
    let Some(branch) = head.shorthand().map(str::to_owned) else {
        return BucketFreshness::Unknown;
    };

    // The previous implementation compared HEAD against the local
    // `refs/remotes/origin/<branch>`, which only changes on fetch — so it
    // could never notice new upstream commits and reported every bucket as
    // fine forever. `ls-remote` asks the remote directly.
    match session.git_service().remote_head_sha(path, &branch).await {
        Ok(Some(remote_sha)) => {
            let Ok(remote_id) = git2::Oid::from_str(&remote_sha) else {
                return BucketFreshness::Unknown;
            };
            if remote_id == head_id {
                BucketFreshness::UpToDate
            } else {
                BucketFreshness::Outdated
            }
        }
        Ok(None) => BucketFreshness::Unknown,
        Err(e) => {
            tracing::debug!("bucket {}: ls-remote failed: {e}", bucket.name());
            BucketFreshness::Unknown
        }
    }
}

/// Aggregate bucket freshness across every configured bucket.
///
/// `local_only` skips all network access and reports `Unknown`, which callers
/// must present as "not checked" rather than "up to date".
pub async fn collect_bucket_freshness(session: &Session) -> (bool, Vec<String>) {
    let buckets = crate::operations::bucket::bucket_list(session).unwrap_or_default();

    let results: Vec<(String, BucketFreshness)> =
        futures_util::future::join_all(buckets.iter().map(|b| async move {
            (
                b.name().to_owned(),
                check_bucket_freshness(session, b).await,
            )
        }))
        .await;

    let mut outdated = false;
    let mut unknown = Vec::new();
    for (name, freshness) in results {
        match freshness {
            BucketFreshness::Outdated => outdated = true,
            BucketFreshness::Unknown => unknown.push(name),
            BucketFreshness::UpToDate => {}
        }
    }

    (outdated, unknown)
}

/// Where an installed app's manifest can still be found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestSource {
    /// Present in a bucket's `bucket/` directory.
    Active,
    /// Moved to `<bucket>/deprecated/`.
    Deprecated,
    /// Not in any bucket and not reachable by URL — the app is orphaned.
    Missing,
}

/// Locate an app's bucket manifest the way `app_status` does.
///
/// `manifest()` in etalon lib/manifest.ps1:115-121 looks only at the recorded
/// bucket, then at the URL. Rake searches the remaining buckets as well,
/// matching `latest_versions_for_installed`, which backs the `Available`
/// column — a user who moved an app between buckets should see a version
/// comparison rather than being told the app is orphaned. Reporting
/// `manifest_removed` while an `Available` version is shown would be
/// self-contradictory anyway.
///
/// Returns the manifest alongside its source because `app_status` reads
/// `depends` from here, not from the copy frozen in the version directory.
fn resolve_bucket_manifest(
    buckets: &[Bucket],
    recorded_bucket: Option<&str>,
    name: &str,
) -> (ManifestSource, Option<Manifest>) {
    let recorded =
        recorded_bucket.and_then(|recorded| buckets.iter().find(|b| b.name() == recorded));

    // The recorded bucket wins, exactly as in `manifest()`.
    if let Some(bucket) = recorded
        && let Some(manifest) = bucket.load_manifest(name)
    {
        return (ManifestSource::Active, Some(manifest));
    }

    // Then any other bucket, so a bucket rename does not orphan the app.
    for bucket in buckets.iter().filter(|b| Some(b.name()) != recorded_bucket) {
        if let Some(manifest) = bucket.load_manifest(name) {
            return (ManifestSource::Active, Some(manifest));
        }
    }

    // Not active anywhere. A deprecated manifest still means the app is not
    // orphaned, so it is reported as deprecated rather than removed.
    for bucket in buckets {
        if let Some(manifest) = bucket.load_deprecated_manifest(name) {
            return (ManifestSource::Deprecated, Some(manifest));
        }
    }

    (ManifestSource::Missing, None)
}

pub async fn collect_status(session: &Session, local_only: bool) -> Result<StatusReport> {
    let installed = query::query_installed(session)?;
    let latest_versions = query::latest_versions_for_installed(session, &installed)?;
    let (buckets_outdated, buckets_unknown) = if local_only {
        (false, Vec::new())
    } else {
        collect_bucket_freshness(session).await
    };

    let buckets = crate::operations::bucket::bucket_list(session)?;
    let no_junction = session.config().no_junction.unwrap_or(false);

    let ignored = ["lessmsi", "innounp", "7zip", "dark", "scoop"];
    let installed_names: std::collections::HashSet<String> = installed
        .iter()
        .map(|p| p.name().to_ascii_lowercase())
        .collect();

    // Failed installs never reach `query_installed`, so they are collected from
    // the directory listing that `installed_apps` walks in etalon.
    let failed_names: std::collections::HashSet<String> = query::installed_app_dirs(session)?
        .iter()
        .filter(|dir| query::is_failed_install(dir, no_junction))
        .filter_map(|dir| dir.file_name().map(|n| n.to_string_lossy().to_string()))
        .map(|n| n.to_ascii_lowercase())
        .collect();

    let mut entries = Vec::new();

    for pkg in &installed {
        let name = pkg.name().to_owned();
        let name_lower = name.to_ascii_lowercase();

        let state = match &pkg.status {
            PackageStatus::Installed(s) => s,
            _ => continue,
        };

        let mut flags = Vec::new();

        // A failed install is not installed, so `version` is meaningless and no
        // version comparison makes sense.
        let (manifest_source, bucket_manifest) =
            resolve_bucket_manifest(&buckets, state.bucket.as_deref(), &name);

        let latest_version = if manifest_source == ManifestSource::Missing {
            None
        } else {
            latest_versions
                .get(&name_lower)
                .filter(|v| {
                    rake_domain::version::compare_versions(v, &state.version)
                        == std::cmp::Ordering::Greater
                })
                .cloned()
        };

        if latest_version.is_some() {
            flags.push(StatusInfoFlag::Outdated);
        }

        if state.held {
            flags.push(StatusInfoFlag::Held);
        }

        if manifest_source == ManifestSource::Missing {
            flags.push(StatusInfoFlag::ManifestRemoved);
        }

        if manifest_source == ManifestSource::Deprecated {
            flags.push(StatusInfoFlag::Deprecated);
        }

        // `app_status` reads depends from the bucket manifest (the one that
        // describes what a fresh install would pull in), not from the copy
        // frozen in the version directory. Without a bucket manifest there are
        // no dependencies to check, so an orphaned app is not also reported as
        // having missing deps.
        let mut missing_deps = Vec::new();
        if let Some(depends) = bucket_manifest.as_ref().and_then(|m| m.depends.as_ref()) {
            for dep in depends.iter() {
                let dep_lower = dep.to_ascii_lowercase();
                if ignored.contains(&dep_lower.as_str()) {
                    continue;
                }
                if !installed_names.contains(&dep_lower) {
                    missing_deps.push(dep.clone());
                }
            }
        }
        if !missing_deps.is_empty() {
            flags.push(StatusInfoFlag::MissingDependencies);
        }

        if flags.is_empty() {
            continue;
        }

        entries.push(StatusEntry {
            name,
            installed_version: Some(state.version.clone()),
            latest_version,
            missing_dependencies: missing_deps,
            flags,
        });
    }

    // A failed install has no readable version, so it cannot also be reported
    // as outdated. If a healthy entry slipped in for the same app (readable
    // metadata but a broken junction), the failure wins.
    let mut failed_reported = std::collections::HashSet::new();
    for name in &failed_names {
        if failed_reported.insert(name.clone()) {
            entries.push(StatusEntry {
                name: name.clone(),
                installed_version: None,
                latest_version: None,
                missing_dependencies: Vec::new(),
                flags: vec![StatusInfoFlag::InstallFailed],
            });
        }
    }
    entries.retain(|e| {
        let lower = e.name.to_ascii_lowercase();
        !failed_names.contains(&lower) || e.flags.contains(&StatusInfoFlag::InstallFailed)
    });

    entries.sort_by_key(|e| e.name.to_ascii_lowercase());

    Ok(StatusReport {
        entries,
        buckets_outdated,
        buckets_unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::config::Config;
    use tempfile::tempdir;

    fn make_session(root: std::path::PathBuf) -> Session {
        Session::from_config(Config {
            root_path: Some(root),
            ..Default::default()
        })
    }

    fn write(root: &std::path::Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Installed app with metadata in `current` and a bucket manifest.
    fn install_app(root: &std::path::Path, bucket: &str, name: &str, version: &str) {
        std::fs::create_dir_all(root.join("apps").join(name).join(version)).unwrap();
        write(
            root,
            &format!("buckets/{bucket}/bucket/{name}.json"),
            &format!(r#"{{"version":"{version}"}}"#),
        );
        write(
            root,
            &format!("apps/{name}/current/scoop-manifest.json"),
            &format!(r#"{{"version":"{version}"}}"#),
        );
        write(
            root,
            &format!("apps/{name}/current/scoop-install.json"),
            &format!(r#"{{"version":"{version}","bucket":"{bucket}"}}"#),
        );
    }

    /// `local_only` skips the network, so the bucket checks are inert here.
    async fn status_of(root: std::path::PathBuf) -> StatusReport {
        let session = make_session(root);
        collect_status(&session, true).await.unwrap()
    }

    fn flags_of<'a>(report: &'a StatusReport, name: &str) -> &'a [StatusInfoFlag] {
        report
            .entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.flags.as_slice())
            .unwrap_or_else(|| panic!("no entry for {name}"))
    }

    #[tokio::test]
    async fn healthy_app_is_absent_from_report() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        install_app(&root, "main", "git", "2.50.0");

        assert!(status_of(root).await.entries.is_empty());
    }

    #[tokio::test]
    async fn manifest_removed_is_reported() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // bucket/ exists but holds no manifest for the app: orphaned.
        install_app(&root, "main", "ghost", "1.0.0");
        std::fs::remove_dir_all(root.join("buckets/main/bucket")).unwrap();
        std::fs::create_dir_all(root.join("buckets/main/bucket")).unwrap();

        let report = status_of(root).await;
        assert!(flags_of(&report, "ghost").contains(&StatusInfoFlag::ManifestRemoved));
    }

    #[tokio::test]
    async fn manifest_removed_when_bucket_dir_gone_entirely() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        install_app(&root, "main", "ghost", "1.0.0");
        std::fs::remove_dir_all(root.join("buckets")).unwrap();

        let report = status_of(root).await;
        assert!(flags_of(&report, "ghost").contains(&StatusInfoFlag::ManifestRemoved));
    }

    #[tokio::test]
    async fn manifest_found_in_another_bucket_is_not_removed() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // Recorded bucket `main` no longer has it, but `extras` does — and has a
        // newer version, so the app is outdated but definitely not orphaned.
        install_app(&root, "main", "moved", "1.0.0");
        std::fs::remove_file(root.join("buckets/main/bucket/moved.json")).unwrap();
        write(
            &root,
            "buckets/extras/bucket/moved.json",
            r#"{"version":"2.0.0"}"#,
        );

        let report = status_of(root).await;
        let flags = flags_of(&report, "moved");
        assert!(!flags.contains(&StatusInfoFlag::ManifestRemoved));
        assert!(flags.contains(&StatusInfoFlag::Outdated));
    }

    #[tokio::test]
    async fn manifest_removed_suppresses_outdated() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        install_app(&root, "main", "ghost", "1.0.0");
        // Drop the manifest but leave the app installed: there is no version to
        // compare against, so it must not be called outdated.
        std::fs::remove_file(root.join("buckets/main/bucket/ghost.json")).unwrap();

        let report = status_of(root).await;
        let entry = report.entries.iter().find(|e| e.name == "ghost").unwrap();
        assert_eq!(entry.latest_version, None, "no version to compare against");
        assert!(entry.flags.contains(&StatusInfoFlag::ManifestRemoved));
        assert!(!entry.flags.contains(&StatusInfoFlag::Outdated));
    }

    #[tokio::test]
    async fn deprecated_is_reported_when_manifest_moved() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        install_app(&root, "main", "oldapp", "1.0.0");
        // Move the manifest into deprecated/ instead of leaving it active.
        std::fs::rename(
            root.join("buckets/main/bucket/oldapp.json"),
            root.join("buckets/main/bucket/oldapp.json.tmp"),
        )
        .unwrap();
        write(
            &root,
            "buckets/main/deprecated/oldapp.json",
            r#"{"version":"1.0.0"}"#,
        );
        std::fs::remove_file(root.join("buckets/main/bucket/oldapp.json.tmp")).unwrap();

        let report = status_of(root).await;
        assert!(flags_of(&report, "oldapp").contains(&StatusInfoFlag::Deprecated));
        assert!(!flags_of(&report, "oldapp").contains(&StatusInfoFlag::ManifestRemoved));
    }

    #[tokio::test]
    async fn outdated_is_reported_when_bucket_has_newer() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        install_app(&root, "main", "vscode", "1.139.0");
        write(
            &root,
            "buckets/main/bucket/vscode.json",
            r#"{"version":"1.140.0"}"#,
        );

        let report = status_of(root).await;
        let entry = report.entries.iter().find(|e| e.name == "vscode").unwrap();
        assert_eq!(entry.latest_version.as_deref(), Some("1.140.0"));
        assert!(entry.flags.contains(&StatusInfoFlag::Outdated));
    }

    #[tokio::test]
    async fn failed_install_without_current_is_reported() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        install_app(&root, "main", "git", "2.50.0");
        // A second app whose version dir exists but has no `current`.
        std::fs::create_dir_all(root.join("apps/broken/1.0.0")).unwrap();

        let report = status_of(root).await;
        let entry = report
            .entries
            .iter()
            .find(|e| e.name == "broken")
            .expect("failed install must be reported");
        assert_eq!(entry.flags, vec![StatusInfoFlag::InstallFailed]);
        assert_eq!(entry.installed_version, None);
        assert!(!report.entries.iter().any(|e| e.name == "git"));
    }

    #[tokio::test]
    async fn failure_takes_precedence_over_other_flags() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // A version dir with no `current` junction: `query_installed` still
        // surfaces it via the version-dir fallback and would call it outdated,
        // but Scoop counts it as failed. The failure must win.
        std::fs::create_dir_all(root.join("apps/weird/1.0.0")).unwrap();
        write(
            &root,
            "buckets/main/bucket/weird.json",
            r#"{"version":"2.0.0"}"#,
        );

        let report = status_of(root).await;
        let entry = report
            .entries
            .iter()
            .find(|e| e.name == "weird")
            .expect("must be reported");
        assert_eq!(entry.flags, vec![StatusInfoFlag::InstallFailed]);
    }

    #[tokio::test]
    async fn held_is_reported() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        install_app(&root, "main", "pinned", "1.0.0");
        write(
            &root,
            "apps/pinned/current/scoop-install.json",
            r#"{"version":"1.0.0","bucket":"main","hold":true}"#,
        );

        assert!(flags_of(&status_of(root).await, "pinned").contains(&StatusInfoFlag::Held));
    }

    #[tokio::test]
    async fn missing_dependency_is_reported() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        install_app(&root, "main", "needs", "1.0.0");
        write(
            &root,
            "buckets/main/bucket/needs.json",
            r#"{"version":"1.0.0","depends":"absent"}"#,
        );

        let report = status_of(root).await;
        let entry = report.entries.iter().find(|e| e.name == "needs").unwrap();
        assert_eq!(entry.missing_dependencies, vec!["absent".to_owned()]);
        assert!(entry.flags.contains(&StatusInfoFlag::MissingDependencies));
    }

    #[tokio::test]
    async fn scoop_itself_is_never_reported() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(
            &root,
            "apps/scoop/current/scoop-install.json",
            r#"{"version":"1.0.0","bucket":"main"}"#,
        );

        assert!(status_of(root).await.entries.is_empty());
    }

    #[tokio::test]
    async fn entries_are_sorted_case_insensitively() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for name in ["zebra", "Apple", "mango"] {
            std::fs::create_dir_all(root.join(format!("apps/{name}/current"))).unwrap();
            write(
                &root,
                &format!("apps/{name}/current/scoop-install.json"),
                r#"{"version":"1.0.0","bucket":"main"}"#,
            );
            std::fs::create_dir_all(root.join("buckets/main/bucket")).unwrap();
        }

        let report = status_of(root).await;
        let names: Vec<String> = report
            .entries
            .iter()
            .map(|e| e.name.to_ascii_lowercase())
            .collect();
        assert_eq!(names, vec!["apple", "mango", "zebra"]);
    }
}
