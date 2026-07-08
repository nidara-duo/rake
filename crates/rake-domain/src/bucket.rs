use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Bucket {
    pub name: String,
    pub path: PathBuf,
    pub remote_url: Option<String>,
    pub held: bool,
}

impl Bucket {
    pub fn new(
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        remote_url: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            remote_url,
            held: false,
        }
    }

    pub fn manifest_count(&self) -> usize {
        // TODO: scan bucket directory
        0
    }
}

pub const BUILTIN_BUCKETS: &[(&str, &str)] = &[
    ("main", "https://github.com/ScoopInstaller/Main"),
    ("extras", "https://github.com/ScoopInstaller/Extras"),
    ("games", "https://github.com/Calinou/scoop-games"),
    ("java", "https://github.com/ScoopInstaller/Java"),
    ("php", "https://github.com/ScoopInstaller/PHP"),
    ("versions", "https://github.com/ScoopInstaller/Versions"),
    (
        "nonportable",
        "https://github.com/ScoopInstaller/Nonportable",
    ),
];
