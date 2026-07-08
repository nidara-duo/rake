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
        return Ok(0);
    }

    let files = cache_list(session, query)?;
    let count = files.len();

    for f in &files {
        let _ = std::fs::remove_file(f.path());
        // Remove companion .txt file (scoop convention)
        let txt_path = f.path().with_extension("txt");
        let _ = std::fs::remove_file(txt_path);
    }

    Ok(count)
}
