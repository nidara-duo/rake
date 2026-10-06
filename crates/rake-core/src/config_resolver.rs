use std::path::PathBuf;

use rake_domain::config::Config;

use crate::Result;

pub fn resolve_config() -> Result<Config> {
    let mut config = load_config_file().unwrap_or_default();

    if config.root_path.is_none() {
        config.root_path = Some(detect_root_path());
    }

    if config.cache_path.is_none() {
        config.cache_path = Some(detect_cache_path(&config));
    }

    if config.global_path.is_none() {
        config.global_path = Some(detect_global_path());
    }

    Ok(config)
}

/// Where Scoop keeps its own settings, so Rake reads the same file.
///
/// Mirrors scoop's lookup in `lib/core.ps1:1347-1371`:
///
/// ```text
/// %XDG_CONFIG_HOME%\scoop\config.json   (or %USERPROFILE%\.config\...\config.json)
/// <root>\config.json                     — portable installs only
/// ```
///
/// The previous implementation looked only at `<root>/config.json`, which is the
/// *portable* case. On a normal machine Scoop writes to `%USERPROFILE%\.config`, so Rake
/// never found the file at all — meaning `last_update` and the `shim` setting were
/// silently ignored.
fn config_file_path() -> Option<PathBuf> {
    let shared = config_home()
        .map(|h| h.join("scoop").join("config.json"))
        .filter(|p| p.is_file());

    shared.or_else(|| {
        let portable = default_root_path().join("config.json");
        portable.is_file().then_some(portable)
    })
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
}

fn load_config_file() -> Option<Config> {
    let path = config_file_path()?;
    // A malformed file must not stop Rake from running, so the parse result is
    // discarded rather than reported — the same tolerance scoop's `load_cfg` shows by
    // returning `$null` on failure. The read goes through the shared helper because a
    // BOM would otherwise make the file silently unreadable.
    crate::infra::json::read(&path).ok()
}

fn default_root_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join("scoop"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\user\scoop"))
}

/// Where packages, buckets and the cache live.
///
/// Always Scoop's directory, and in Scoop's own precedence order
/// (`lib/core.ps1:1375`): `$SCOOP`, then `root_path` from the shared config file, then
/// `~/scoop`.
///
/// The previous version fell back to `~/rake` when `~/scoop` did not exist, which split
/// Rake and Scoop into two separate package trees. Using Scoop's layout always means that
/// installing real Scoop later finds the populated directory and takes it over instead of
/// starting empty, so the two tools can be used side by side and nothing installed
/// through Rake is stranded.
///
/// This is also why `cleanup` and `uninstall` skip an app named `scoop`: with a shared
/// tree, removing it would remove the very Scoop the user may still be running.
pub(crate) fn detect_root_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("SCOOP") {
        return PathBuf::from(explicit);
    }

    // Consulted only after `$SCOOP`, matching scoop. Reading the config needs a path to
    // look at first, which is why `config_file_path` derives one without asking for the
    // root — otherwise the lookup would be circular.
    if let Some(from_config) = load_config_file().and_then(|c| c.root_path) {
        return from_config;
    }

    default_root_path()
}

/// Where downloads are cached — `$SCOOP_CACHE`, then the config, then `<root>\cache`
/// (`lib/core.ps1:1385`). `$SCOOP_CACHE` was previously ignored entirely, so a user who
/// had set it silently got the default location instead.
fn detect_cache_path(config: &Config) -> PathBuf {
    if let Some(explicit) = std::env::var_os("SCOOP_CACHE") {
        return PathBuf::from(explicit);
    }
    if let Some(from_config) = &config.cache_path {
        return from_config.clone();
    }
    config
        .root_path
        .clone()
        .unwrap_or_else(default_root_path)
        .join("cache")
}

/// The machine-wide tree — `$SCOOP_GLOBAL`, then the config, then ProgramData
/// (`lib/core.ps1:1378`).
pub(crate) fn detect_global_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("SCOOP_GLOBAL") {
        return PathBuf::from(explicit);
    }
    if let Some(from_config) = load_config_file().and_then(|c| c.global_path) {
        return from_config;
    }
    PathBuf::from(r"C:\ProgramData\scoop")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The package root is always Scoop's, never a Rake-specific one. The fallback to
    /// `~/rake` split the two tools into separate trees; with a shared tree, installing
    /// real Scoop later finds the packages already there.
    #[test]
    fn root_is_scoop_by_default() {
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
    }

    /// `$SCOOP` outranks the config file, as in scoop's `Select-Object -First 1` chain.
    #[test]
    fn scoop_env_var_takes_precedence() {
        let root = detect_root_path();
        if let Some(explicit) = std::env::var_os("SCOOP") {
            assert_eq!(root, PathBuf::from(explicit));
        } else {
            assert!(root.ends_with("scoop"));
        }
    }

    /// The config file lives under `.config\scoop`, not in the package root. Looking only
    /// in the root is the portable-install case and misses every normal machine.
    #[test]
    fn config_lives_under_dot_config_not_in_the_root() {
        let Some(path) = config_file_path() else {
            return; // no config on this machine, nothing to assert
        };
        let home = config_home().expect("config home");
        assert!(
            path.starts_with(&home),
            "config should be under {}, got {}",
            home.display(),
            path.display()
        );
        assert!(
            !path.starts_with(default_root_path().join("cache")),
            "config must not be confused with the cache directory"
        );
    }

    /// `XDG_CONFIG_HOME` wins when set, matching scoop.
    #[test]
    fn xdg_config_home_is_preferred() {
        let home = config_home().expect("config home");
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            assert_eq!(home, PathBuf::from(xdg));
        } else {
            assert!(home.ends_with(".config"), "got {}", home.display());
        }
    }

    /// `$SCOOP_GLOBAL` names the machine-wide tree, which stays under ProgramData.
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

    /// The cache follows the root unless told otherwise, so a moved root takes the cache
    /// with it.
    #[test]
    fn cache_defaults_to_a_cache_dir_inside_the_root() {
        let config = Config {
            root_path: Some(PathBuf::from(r"C:\somewhere\else")),
            ..Default::default()
        };
        if std::env::var_os("SCOOP_CACHE").is_some() {
            return;
        }
        assert_eq!(
            detect_cache_path(&config),
            PathBuf::from(r"C:\somewhere\else\cache")
        );
    }

    /// An explicit cache path in the config wins over the derived one.
    #[test]
    fn config_cache_path_wins_over_the_derived_one() {
        let config = Config {
            root_path: Some(PathBuf::from(r"C:\root")),
            cache_path: Some(PathBuf::from(r"D:\elsewhere\cache")),
            ..Default::default()
        };
        if std::env::var_os("SCOOP_CACHE").is_some() {
            return;
        }
        assert_eq!(
            detect_cache_path(&config),
            PathBuf::from(r"D:\elsewhere\cache")
        );
    }
}
