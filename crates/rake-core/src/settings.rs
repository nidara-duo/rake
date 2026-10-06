//! Where the settings file lives, and how it is read and written.
//!
//! Deliberately separate from `config_resolver`. That module reads *Scoop's* settings so
//! the two tools share a package tree; this one reads *our* settings. Mixing them would
//! be a hazard in both directions: Scoop's `set_config` rewrites its whole document, so
//! anything we stored there would be dropped the first time a user ran `scoop config`.

use std::path::{Path, PathBuf};

use rake_domain::settings::Settings;

use crate::Result;

/// `%XDG_CONFIG_HOME%\rake\settings.json`, or `%USERPROFILE%\.config\rake\settings.json`.
///
/// Follows the same resolution Scoop uses for its own config (`lib/core.ps1:1347`), so a
/// user who has moved their config home does not end up with two locations to remember.
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
    Some(home.join("rake").join("settings.json"))
}

/// Read the settings, falling back to the defaults.
///
/// A missing file is the normal case and must not be an error. A malformed one is worth
/// reporting rather than swallowing — a typo in the settings file otherwise looks
/// exactly like "my setting was ignored", which is the sort of thing that gets an hour
/// lost.
pub fn load() -> Result<Settings> {
    let Some(path) = settings_path() else {
        return Ok(Settings::default());
    };
    load_from(&path)
}

/// Read settings from a specific path. Separated from [`load`] so it can be tested
/// against a temporary directory instead of the real one.
pub fn load_from(path: &Path) -> Result<Settings> {
    if !path.is_file() {
        return Ok(Settings::default());
    }

    let content = crate::infra::json::read_to_string(path)?;
    // serde_json's own error names the line, which is the useful part when a user has
    // hand-edited the file.
    serde_json::from_str(&content)
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("{}: {e}", path.display()))))
}

/// Write settings, creating the directory if needed.
///
/// Serialises the whole document rather than patching a single key, so the file always
/// reflects what the program believes. `preserve_order` on the serde_json feature keeps
/// the user's key order stable instead of reshuffling on every save.
pub fn save(settings: &Settings) -> Result<()> {
    let path = settings_path()
        .ok_or_else(|| crate::Error::Io(std::io::Error::other("cannot locate a config home")))?;
    save_to(settings, &path)
}

pub fn save_to(settings: &Settings, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        crate::infra::fs::ensure_dir(parent)?;
    }
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| crate::Error::Io(std::io::Error::other(e)))?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// The common case: no file at all means the defaults, and must not be an error.
    #[test]
    fn missing_file_yields_defaults() {
        let dir = tmp();
        let settings = load_from(&dir.path().join("settings.json")).unwrap();
        assert_eq!(settings, Settings::default());
        assert!(settings.status.offline_by_default);
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tmp();
        let path = dir.path().join("settings.json");

        let mut written = Settings::default();
        written.set("status.hide_offline_note", "true").unwrap();
        save_to(&written, &path).unwrap();

        assert_eq!(load_from(&path).unwrap(), written);
    }

    /// The file is created inside a directory that does not exist yet, since
    /// `~/.config/rake` will not be there on a first run.
    #[test]
    fn creates_missing_parent_directories() {
        let dir = tmp();
        let path = dir
            .path()
            .join("nested")
            .join("deeper")
            .join("settings.json");
        save_to(&Settings::default(), &path).unwrap();
        assert!(path.is_file());
    }

    /// A hand-edited file with a typo must say so, rather than being discarded — an
    /// ignored file and a correct-but-unread file look identical from the outside.
    #[test]
    fn malformed_file_is_reported_with_its_path() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();

        let err = load_from(&path).unwrap_err().to_string();
        assert!(err.contains("settings.json"), "got: {err}");
    }

    /// A wrong type for a known key is also worth naming.
    #[test]
    fn wrong_type_is_reported() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"status":{"hide_offline_note":"yes please"}}"#).unwrap();
        assert!(load_from(&path).is_err());
    }

    /// The file is written as UTF-8 without a BOM. Writing one would be self-inflicted —
    /// Rake is now tolerant on read, but there is no reason to create the problem.
    #[test]
    fn written_file_has_no_bom() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        save_to(&Settings::default(), &path).unwrap();

        let head = std::fs::read(&path).unwrap();
        assert_eq!(head.first(), Some(&b'{'), "file must start with '{{'");
    }

    /// The path follows the same resolution as Scoop's config, so there is one config home
    /// to remember rather than two.
    #[test]
    fn path_lives_under_the_config_home() {
        let Some(path) = settings_path() else {
            return;
        };
        let home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .unwrap();
        assert_eq!(path.parent().unwrap().parent().unwrap(), home);
        assert_eq!(path.file_name().unwrap(), "settings.json");
        // Not inside Scoop's directory: that file is rewritten wholesale by Scoop.
        assert!(!path.to_string_lossy().contains("scoop"));
    }
}
