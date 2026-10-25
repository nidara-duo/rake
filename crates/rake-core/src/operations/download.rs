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

/// One verified file in the cache, ready for the install step.
///
/// `url` keeps the manifest URL exactly as written, fragment included, because
/// the extractor reads `#/name.ext` to decide the real archive format.
/// `target_filename` is the name the bytes must get once they are copied or
/// extracted into `apps/<app>/<version>` — Scoop's `url_filename`.
/// `verified` says whether a manifest hash was present and matched; a manifest
/// without a hash cannot be checked, and Scoop warns and proceeds in that case.
#[derive(Debug, Clone)]
pub struct DownloadedFile {
    pub ident: PackageIdent,
    pub url: String,
    pub cache_path: PathBuf,
    pub target_filename: String,
    pub verified: bool,
}

/// Download every artifact of one resolved package into the cache and verify
/// each file against the manifest hash.
///
/// This is the only place in the workspace that fetches package artifacts.
/// `download`, `install` and `update` all call it (directly or through
/// [`download_packages`]); none of them is allowed to open an HTTP connection
/// of its own.
///
/// `reuse_cache == false` forces a fresh download even when a cache file
/// exists. A cached file that fails hash verification is deleted and treated
/// as a miss, so a corrupted cache entry can never be installed.
pub async fn download_package(
    session: &Session,
    pkg: &Package,
    reuse_cache: bool,
    arch: Arch,
) -> Result<Vec<DownloadedFile>> {
    let cache_root = cache_root(session);
    let progress_tx = session.event_bus().core_sender();

    let _ = progress_tx.try_send(Event::DownloadStart(pkg.ident.clone()));

    let urls = resolve_urls(pkg, arch);
    if urls.is_empty() {
        let msg = format!("package {} has no downloadable URLs", pkg.name());
        let _ = progress_tx.try_send(Event::DownloadError(pkg.ident.clone(), msg.clone()));
        return Err(download_error(msg));
    }

    let hashes = pkg.manifest.resolve_hashes(arch);
    let mut results = Vec::new();
    let mut failed = 0usize;

    for (url_idx, url) in urls.iter().enumerate() {
        let target_name = target_filename(url);
        let expected_hash = hashes.as_ref().and_then(|h| h.get(url_idx));

        if reuse_cache && let Some(cache_path) = find_cached_file(&cache_root, pkg, url) {
            match expected_hash {
                Some(expected) => match verify_file_hash(&cache_path, expected) {
                    Ok(()) => {
                        let _ = progress_tx.try_send(Event::DownloadCached(pkg.ident.clone()));
                        results.push(DownloadedFile {
                            ident: pkg.ident.clone(),
                            url: url.clone(),
                            cache_path,
                            target_filename: target_name,
                            verified: true,
                        });
                        continue;
                    }
                    Err(e) => {
                        // A cache hit that fails its hash is not a hit. Delete it so
                        // the fresh download below cannot later be mistaken for it.
                        tracing::warn!("{}: cached file failed verification: {e}", pkg.name());
                        let _ = std::fs::remove_file(&cache_path);
                    }
                },
                None => {
                    // Scoop warns and accepts: there is nothing to compare against.
                    tracing::warn!(
                        "{}: no hash in manifest, cached file {} cannot be verified",
                        pkg.name(),
                        cache_path.display()
                    );
                    let _ = progress_tx.try_send(Event::DownloadCached(pkg.ident.clone()));
                    results.push(DownloadedFile {
                        ident: pkg.ident.clone(),
                        url: url.clone(),
                        cache_path,
                        target_filename: target_name,
                        verified: false,
                    });
                    continue;
                }
            }
        }

        if let Err(e) = crate::infra::fs::ensure_dir(&cache_root) {
            let _ = progress_tx.try_send(Event::DownloadError(pkg.ident.clone(), e.to_string()));
            failed += 1;
            continue;
        }

        let cache_path = cache_root.join(cache_filename(pkg, url));
        let tmp_name = cache_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| pkg.name().to_owned());
        let tmp_path = cache_root.join(format!("{tmp_name}.download"));
        let ident = pkg.ident.clone();

        match session
            .http_client()
            // A URL fragment is client-side only; the server must never see it.
            .download(
                request_url(url),
                &tmp_path,
                ident.clone(),
                Some(progress_tx.clone()),
            )
            .await
        {
            Ok(_) => {
                if let Some(expected) = expected_hash
                    && let Err(e) = verify_file_hash(&tmp_path, expected)
                {
                    let _ = std::fs::remove_file(&tmp_path);
                    let _ = progress_tx
                        .try_send(Event::DownloadError(pkg.ident.clone(), e.to_string()));
                    failed += 1;
                    continue;
                }

                // `--force` re-downloads over an existing cache file, and on
                // Windows `rename` refuses to replace a destination.
                if cache_path.exists() {
                    let _ = std::fs::remove_file(&cache_path);
                }
                if let Err(e) = std::fs::rename(&tmp_path, &cache_path) {
                    let _ = progress_tx
                        .try_send(Event::DownloadError(pkg.ident.clone(), e.to_string()));
                    failed += 1;
                    continue;
                }

                results.push(DownloadedFile {
                    ident: pkg.ident.clone(),
                    url: url.clone(),
                    cache_path,
                    target_filename: target_name,
                    verified: expected_hash.is_some(),
                });
            }
            Err(e) => {
                let _ = tmp_path.exists().then(|| std::fs::remove_file(&tmp_path));
                let _ =
                    progress_tx.try_send(Event::DownloadError(pkg.ident.clone(), e.to_string()));
                failed += 1;
            }
        }
    }

    // A package is only ready for installation when *every* artifact is in the
    // cache: installing half of a two-file package would produce a broken app.
    if failed > 0 {
        let msg = format!(
            "{} of {} file(s) failed for package {}",
            failed,
            urls.len(),
            pkg.name()
        );
        let _ = progress_tx.try_send(Event::DownloadError(pkg.ident.clone(), msg.clone()));
        return Err(download_error(msg));
    }

    Ok(results)
}

