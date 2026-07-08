use std::path::Path;

use rake_domain::package::PackageStatus;
use rayon::prelude::*;
use serde::Serialize;

use crate::Result;
use crate::operations::query;
use crate::session::Session;

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub entries: Vec<StatusEntry>,
    pub buckets_outdated: bool,
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

fn check_bucket_outdated(repo_path: &Path) -> bool {
    if !repo_path.join(".git").exists() {
        return false;
    }
    let Ok(repo) = git2::Repository::open(repo_path) else {
        return false;
    };
    let Ok(head) = repo.head() else {
        return false;
    };
    let Some(head_id) = head.target() else {
        return false;
    };
    let branch = head.shorthand().unwrap_or("main");
    let remote_ref_name = format!("refs/remotes/origin/{branch}");
    match repo.find_reference(&remote_ref_name) {
        Ok(remote_ref) => remote_ref.target().is_some_and(|id| id != head_id),
        Err(_) => false,
    }
}

pub fn collect_bucket_freshness(session: &Session) -> Result<bool> {
    let buckets = crate::operations::bucket::bucket_list(session)?;
    Ok(buckets.par_iter().any(|b| check_bucket_outdated(b.path())))
}

pub fn collect_status(session: &Session, local_only: bool) -> Result<StatusReport> {
    let installed = query::query_installed(session)?;
    let latest_versions = query::latest_versions_for_installed(session, &installed)?;
    let buckets_outdated = if local_only {
        false
    } else {
        collect_bucket_freshness(session)?
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
    })
}
