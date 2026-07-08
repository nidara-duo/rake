use std::path::PathBuf;

use rake_domain::config::Config;

use crate::Result;

pub fn resolve_config() -> Result<Config> {
    let mut config = load_config_file().unwrap_or_default();

    if config.root_path.is_none() {
        config.root_path = Some(detect_root_path());
    }

    if config.cache_path.is_none() {
        config.cache_path = Some(config.root_path.as_ref().unwrap().join("cache"));
    }

    if config.global_path.is_none() {
        config.global_path = Some(detect_global_path());
    }

    Ok(config)
}

fn load_config_file() -> Option<Config> {
    let scoop_root = std::env::var("SCOOP")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join("scoop")))?;

    let config_path = scoop_root.join("config.json");
    let content = std::fs::read_to_string(config_path).ok()?;
    serde_json::from_str(&content).ok()
}

fn detect_root_path() -> PathBuf {
    std::env::var("SCOOP")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            let scoop_dir = dirs::home_dir().map(|h| h.join("scoop"));
            if scoop_dir.as_ref().is_some_and(|p| p.exists()) {
                scoop_dir
            } else {
                dirs::home_dir().map(|h| h.join("rake"))
            }
        })
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\user\rake"))
}

fn detect_global_path() -> PathBuf {
    std::env::var("SCOOP_GLOBAL")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\scoop"))
}
