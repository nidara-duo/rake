use std::io::Read;
use std::path::{Path, PathBuf};

use futures_util::future::join_all;
use md5::Md5;
use rake_domain::arch::Arch;
use rake_domain::package::{Package, PackageIdent};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::Result;
use crate::event::Event;
use crate::session::Session;

#[derive(Debug)]
pub struct DownloadSize {
    pub total: u64,
    pub estimated: bool,
}

#[derive(Debug)]
pub struct DownloadedFile {
    pub ident: PackageIdent,
    pub url: String,
    pub cache_path: PathBuf,
}

pub async fn download_packages(
    session: &Session,
    packages: &[Package],
    reuse_cache: bool,
    arch: Arch,
) -> Result<Vec<DownloadedFile>> {
    let cache_root = session
        .config()
        .cache_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("cache"));
    let progress_tx = session.event_bus().core_sender();

    let mut results = Vec::new();

    for pkg in packages {
        let _ = progress_tx.try_send(Event::DownloadStart(pkg.ident.clone()));

        let urls = resolve_urls(pkg, arch);
        if urls.is_empty() {
            let msg = format!("package {} has no downloadable URLs", pkg.name());
            let _ = progress_tx.try_send(Event::DownloadError(pkg.ident.clone(), msg));
            continue;
        }

        let hashes = pkg.manifest.resolve_hashes(arch);

        let mut ok = false;
        for (url_idx, url) in urls.into_iter().enumerate() {
            let cache_path = if reuse_cache {
                match find_cached_file(&cache_root, pkg, &url) {
                    Some(path) => path,
                    None => cache_root.join(cache_filename(pkg, &url)),
                }
            } else {
                cache_root.join(cache_filename(pkg, &url))
            };

            let cache_hit = reuse_cache
                && cache_path.exists()
                && cache_path.metadata().map(|m| m.len() > 0).unwrap_or(false);

            if cache_hit {
                let cache_ok = match hashes.as_ref().and_then(|h| h.get(url_idx)) {
                    Some(expected_hash) => verify_file_hash(&cache_path, expected_hash).is_ok(),
                    None => true,
                };

                if cache_ok {
                    let _ = progress_tx.try_send(Event::DownloadCached(pkg.ident.clone()));
                    results.push(DownloadedFile {
                        ident: pkg.ident.clone(),
                        url,
                        cache_path,
                    });
                    ok = true;
                    break;
                } else {
                    let _ = std::fs::remove_file(&cache_path);
                }
            }

            if let Err(e) = crate::infra::fs::ensure_dir(&cache_root) {
                let _ =
                    progress_tx.try_send(Event::DownloadError(pkg.ident.clone(), e.to_string()));
                continue;
            }

            let tmp_path = cache_root.join(format!("{}.download", cache_filename(pkg, &url)));
            let ident = pkg.ident.clone();

            match session
                .http_client()
                .download(&url, &tmp_path, ident.clone(), Some(progress_tx.clone()))
                .await
            {
                Ok(_) => {
                    if let Some(ref hash_list) = hashes
                        && let Some(expected_hash) = hash_list.get(url_idx)
                        && let Err(e) = verify_file_hash(&tmp_path, expected_hash)
                    {
                        let _ = std::fs::remove_file(&tmp_path);
                        let _ = progress_tx
                            .try_send(Event::DownloadError(pkg.ident.clone(), e.to_string()));
                        continue;
                    }

                    if let Err(e) = std::fs::rename(&tmp_path, &cache_path) {
                        let _ = progress_tx
                            .try_send(Event::DownloadError(pkg.ident.clone(), e.to_string()));
                        continue;
                    }

                    results.push(DownloadedFile {
                        ident: pkg.ident.clone(),
                        url,
                        cache_path,
                    });
                    ok = true;
                    break;
                }
                Err(e) => {
                    let _ = tmp_path.exists().then(|| std::fs::remove_file(&tmp_path));
                    let _ = progress_tx
                        .try_send(Event::DownloadError(pkg.ident.clone(), e.to_string()));
                    continue;
                }
            }
        }

        if !ok {
            let msg = format!("all URLs failed for package {}", pkg.name());
            let _ = progress_tx.try_send(Event::DownloadError(pkg.ident.clone(), msg));
        }
    }

    let _ = progress_tx.try_send(Event::DownloadDone);

    Ok(results)
}