/// Download several resolved packages through [`download_package`].
///
/// A package is omitted from the result when any of its artifacts failed; the
/// remaining packages are still attempted.
pub async fn download_packages(
    session: &Session,
    packages: &[Package],
    reuse_cache: bool,
    arch: Arch,
) -> Result<Vec<DownloadedFile>> {
    let mut results = Vec::new();

    for pkg in packages {
        match download_package(session, pkg, reuse_cache, arch).await {
            Ok(files) => results.extend(files),
            Err(e) => tracing::warn!("{}: download failed: {e}", pkg.name()),
        }
    }

    let _ = session
        .event_bus()
        .core_sender()
        .try_send(Event::DownloadDone);

    Ok(results)
}

pub async fn calculate_download_size(
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
                // The fragment is not part of the request.
                let request = request_url(url);
                match session.http_client().content_length(request).await {
                    Ok(Some(len)) => {
                        tracing::debug!(
                            "Package {}@{} URL {} size: {} bytes",
                            pkg_name,
                            pkg_version,
                            request,
                            len
                        );
                        total_for_pkg += len;
                    }
                    Ok(None) => {
                        tracing::debug!(
                            "Package {}@{} URL {} size: unknown (server did not provide)",
                            pkg_name,
                            pkg_version,
                            request
                        );
                        estimated_for_pkg = true;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Package {}@{} URL {} error: {}",
                            pkg_name,
                            pkg_version,
                            request,
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

/// Split a Scoop download URL into the URL that is actually requested and the
/// local name declared by a `#/<name>` fragment.
///
/// Scoop's `url_filename` (`lib/download.ps1`) takes the last path segment of
/// the whole URL, so `https://example.com/file.exe#/dl.7z` yields `dl.7z`.
/// Any fragment is client-side only and never reaches the server; only the
/// `#/` spelling renames the file.
pub fn split_url_fragment(url: &str) -> (&str, Option<&str>) {
    let (base, fragment) = match url.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (url, None),
    };

    let target = fragment
        .and_then(|f| f.strip_prefix('/'))
        .map(|f| f.split('?').next().unwrap_or(f))
        .filter(|f| !f.is_empty());

    (base, target)
}

/// The URL to hand to the HTTP client: a fragment is client-side only.
pub fn request_url(url: &str) -> &str {
    split_url_fragment(url).0
}

/// The name the downloaded bytes must have on disk.
///
/// This is Scoop's `url_filename`: the `#/name` fragment when present,
/// otherwise the last path segment of the URL with its query string removed.
pub fn target_filename(url: &str) -> String {
    if let Some(name) = split_url_fragment(url).1 {
        return name.to_owned();
    }

    let base = request_url(url);
    let leaf = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let leaf = leaf.split('?').next().unwrap_or(leaf).trim();

    if leaf.is_empty() {
        "download".to_owned()
    } else {
        leaf.to_owned()
    }
}

/// Generate the cache filename exactly as Scoop's `cache_path` does:
/// `<app>#<version>#<url>`, where every run of characters outside
/// `[\w.-]` in the *whole* URL (fragment included) becomes a single `_`.
///
/// The full URL is what makes two artifacts of one package distinct:
/// `https://example.com/file.exe#/dl.7z` and
/// `https://example.com/file.exe#/other.7z` must not share a cache file.
pub fn cache_filename(pkg: &Package, url: &str) -> String {
    format!(
        "{}#{}#{}",
        pkg.name(),
        pkg.version(),
        sanitize_url_for_cache(url)
    )
}

/// The pre-migration Scoop name: `sha256(url)[..7]` plus the URL extension.
/// Kept so cache files written by older Rake/Scoop builds are still found.
fn legacy_cache_filename(pkg: &Package, url: &str) -> String {
    let hash = hex::encode(Sha256::digest(url.as_bytes()));
    format!(
        "{}#{}#{}{}",
        pkg.name(),
        pkg.version(),
        &hash[..7],
        url_extension(url)
    )
}

fn sanitize_url_for_cache(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    let mut in_run = false;

    for ch in url.chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '.' | '-') {
            out.push(ch);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }

    out
}

fn url_extension(url: &str) -> &str {
    let leaf = url.rsplit(['/', '\\']).next().unwrap_or(url);
    let leaf = leaf.split('?').next().unwrap_or(leaf);
    match leaf.rfind('.') {
        Some(pos) => &leaf[pos..],
        None => "",
    }
}

/// Find a cached file for a package+URL.
///
/// The lookup is deterministic: the current Scoop name first, then the legacy
/// hash name. A glob over `name#version#*` would hand the first URL's file to
/// every other URL of the same package, which is exactly the multi-artifact
/// bug the `#/name` fragment makes possible.
pub fn find_cached_file(
    cache_root: &std::path::Path,
    pkg: &Package,
    url: &str,
) -> Option<std::path::PathBuf> {
    for name in [cache_filename(pkg, url), legacy_cache_filename(pkg, url)] {
        let path = cache_root.join(name);
        if path.is_file() && path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            return Some(path);
        }
    }

    None
}

