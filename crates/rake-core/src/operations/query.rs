use rake_domain::package::InstallRecord;
use std::collections::HashMap;

use rake_domain::manifest::Manifest;
use rake_domain::package::{InstallState, Package, PackageIdent, PackageSource, PackageStatus};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::Result;
use crate::session::Session;

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
        .collect();

    let results: Vec<Package> = entries
        .par_iter()
        .filter_map(|entry| {
            let app_dir = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let current_dir = app_dir.join("current");

            let install_info: Option<InstallRecord> =
                std::fs::read_to_string(current_dir.join("install.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok());

            let manifest: Option<Manifest> =
                std::fs::read_to_string(current_dir.join("manifest.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok());

            let manifest = match manifest {
                Some(m) => m,
                None => return None,
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
                    if let Ok(content) = std::fs::read_to_string(manifest_path)
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