pub async fn calculate_total_download_size(
    session: &Session,
    packages: &[Package],
    arch: Arch,
) -> Result<DownloadSize> {
    let futures = packages.iter().map(|pkg| {
        let urls = resolve_urls(pkg, arch);
        let pkg_name = pkg.name().to_string();
        let pkg_version = pkg.version().to_string();
        async move {
            let mut total_for_pkg = 0;
            let mut estimated_for_pkg = false;

            if urls.is_empty() {
                tracing::warn!(
                    "Package {}@{} has no downloadable URLs",
                    pkg_name,
                    pkg_version
                );
            }

            for url in &urls {
                match session.http_client().content_length(url).await {
                    Ok(Some(len)) => {
                        tracing::debug!(
                            "Package {}@{} URL {} size: {} bytes",
                            pkg_name,
                            pkg_version,
                            url,
                            len
                        );
                        total_for_pkg += len;
                    }
                    Ok(None) => {
                        tracing::debug!(
                            "Package {}@{} URL {} size: unknown (server did not provide)",
                            pkg_name,
                            pkg_version,
                            url
                        );
                        estimated_for_pkg = true;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Package {}@{} URL {} error: {}",
                            pkg_name,
                            pkg_version,
                            url,
                            e
                        );
                        estimated_for_pkg = true;
                    }
                }
            }

            if total_for_pkg > 0 {
                tracing::info!(
                    "Package {}@{} total download size: {} bytes",
                    pkg_name,
                    pkg_version,
                    total_for_pkg
                );
            }

            (total_for_pkg, estimated_for_pkg)
        }
    });

    let results = join_all(futures).await;

    let mut total = 0;
    let mut estimated = false;

    for (len, est) in results {
        total += len;
        estimated |= est;
    }

    tracing::info!(
        "Total download size for all packages: {} bytes ({})",
        total,
        if estimated { "estimated" } else { "exact" }
    );

    Ok(DownloadSize { total, estimated })
}

pub fn resolve_urls(pkg: &Package, arch: Arch) -> Vec<String> {
    let manifest = &pkg.manifest;

    if let Some(ref arch_map) = manifest.architecture {
        let spec = match arch {
            Arch::Amd64 => arch_map.amd64.as_ref(),
            Arch::Ia32 => arch_map.ia32.as_ref(),
            Arch::Aarch64 => arch_map.aarch64.as_ref(),
        };

        if let Some(spec) = spec
            && let Some(ref urls) = spec.url
        {
            let v: Vec<String> = urls.clone().into_vec();
            if !v.is_empty() {
                return v;
            }
        }
    }

    manifest
        .url
        .clone()
        .map(|u| u.into_vec())
        .unwrap_or_default()
}

/// Generate cache filename matching Scoop's format: `name#version#hash.EXT`
///
/// Scoop computes: SHA256(url)[..7] as lowercase hex, extension via `Path.GetExtension`
/// which takes everything after the last `.` after the last `/`.
pub fn cache_filename(pkg: &Package, url: &str) -> String {
    let url_bytes = url.as_bytes();
    let hash = hex::encode(Sha256::digest(url_bytes));
    let prefix = &hash[..7];

    let clean = url.split('?').next().unwrap_or(url);
    let clean = clean.split('#').next().unwrap_or(clean);
    let ext = match clean.rfind('.') {
        Some(pos) => &clean[pos..],
        None => "",
    };

    format!("{}#{}#{}{}", pkg.name(), pkg.version(), prefix, ext)
}

/// Find a cached file for a package+URL, falling back to a glob scan of `name#version#*`.
///
/// Scoop and Rake may use slightly different hash prefixes (7 vs 8 chars, different source).
/// This function first tries the exact filename, then scans the cache directory for any match.
pub fn find_cached_file(
    cache_root: &std::path::Path,
    pkg: &Package,
    url: &str,
) -> Option<std::path::PathBuf> {
    let exact = cache_root.join(cache_filename(pkg, url));
    if exact.exists() && exact.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Some(exact);
    }

    let prefix = format!("{}#{}#", pkg.name(), pkg.version());
    if let Ok(entries) = std::fs::read_dir(cache_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && !name.ends_with(".download") {
                let path = entry.path();
                if path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                    return Some(path);
                }
            }
        }
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

