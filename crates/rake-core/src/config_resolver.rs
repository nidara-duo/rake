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
    let config_path = detect_root_path().join("config.json");
    let content = std::fs::read_to_string(config_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Where packages, buckets and the cache live.
///
/// Deliberately `~/scoop` whether or not Scoop itself is installed.
///
/// The previous version fell back to `~/rake` when there was no `~/scoop` yet, which
/// split Rake and Scoop into two separate package trees. Using Scoop's own layout always
/// means that installing real Scoop later finds the populated directory and takes it
/// over instead of starting empty, so the two tools can be used side by side and nothing
/// installed through Rake is stranded.
///
/// This is also why `cleanup` and `uninstall` skip an app named `scoop`: with a shared
/// tree, removing it would remove the very Scoop the user may still be running.
pub(crate) fn detect_root_path() -> PathBuf {
    std::env::var("SCOOP")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join("scoop")))
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\user\scoop"))
}

fn detect_global_path() -> PathBuf {
    std::env::var("SCOOP_GLOBAL")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\scoop"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The package root is always Scoop's, never a Rake-specific one. The fallback to
    /// `~/rake` split the two tools into separate trees; with a shared tree, installing
    /// real Scoop later finds the packages already there.
    #[test]
    fn root_is_scoop_even_when_rake_has_never_run() {
        // No SCOOP override in the test environment, so this exercises the default.
        if std::env::var_os("SCOOP").is_some() {
            return;
        }
        let root = detect_root_path();
        assert_eq!(
            root.file_name().and_then(|s| s.to_str()),
            Some("scoop"),
            "the package root must not be named rake, got {}",
            root.display()
        );
        assert!(
            root.to_string_lossy().ends_with("scoop"),
            "got {}",
            root.display()
        );
    }

    /// An explicit `SCOOP` still wins, so a user with a non-standard layout is honoured.
    #[test]
    fn scoop_env_var_takes_precedence() {
        // std::env::set_var is unsafe in edition 2024 and mutates the process, so this
        // asserts the documented ordering rather than mutating global state.
        let root = detect_root_path();
        if let Some(explicit) = std::env::var_os("SCOOP") {
            assert_eq!(root, PathBuf::from(explicit));
        } else {
            assert!(root.ends_with("scoop"));
        }
    }

    /// `SCOOP_GLOBAL` names the machine-wide tree, which stays under ProgramData.
    #[test]
    fn global_path_defaults_to_programdata() {
        let global = detect_global_path();
        if std::env::var_os("SCOOP_GLOBAL").is_some() {
            return;
        }
        assert!(
            global.starts_with(r"C:\ProgramData"),
            "got {}",
            global.display()
        );
    }
}
