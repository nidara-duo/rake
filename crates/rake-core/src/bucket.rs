use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use walkdir::WalkDir;

use crate::Result;
use rake_domain::manifest::Manifest;

pub static BUILTIN_BUCKETS: &[(&str, &str)] = &[
    ("main", "https://github.com/ScoopInstaller/Main"),
    ("extras", "https://github.com/ScoopInstaller/Extras"),
    ("games", "https://github.com/calinou/scoop-games"),
    ("java", "https://github.com/ScoopInstaller/Java"),
    ("php", "https://github.com/ScoopInstaller/PHP"),
    ("versions", "https://github.com/ScoopInstaller/Versions"),
    (
        "nonportable",
        "https://github.com/ScoopInstaller/Nonportable",
    ),
];

#[derive(Debug, Clone)]
pub struct Bucket {
    path: PathBuf,
    name: String,
    remote_url: OnceLock<Option<String>>,
}

impl Bucket {
    pub fn from(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if !path.exists() {
            return Err(crate::Error::Domain(rake_domain::Error::BucketNotFound(
                name,
            )));
        }

        Ok(Self {
            path: path.to_owned(),
            name,
            remote_url: OnceLock::new(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_held(&self) -> bool {
        self.path.join(".hold").exists()
    }

    pub fn remote_url(&self) -> Option<&str> {
        self.remote_url
            .get_or_init(|| {
                let output = std::process::Command::new("git")
                    .arg("remote")
                    .arg("get-url")
                    .arg("origin")
                    .current_dir(&self.path)
                    .output()
                    .ok()?;
                if output.status.success() {
                    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
                } else {
                    None
                }
            })
            .as_deref()
    }

    pub fn source(&self) -> String {
        self.remote_url()
            .map(|s| s.to_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().to_string())
    }

    pub fn manifest_count(&self) -> usize {
        self.manifest_paths().len()
    }

    pub fn path_of_manifest(&self, name: &str) -> Option<PathBuf> {
        let filename = format!("{name}.json");

        let flat = self.path.join(&filename);
        if flat.exists() {
            return Some(flat);
        }

        let standard = self.path.join("bucket").join(&filename);
        if standard.exists() {
            return Some(standard);
        }

        let first = name.chars().next()?;
        let category = if first.is_ascii_lowercase() {
            first.to_string()
        } else {
            "#".to_owned()
        };
        let categorized = self.path.join("bucket").join(&category).join(&filename);
        if categorized.exists() {
            return Some(categorized);
        }

        None
    }

    pub fn load_manifest(&self, name: &str) -> Option<Manifest> {
        let path = self.path_of_manifest(name)?;
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn manifest_paths(&self) -> Vec<PathBuf> {
        let bucket_dir = self.path.join("bucket");
        let search_dir = if bucket_dir.exists() {
            bucket_dir
        } else {
            self.path.clone()
        };

        WalkDir::new(&search_dir)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.path().extension().is_some_and(|ext| ext == "json")
                    && e.file_name() != "package.json"
            })
            .map(|e| e.path().to_owned())
            .collect()
    }
}

pub fn added_buckets(session: &crate::session::Session) -> Result<Vec<Bucket>> {
    let buckets_dir = session
        .config()
        .root_path
        .as_ref()
        .map(|p| p.join("buckets"));

    let buckets_dir = match buckets_dir {
        Some(p) if p.exists() => p,
        _ => return Ok(vec![]),
    };

    let mut buckets = Vec::new();
    for entry in std::fs::read_dir(&buckets_dir).map_err(crate::Error::Io)? {
        let entry = entry.map_err(crate::Error::Io)?;
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            && let Ok(bucket) = Bucket::from(&entry.path())
        {
            buckets.push(bucket);
        }
    }

    Ok(buckets)
}