/// Split a manifest hash spec into (algorithm, hex_digest).
///
/// Scoop manifests express hashes either as bare hex (algorithm inferred
/// from length: 32=md5, 40=sha1, 64=sha256, 128=sha512) or as
/// `"algo:hexdigest"` (e.g. `"sha512:abcd..."`). Rake previously ALWAYS
/// hashed downloads with SHA256 regardless of what the manifest asked
/// for, which made every package pinned to sha512 (or md5/sha1) fail
/// verification 100% of the time — this was the root cause of
/// "hash mismatch" errors during install/update.
fn parse_hash_spec(spec: &str) -> (HashAlgo, &str) {
    if let Some((prefix, rest)) = spec.split_once(':') {
        let algo = match prefix.to_ascii_lowercase().as_str() {
            "md5" => HashAlgo::Md5,
            "sha1" => HashAlgo::Sha1,
            "sha256" => HashAlgo::Sha256,
            "sha512" => HashAlgo::Sha512,
            _ => HashAlgo::Sha256,
        };
        (algo, rest)
    } else {
        let algo = match spec.len() {
            32 => HashAlgo::Md5,
            40 => HashAlgo::Sha1,
            128 => HashAlgo::Sha512,
            _ => HashAlgo::Sha256,
        };
        (algo, spec)
    }
}

fn hash_file<D: Digest>(file: &mut std::fs::File) -> Result<String> {
    let mut hasher = D::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Verify that a file's hash matches the expected manifest hash spec.
///
/// Supports md5 / sha1 / sha256 / sha512, with or without an `"algo:"`
/// prefix (see [`parse_hash_spec`]). Returns `Ok(())` on match, or an
/// error on mismatch / I/O failure. Hash comparison is case-insensitive.
pub fn verify_file_hash(path: &Path, expected: &str) -> Result<()> {
    let (algo, expected_hex) = parse_hash_spec(expected);

    let mut file = std::fs::File::open(path)?;

    let actual = match algo {
        HashAlgo::Md5 => hash_file::<Md5>(&mut file)?,
        HashAlgo::Sha1 => hash_file::<Sha1>(&mut file)?,
        HashAlgo::Sha256 => hash_file::<Sha256>(&mut file)?,
        HashAlgo::Sha512 => hash_file::<Sha512>(&mut file)?,
    };

    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(crate::Error::Io(std::io::Error::other(format!(
            "hash mismatch: expected {expected}, got {actual}"
        ))))
    }
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    #[test]
    fn infers_sha256_from_bare_64_char_hex() {
        let spec = "a".repeat(64);
        assert_eq!(parse_hash_spec(&spec).0, HashAlgo::Sha256);
    }

    #[test]
    fn infers_sha512_from_bare_128_char_hex() {
        let spec = "a".repeat(128);
        assert_eq!(parse_hash_spec(&spec).0, HashAlgo::Sha512);
    }

    #[test]
    fn infers_md5_from_bare_32_char_hex() {
        let spec = "a".repeat(32);
        assert_eq!(parse_hash_spec(&spec).0, HashAlgo::Md5);
    }

    #[test]
    fn infers_sha1_from_bare_40_char_hex() {
        let spec = "a".repeat(40);
        assert_eq!(parse_hash_spec(&spec).0, HashAlgo::Sha1);
    }

    #[test]
    fn respects_explicit_sha512_prefix() {
        let spec = format!("sha512:{}", "a".repeat(128));
        let (algo, hex) = parse_hash_spec(&spec);
        assert_eq!(algo, HashAlgo::Sha512);
        assert_eq!(hex, "a".repeat(128));
    }

    #[test]
    fn verifies_known_sha256_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        verify_file_hash(&path, expected).unwrap();
    }

    #[test]
    fn verifies_known_sha512_prefixed_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let expected = "sha512:309ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f";
        verify_file_hash(&path, expected).unwrap();
    }

    #[test]
    fn rejects_wrong_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let wrong = "0".repeat(64);
        assert!(verify_file_hash(&path, &wrong).is_err());
    }
}
