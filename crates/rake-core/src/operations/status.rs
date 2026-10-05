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
    /// Label shown in the `Info` column.
    ///
    /// The wording is lifted verbatim from `scoop status`
    /// (ethalon libexec/scoop-status.ps1:69-74) so that the two commands read
    /// identically. `Outdated` and `MissingDependencies` have no counterpart
    /// there: Scoop signals outdated via the populated `Latest Version` column
    /// and missing deps via its own column, so neither belongs in `Info`.
    pub fn as_str(&self) -> &'static str {
        match self {
            StatusInfoFlag::InstallFailed => "Install failed",
            StatusInfoFlag::Held => "Held package",
            StatusInfoFlag::ManifestRemoved => "Manifest removed",
            StatusInfoFlag::Deprecated => "Deprecated",
            StatusInfoFlag::Outdated | StatusInfoFlag::MissingDependencies => "",
        }
    }

    /// Whether this flag contributes a label to the `Info` column.
    pub fn shown_in_info(&self) -> bool {
        !matches!(
            self,
            StatusInfoFlag::Outdated | StatusInfoFlag::MissingDependencies
        )
    }
}

/// How far behind its remote a bucket is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketFreshness {
    /// Local HEAD matches the last known upstream state.
    UpToDate,
    /// The last known upstream state has commits the local checkout lacks.
    Outdated,
    /// Could not be determined (no repository, no remote ref, detached HEAD).
    Unknown,
}

/// Whether to touch the network while checking buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckBuckets {
    /// No network: compare HEAD against `refs/remotes/origin/<branch>` as it
    /// was left by the last fetch. Instant, but only as fresh as that fetch.
    Local,
    /// Fetch first, then compare. Costs a round trip per bucket, but leaves the
    /// remote-tracking refs updated, which is exactly what a later `rake update`
    /// needs — so the work is not repeated.
    Fetch,
}

/// Compare a bucket's HEAD with its recorded upstream state.
///
/// Only meaningful after a fetch: `refs/remotes/origin/<branch>` is written by
/// fetch and by nothing else, so reading it without fetching answers "is the
/// bucket behind what we last knew", never "is it behind upstream".
fn compare_with_remote_ref(repo: &git2::Repository, path: &std::path::Path) -> BucketFreshness {
    let Ok(head) = repo.head() else {
        return BucketFreshness::Unknown;
    };
    let Some(head_id) = head.target() else {
        return BucketFreshness::Unknown;
    };
    let Some(branch) = head.shorthand().map(str::to_owned) else {
        return BucketFreshness::Unknown;
    };

    let remote_ref = format!("refs/remotes/origin/{branch}");
    match repo.find_reference(&remote_ref) {
        Ok(reference) => {
            if reference.target().is_some_and(|id| id == head_id) {
                BucketFreshness::UpToDate
            } else {
                BucketFreshness::Outdated
            }
        }
        // A bucket cloned without remote-tracking refs, or one whose branch has
        // never been fetched, cannot be judged at all.
        Err(e) => {
            tracing::debug!("{}: no {}: {e}", path.display(), remote_ref);
            BucketFreshness::Unknown
        }
    }
}

pub async fn check_bucket_freshness(
    session: &Session,
    bucket: &Bucket,
    mode: CheckBuckets,
) -> BucketFreshness {
    let path = bucket.path();

    if !path.join(".git").exists() {
        return BucketFreshness::Unknown;
    }

    // A failed fetch invalidates the comparison: the remote-tracking ref still
    // holds whatever the previous fetch left behind, and reporting that as a
    // verdict would pass a stale answer off as a current one.
    if mode == CheckBuckets::Fetch && session.git_service().fetch(path).await.is_err() {
        tracing::debug!("bucket {}: fetch failed", bucket.name());
        return BucketFreshness::Unknown;
    }

    let Ok(repo) = git2::Repository::open(path) else {
        return BucketFreshness::Unknown;
    };

    compare_with_remote_ref(&repo, path)
}