fn cache_root(session: &Session) -> PathBuf {
    session
        .config()
        .cache_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("cache"))
}

fn download_error(msg: String) -> crate::Error {
    crate::Error::Io(std::io::Error::other(msg))
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
mod url_tests {
    use super::*;
    use rake_domain::manifest::Manifest;
    use rake_domain::package::PackageStatus;

    fn pkg(name: &str, version: &str) -> Package {
        let manifest: Manifest =
            serde_json::from_str(&format!(r#"{{"version":"{version}"}}"#)).unwrap();
        Package::new(
            PackageIdent::new("main", name),
            manifest,
            None,
            PackageStatus::NotInstalled,
        )
    }

    #[test]
    fn a_slash_fragment_renames_the_file_and_is_not_requested() {
        let (base, target) = split_url_fragment("https://example.com/file.exe#/dl.7z");

        assert_eq!(base, "https://example.com/file.exe");
        assert_eq!(target, Some("dl.7z"));
    }

    #[test]
    fn a_fragment_can_carry_an_exe_name() {
        let (base, target) = split_url_fragment("https://example.com/app-win64.exe#/app.exe");

        assert_eq!(base, "https://example.com/app-win64.exe");
        assert_eq!(target, Some("app.exe"));
    }

    #[test]
    fn a_url_without_a_fragment_is_left_alone() {
        let url = "https://example.com/file.zip";
        let (base, target) = split_url_fragment(url);

        assert_eq!(base, url);
        assert_eq!(target, None);
    }

    #[test]
    fn a_non_slash_fragment_is_stripped_but_does_not_rename() {
        // Scoop only honours `#/`. Any other fragment is still client-side.
        let (base, target) = split_url_fragment("https://example.com/file.zip#section");

        assert_eq!(base, "https://example.com/file.zip");
        assert_eq!(target, None);
    }

    #[test]
    fn target_filename_prefers_the_fragment_over_the_remote_name() {
        assert_eq!(
            target_filename("https://example.com/file.exe#/dl.7z"),
            "dl.7z"
        );
        assert_eq!(
            target_filename("https://example.com/app-win64.exe#/app.exe"),
            "app.exe"
        );
    }

    #[test]
    fn target_filename_uses_the_last_path_segment_and_drops_the_query() {
        assert_eq!(
            target_filename("https://example.com/downloads/tool.zip"),
            "tool.zip"
        );
        assert_eq!(
            target_filename("https://example.com/tool.zip?token=abc123"),
            "tool.zip"
        );
    }

    #[test]
    fn target_filename_has_a_fallback_for_a_bare_directory_url() {
        assert_eq!(target_filename("https://example.com/"), "download");
    }

    /// The exact name Scoop's `cache_path` produces: every run of characters
    /// outside `[\w.-]` collapses to one underscore, and the whole URL —
    /// fragment included — is part of the name.
    #[test]
    fn cache_filename_matches_scoop_for_a_fragment_url() {
        let git = pkg("git", "2.50.0");

        assert_eq!(
            cache_filename(&git, "https://example.com/file.exe#/dl.7z"),
            "git#2.50.0#https_example.com_file.exe_dl.7z"
        );
    }

    #[test]
    fn cache_filename_collapses_runs_of_unsafe_characters() {
        let app = pkg("app", "1.0");

        assert_eq!(
            cache_filename(&app, "https://a.example//b"),
            "app#1.0#https_a.example_b"
        );
    }

    /// Two artifacts of the same package that share a remote file name but
    /// differ in the fragment must not share a cache file.
    #[test]
    fn cache_filename_distinguishes_fragments() {
        let app = pkg("app", "1.0");
        let one = cache_filename(&app, "https://example.com/file.exe#/dl.7z");
        let two = cache_filename(&app, "https://example.com/file.exe#/other.7z");

        assert_ne!(one, two);
    }

    /// A cache name is one flat file name: exactly the two `#` separators
    /// Scoop uses, and no path separators from the URL.
    #[test]
    fn cache_filename_is_a_flat_name_with_two_separators() {
        let app = pkg("app", "1.0");
        let name = cache_filename(&app, "https://example.com/a/b/file.exe#/dl.7z");

        assert_eq!(name.matches('#').count(), 2);
        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
        assert!(!name.contains(':'));
    }

    #[test]
    fn find_cached_file_uses_the_current_name_first() {
        let dir = tempfile::tempdir().unwrap();
        let app = pkg("app", "1.0");
        let url = "https://example.com/file.exe#/dl.7z";
        let path = dir.path().join(cache_filename(&app, url));
        std::fs::write(&path, b"bytes").unwrap();

        assert_eq!(find_cached_file(dir.path(), &app, url), Some(path));
    }

    #[test]
    fn find_cached_file_still_sees_the_legacy_hash_name() {
        let dir = tempfile::tempdir().unwrap();
        let app = pkg("app", "1.0");
        let url = "https://example.com/file.exe#/dl.7z";
        let path = dir.path().join(legacy_cache_filename(&app, url));
        std::fs::write(&path, b"bytes").unwrap();

        assert_eq!(find_cached_file(dir.path(), &app, url), Some(path));
    }

    /// The glob this replaced returned the first `app#1.0#*` entry for every
    /// URL, so the second artifact of a package was silently the first one.
    #[test]
    fn find_cached_file_does_not_reuse_another_urls_file() {
        let dir = tempfile::tempdir().unwrap();
        let app = pkg("app", "1.0");
        let first = "https://example.com/file.exe#/one.7z";
        let second = "https://example.com/file.exe#/two.7z";
        std::fs::write(dir.path().join(cache_filename(&app, first)), b"one").unwrap();

        assert_eq!(find_cached_file(dir.path(), &app, second), None);
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
