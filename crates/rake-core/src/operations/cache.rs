use std::path::{Path, PathBuf};

use crate::Result;
use crate::infra::fs;
use crate::session::Session;

#[derive(Debug, Clone)]
pub struct CacheFile {
    path: PathBuf,
    name: String,
    version: String,
}

impl CacheFile {
    pub fn from_path(path: PathBuf) -> Option<Self> {
        let filename = path.file_name()?.to_str()?.to_owned();

        // A `.txt` alongside a cache file is Scoop's sidecar holding the source URL, not
        // a download of its own. The name still looks like an entry — it contains `#` — so
        // without this it was listed by `rake cache` as a separate file and counted again
        // on removal, double-reporting every archive that had a sidecar.
        if filename.to_ascii_lowercase().ends_with(".txt") {
            return None;
        }

        let (name, version) = Self::parse_filename(&filename)?;
        Some(Self {
            path,
            name: name.to_owned(),
            version: version.to_owned(),
        })
    }

    fn parse_filename(filename: &str) -> Option<(&str, &str)> {
        let (name, rest) = filename.split_once('#')?;
        // name#hash.ext (old format) → version "-"
        // name#version#hash.ext (new format) → real version
        let version = rest.split_once('#').map(|(v, _)| v).unwrap_or("-");
        Some((name, version))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn filename(&self) -> &str {
        self.path.file_name().and_then(|s| s.to_str()).unwrap_or("")
    }

    pub fn size(&self) -> u64 {
        self.path.metadata().map(|m| m.len()).unwrap_or(0)
    }
}

pub fn cache_list(session: &Session, query: &str) -> Result<Vec<CacheFile>> {
    let cache_dir = session
        .config()
        .cache_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("cache"));

    if !cache_dir.exists() {
        return Ok(Vec::new());
    }

    let is_wildcard = query == "*" || query.is_empty();
    let query_lower = query.to_lowercase();

    let files: Vec<CacheFile> = std::fs::read_dir(&cache_dir)
        .map_err(crate::Error::Io)?
        .filter_map(|entry| entry.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| CacheFile::from_path(e.path()))
        .filter(|f| is_wildcard || f.name().to_lowercase().contains(&query_lower))
        .collect();

    Ok(files)
}

pub fn cache_remove(session: &Session, query: &str) -> Result<usize> {
    let cache_dir = session
        .config()
        .cache_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("cache"));

    if query == "*" || query == "-a" || query == "--all" {
        if cache_dir.exists() {
            fs::empty_dir(&cache_dir)?;
        }
        // Reported as zero because the directory is emptied wholesale and nothing was
        // counted before. The previous value was right by accident; say so plainly
        // rather than inventing a number.
        return Ok(0);
    }

    let files = cache_list(session, query)?;

    // Counted after deleting, not before. Returning `files.len()` announced a removal for
    // every file whether or not it was deleted, so a cache file held open by anything
    // still produced "removed N" — the same false success as the one fixed in `cleanup`
    // and `uninstall`.
    let mut removed = 0usize;
    for f in &files {
        if remove_cache_entry(f.path()) {
            removed += 1;
        }
    }

    Ok(removed)
}

/// Delete one cache entry and its companion `.txt`, reporting whether the entry itself
/// went away.
fn remove_cache_entry(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => {
            // The `.txt` sidecar is a Scoop convention, not the thing that was asked for;
            // failing to remove it is worth noting but must not fail the removal.
            let txt = path.with_extension("txt");
            if let Err(e) = std::fs::remove_file(&txt)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!("could not remove {}: {e}", txt.display());
            }
            true
        }
        Err(e) => {
            tracing::warn!("could not remove {}: {e}", path.display());
            false
        }
    }
}

