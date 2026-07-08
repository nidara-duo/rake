use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub root_path: Option<PathBuf>,

    #[serde(default)]
    pub cache_path: Option<PathBuf>,

    #[serde(default)]
    pub global_path: Option<PathBuf>,

    pub proxy: Option<String>,

    pub aria2_enabled: Option<bool>,
    pub aria2_split: Option<u32>,
    pub aria2_max_connection_per_server: Option<u32>,
    pub aria2_min_split_size: Option<String>,
    pub aria2_retry_wait: Option<u32>,

    pub use_external_7zip: Option<bool>,
    pub no_junction: Option<bool>,
    pub ignore_running_processes: Option<bool>,
    pub debug: Option<bool>,
    pub force_update: Option<bool>,

    pub gh_token: Option<String>,

    pub shim: Option<String>,

    pub last_update: Option<String>,
    pub show_update_log: Option<bool>,
    pub scoop_branch: Option<String>,
    pub scoop_repo: Option<String>,
    pub use_sqlite_cache: Option<bool>,

    pub private_hosts: Option<Vec<PrivateHost>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivateHost {
    #[serde(rename = "match")]
    pub match_pattern: String,
    pub headers: String,
}
