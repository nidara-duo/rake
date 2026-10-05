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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum StatusInfoFlag {
    Outdated,
    InstallFailed,
    Held,
    ManifestRemoved,
    MissingDependencies,
}

impl StatusInfoFlag {
    pub fn as_str(&self) -> &'static str {
        match self {
            StatusInfoFlag::Outdated => "outdated",
            StatusInfoFlag::InstallFailed => "install_failed",
            StatusInfoFlag::Held => "held",
            StatusInfoFlag::ManifestRemoved => "manifest_removed",
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

pub async fn collect_status(session: &Session, local_only: bool) -> Result<StatusReport> {
    let installed = query::query_installed(session)?;
    let latest_versions = query::latest_versions_for_installed(session, &installed)?;
    let (buckets_outdated, buckets_unknown) = if local_only {
        (false, Vec::new())
    } else {
        collect_bucket_freshness(session).await
    };

    let ignored = ["lessmsi", "innounp", "7zip", "dark", "scoop"];
    let installed_names: std::collections::HashSet<String> = installed
        .iter()
        .map(|p| p.name().to_ascii_lowercase())
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

        let latest_version = latest_versions
            .get(&name_lower)
            .filter(|v| {
                rake_domain::version::compare_versions(v, &state.version)
                    == std::cmp::Ordering::Greater
            })
            .cloned();

        if latest_version.is_some() {
            flags.push(StatusInfoFlag::Outdated);
        }

        if state.held {
            flags.push(StatusInfoFlag::Held);
        }

        let mut missing_deps = Vec::new();
        if let Some(ref depends) = pkg.manifest.depends {
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

    Ok(StatusReport {
        entries,
        buckets_outdated,
        buckets_unknown,
    })
}