/// Remove cached downloads for one app, keeping any entry belonging to
/// `current_version`.
///
/// `rake cleanup -k` used to call `cache_remove(session, "*")`, which emptied the whole
/// cache — including the download for the version still installed. Scoop keeps that one
/// (`Remove-Item "$cachedir\$app#*" -Exclude "$app#$current_version#*"`,
/// libexec/scoop-cleanup.ps1:33), which is the point of the flag: it drops stale mirrors
/// without forcing a re-download of what is in use.
pub fn cache_remove_except(
    session: &Session,
    app: &str,
    current_version: Option<&str>,
) -> Result<usize> {
    let mut removed = 0usize;
    for f in cache_list(session, app)? {
        if f.name().eq_ignore_ascii_case(app)
            && Some(f.version()) == current_version
            && f.version() != "-"
        {
            continue;
        }
        if remove_cache_entry(f.path()) {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Remove interrupted downloads, which Scoop sweeps at the end of a `-k` cleanup
/// (libexec/scoop-cleanup.ps1:80).
pub fn cache_remove_partials(session: &Session) -> Result<usize> {
    let cache_dir = session
        .config()
        .cache_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("cache"));
    if !cache_dir.exists() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in std::fs::read_dir(&cache_dir)
        .map_err(crate::Error::Io)?
        .flatten()
    {
        let path = entry.path();
        let is_partial = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".download"));
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            && is_partial
            && std::fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::config::Config;

    fn session_with_cache(root: &Path) -> Session {
        let cache = root.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        Session::from_config(Config {
            root_path: Some(root.to_path_buf()),
            cache_path: Some(cache),
            ..Default::default()
        })
    }

    /// The count must describe what was deleted, not what was attempted. Returning
    /// `files.len()` announced a removal for every candidate even when the file was held
    /// open and nothing was deleted — the same false success as in `cleanup`.
    #[test]
    fn counts_only_what_actually_went_away() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let session = session_with_cache(tmp.path());

        for name in ["demo#1.0.0#a.zip", "demo#1.0.0#b.zip"] {
            std::fs::write(cache.join(name), b"x").unwrap();
        }

        assert_eq!(cache_remove(&session, "demo").unwrap(), 2);
        assert!(!cache.join("demo#1.0.0#a.zip").exists());
        assert!(!cache.join("demo#1.0.0#b.zip").exists());
    }

    /// The companion `.txt` is Scoop's sidecar, not a download of its own. It must be removed
    /// alongside the archive without being counted as a second entry — it looks like one,
    /// since the name also contains `#`.
    #[test]
    fn removes_the_companion_txt_without_counting_it() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let session = session_with_cache(tmp.path());

        std::fs::write(cache.join("demo#1.0.0#a.zip"), b"x").unwrap();
        std::fs::write(cache.join("demo#1.0.0#a.txt"), b"url").unwrap();

        assert_eq!(cache_remove(&session, "demo").unwrap(), 1);
        assert!(!cache.join("demo#1.0.0#a.txt").exists());
    }

    /// The same reason: a sidecar must not appear in the listing either.
    #[test]
    fn a_sidecar_is_not_listed_as_a_cache_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let session = session_with_cache(tmp.path());

        std::fs::write(cache.join("demo#1.0.0#a.zip"), b"x").unwrap();
        std::fs::write(cache.join("demo#1.0.0#a.txt"), b"url").unwrap();

        let listed = cache_list(&session, "demo").unwrap();
        assert_eq!(listed.len(), 1, "got {:?}", listed);
        assert_eq!(listed[0].filename(), "demo#1.0.0#a.zip");
    }

    #[test]
    fn nothing_to_remove_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session_with_cache(tmp.path());
        assert_eq!(cache_remove(&session, "nosuchapp").unwrap(), 0);
    }

    /// `cleanup -k` keeps the current version's download, so the count must reflect the
    /// entries it actually dropped.
    #[test]
    fn except_keeps_the_current_version_and_counts_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let session = session_with_cache(tmp.path());

        for name in [
            "demo#1.0.0#old.zip",
            "demo#2.0.0#new.zip",
            "demo#2.0.0#mirror.zip",
        ] {
            std::fs::write(cache.join(name), b"x").unwrap();
        }

        let removed = cache_remove_except(&session, "demo", Some("2.0.0")).unwrap();
        assert_eq!(removed, 1, "only the stale version should be counted");
        assert!(!cache.join("demo#1.0.0#old.zip").exists());
        assert!(cache.join("demo#2.0.0#new.zip").exists());
        assert!(cache.join("demo#2.0.0#mirror.zip").exists());
    }

    /// Interrupted downloads are swept, and only those are counted.
    #[test]
    fn partials_are_swept_and_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let session = session_with_cache(tmp.path());

        std::fs::write(cache.join("demo#1.0.0#a.zip"), b"x").unwrap();
        std::fs::write(cache.join("demo#1.0.0#a.zip.download"), b"partial").unwrap();
        std::fs::write(cache.join("demo#1.0.0#b.zip.download"), b"partial").unwrap();

        assert_eq!(cache_remove_partials(&session).unwrap(), 2);
        assert!(
            cache.join("demo#1.0.0#a.zip").exists(),
            "finished entries stay"
        );
        assert!(!cache.join("demo#1.0.0#a.zip.download").exists());
    }
}