/// Aggregate bucket freshness across every configured bucket.
///
/// Buckets are checked concurrently: the network round trip dominates and there
/// is no dependency between buckets, so serialising them would only add up the
/// latencies.
pub async fn collect_bucket_freshness(
    session: &Session,
    mode: CheckBuckets,
) -> (bool, Vec<String>) {
    let buckets = crate::operations::bucket::bucket_list(session).unwrap_or_default();

    let results: Vec<(String, BucketFreshness)> =
        futures_util::future::join_all(buckets.iter().map(|b| async move {
            (
                b.name().to_owned(),
                check_bucket_freshness(session, b, mode).await,
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

pub async fn collect_status(session: &Session, mode: CheckBuckets) -> Result<StatusReport> {
    let installed = query::query_installed(session)?;
    let latest_versions = query::latest_versions_for_installed(session, &installed)?;
    let (buckets_outdated, buckets_unknown) = collect_bucket_freshness(session, mode).await;

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
        collect_status(&session, CheckBuckets::Local).await.unwrap()
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

    #[test]
    fn info_labels_match_scoop_wording() {
        // etalon libexec/scoop-status.ps1:70-73
        assert_eq!(StatusInfoFlag::InstallFailed.as_str(), "Install failed");
        assert_eq!(StatusInfoFlag::Held.as_str(), "Held package");
        assert_eq!(StatusInfoFlag::Deprecated.as_str(), "Deprecated");
        assert_eq!(StatusInfoFlag::ManifestRemoved.as_str(), "Manifest removed");
    }

    #[test]
    fn outdated_and_missing_deps_stay_out_of_info() {
        // Scoop signals these through their own columns, never in Info.
        assert!(!StatusInfoFlag::Outdated.shown_in_info());
        assert!(!StatusInfoFlag::MissingDependencies.shown_in_info());
        assert!(StatusInfoFlag::InstallFailed.shown_in_info());
        assert!(StatusInfoFlag::Held.shown_in_info());
        assert!(StatusInfoFlag::Deprecated.shown_in_info());
        assert!(StatusInfoFlag::ManifestRemoved.shown_in_info());
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

    /// Build a bare git repo at `<root>/buckets/<name>` on branch `master` with
    /// one commit, plus a second commit upstream that the local checkout lacks.
    fn init_bucket(root: &std::path::Path, name: &str, extra_upstream_commits: usize) {
        let path = root.join("buckets").join(name);
        std::fs::create_dir_all(&path).unwrap();

        let git = |args: &[&str], cwd: &std::path::Path| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} in {} failed: {}",
                cwd.display(),
                String::from_utf8_lossy(&out.stderr)
            );
        };

        git(&["init", "-q", "-b", "master"], &path);
        git(&["config", "user.email", "t@example.com"], &path);
        git(&["config", "user.name", "test"], &path);
        std::fs::write(path.join("f.txt"), b"v1").unwrap();
        git(&["add", "-A"], &path);
        git(&["commit", "-q", "-m", "first"], &path);

        let _ = extra_upstream_commits;
    }

    /// Simulate an upstream that has moved on: make a commit, then rewind the
    /// local branch and point `refs/remotes/origin/master` at the newer commit.
    ///
    /// Rewinding matters — committing alone would also move HEAD, leaving the
    /// bucket genuinely up to date and the test meaningless.
    fn advance_remote_ref(root: &std::path::Path, name: &str) -> git2::Oid {
        let path = root.join("buckets").join(name);
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&path)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        let rev_parse = |refname: &str| {
            let out = std::process::Command::new("git")
                .args(["rev-parse", refname])
                .current_dir(&path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };

        let before = rev_parse("HEAD");

        std::fs::write(path.join("f.txt"), b"v2").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "second"]);
        let ahead = rev_parse("HEAD");

        git(&["reset", "-q", "--hard", &before]);
        git(&["update-ref", "refs/remotes/origin/master", &ahead]);

        git2::Oid::from_str(&ahead).unwrap()
    }

    #[tokio::test]
    async fn local_mode_reports_nothing_when_no_remote_ref_exists() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        init_bucket(&root, "main", 0);

        let session = make_session(root);
        let (outdated, unknown) = collect_bucket_freshness(&session, CheckBuckets::Local).await;
        assert!(!outdated);
        assert_eq!(unknown, vec!["main".to_owned()], "never fetched");
    }

    #[tokio::test]
    async fn local_mode_detects_outdated_via_remote_ref() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        init_bucket(&root, "main", 0);
        advance_remote_ref(&root, "main");

        let session = make_session(root);
        let (outdated, unknown) = collect_bucket_freshness(&session, CheckBuckets::Local).await;
        assert!(outdated, "local HEAD is behind the fetched remote ref");
        assert!(unknown.is_empty());
    }

    #[tokio::test]
    async fn local_mode_reports_up_to_date_when_refs_match() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        init_bucket(&root, "main", 0);
        let sha = {
            let path = root.join("buckets/main");
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let path = root.join("buckets/main");
        std::process::Command::new("git")
            .args(["update-ref", "refs/remotes/origin/master", &sha])
            .current_dir(&path)
            .output()
            .unwrap();

        let session = make_session(root);
        let (outdated, unknown) = collect_bucket_freshness(&session, CheckBuckets::Local).await;
        assert!(!outdated);
        assert!(unknown.is_empty());
    }

    #[tokio::test]
    async fn local_mode_never_fetches() {
        // The offline guarantee: with an unreachable remote, Local must still
        // reach a verdict rather than reporting Unknown.
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        init_bucket(&root, "main", 0);
        advance_remote_ref(&root, "main");

        let path = root.join("buckets/main");
        std::process::Command::new("git")
            .args([
                "remote",
                "set-url",
                "origin",
                "https://invalid.invalid/nope.git",
            ])
            .current_dir(&path)
            .output()
            .unwrap();

        let session = make_session(root);
        let (outdated, unknown) = collect_bucket_freshness(&session, CheckBuckets::Local).await;
        assert!(outdated, "answered from local refs");
        assert!(unknown.is_empty(), "no network attempt, no failure");
    }

    #[tokio::test]
    async fn fetch_mode_marks_bucket_unknown_when_fetch_fails() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        init_bucket(&root, "main", 0);
        advance_remote_ref(&root, "main");

        let path = root.join("buckets/main");
        std::process::Command::new("git")
            .args([
                "remote",
                "set-url",
                "origin",
                "https://invalid.invalid/nope.git",
            ])
            .current_dir(&path)
            .output()
            .unwrap();

        let session = make_session(root);
        let (outdated, unknown) = collect_bucket_freshness(&session, CheckBuckets::Fetch).await;
        assert!(!outdated, "the failed fetch left the old ref in place");
        assert!(!unknown.is_empty(), "a failed fetch must be visible");
    }
}
