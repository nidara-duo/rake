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
/// One setting that could not be read, and what was used instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingProblem {
    /// Dotted setting name, or `None` when the whole document failed to parse.
    pub key: Option<String>,
    /// What the file contained.
    pub found: String,
    /// What was used instead.
    pub used: String,
    /// Extra explanation. A whole-file parse error carries a line and column; a single
    /// bad key cannot be located that way without a dependency that tracks spans, so the
    /// key name is used instead — the more useful handle on a small JSON file anyway.
    pub detail: Option<String>,
}

/// The settings, plus anything that had to be corrected on the way in.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub settings: Settings,
    pub problems: Vec<SettingProblem>,
}

impl Loaded {
    fn clean(settings: Settings) -> Self {
        Self {
            settings,
            problems: Vec::new(),
        }
    }

    /// One line per problem, for stderr, so a typo in the file is visible instead of
    /// looking like a preference that was quietly ignored.
    pub fn warnings(&self) -> Vec<String> {
        self.problems
            .iter()
            .map(|p| match (&p.key, &p.detail) {
                (Some(key), _) => {
                    format!(
                        "ignoring invalid setting '{key}' ({}); using default {}",
                        p.found, p.used
                    )
                }
                (None, Some(detail)) => {
                    format!("ignoring settings file ({detail}); using all defaults")
                }
                (None, None) => "ignoring settings file; using all defaults".to_owned(),
            })
            .collect()
    }
}

/// Read the settings, falling back to the built-in default for anything unreadable.
///
/// A missing file is the normal case and must not be an error, and neither is a single
/// unreadable value: the document is read key by key so one bad entry does not discard the
/// good ones, and every correction is reported rather than applied silently. A silently
/// substituted default is indistinguishable from "my setting did nothing".
pub fn load() -> Result<Loaded> {
    let Some(path) = settings_path() else {
        return Ok(Loaded::clean(Settings::default()));
    };
    load_from(&path)
}

/// Read settings from a specific path. Separated from [`load`] so it can be tested
/// against a temporary directory instead of the real one.
pub fn load_from(path: &Path) -> Result<Loaded> {
    if !path.is_file() {
        return Ok(Loaded::clean(Settings::default()));
    }

    let raw = crate::infra::json::read_to_string(path)?;
    let mut settings = Settings::default();
    let mut problems = Vec::new();

    // Parsed generically first, so a syntax error is distinguishable from a value of the
    // wrong type: the first costs every setting, the second only one.
    let document: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return Ok(Loaded {
                settings,
                problems: vec![SettingProblem {
                    key: None,
                    found: e.to_string(),
                    used: "all defaults".to_owned(),
                    detail: Some(format!("{}: {e}", path.display())),
                }],
            });
        }
    };

    for key in Settings::keys() {
        let Some(found) = lookup(&document, key) else {
            continue; // absent: the default stands, which is not a problem
        };

        let text = render(found);

        if let Err(e) = settings.set(key, &text) {
            problems.push(SettingProblem {
                key: Some(key.to_owned()),
                found: text,
                used: settings.get(key).unwrap_or_default(),
                detail: Some(e.to_string()),
            });
        }
    }

    // A section that is present but is not an object makes every key inside it
    // unreachable. Falling back silently would hide the user's edit entirely, so it is
    // reported once for the section rather than once per key.
    for section in ["status"] {
        if let Some(value) = document.get(section)
            && !value.is_object()
        {
            problems.push(SettingProblem {
                key: Some(section.to_owned()),
                found: render(value),
                used: "all status defaults".to_owned(),
                detail: None,
            });
        }
    }

    Ok(Loaded { settings, problems })
}

/// Fetch a dotted path out of a parsed document, returning `None` when absent.
fn lookup<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Show a JSON value the way the user wrote it.
///
/// A string is unquoted on purpose: reporting `ignoring invalid setting 'x' ("yes
/// please")` for something the file holds as `"yes please"` adds a level of escaping
/// the user has to mentally undo.
fn render(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
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

    /// Write `json` to a temporary settings file and load it back.
    fn load_text(json: &str) -> Loaded {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, json).unwrap();
        load_from(&path).unwrap()
    }

    /// The common case: no file at all means the defaults, and must not be an error.
    #[test]
    fn missing_file_yields_defaults() {
        let dir = tmp();
        let loaded = load_from(&dir.path().join("settings.json")).unwrap();
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.problems.is_empty(), "nothing was wrong");
        assert!(loaded.settings.status.offline_by_default);
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tmp();
        let path = dir.path().join("settings.json");

        let mut written = Settings::default();
        written.set("status.hide_offline_note", "true").unwrap();
        save_to(&written, &path).unwrap();

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.settings, written);
        assert!(loaded.problems.is_empty());
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

    /// The point of the tolerant loader: one unreadable value costs that setting and not
    /// the others, and the substitution is reported rather than silent.
    #[test]
    fn a_bad_value_falls_back_to_the_default_and_says_so() {
        let loaded =
            load_text(r#"{"status":{"offline_by_default":"yes please","hide_offline_note":true}}"#);

        assert!(
            loaded.settings.status.offline_by_default,
            "the unreadable key must fall back to the built-in default"
        );
        assert!(
            loaded.settings.status.hide_offline_note,
            "a broken sibling must not cost the readable setting"
        );

        assert_eq!(loaded.problems.len(), 1, "got {:?}", loaded.problems);
        assert_eq!(
            loaded.problems[0].key.as_deref(),
            Some("status.offline_by_default")
        );
        assert_eq!(loaded.problems[0].found, "yes please");
        assert_eq!(loaded.problems[0].used, "true");

        let warning = &loaded.warnings()[0];
        assert!(
            warning.contains("status.offline_by_default"),
            "got: {warning}"
        );
        assert!(warning.contains("yes please"), "got: {warning}");
    }

    /// A document that will not parse costs every setting, and says so with a position.
    #[test]
    fn unparseable_document_falls_back_to_all_defaults_and_reports_it() {
        let loaded = load_text("{ not json");
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.problems.len(), 1);
        assert_eq!(loaded.problems[0].key, None, "a document-level failure");
        let warning = &loaded.warnings()[0];
        assert!(warning.contains("all defaults"), "got: {warning}");
    }

    /// A section present but not an object makes its keys unreachable. Reported once for
    /// the section, not once per key.
    #[test]
    fn a_section_of_the_wrong_shape_is_reported_once() {
        let loaded = load_text(r#"{"status":"true"}"#);
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.problems.len(), 1, "got {:?}", loaded.problems);
        assert_eq!(loaded.problems[0].key.as_deref(), Some("status"));
    }

    /// A key that is simply absent is not a problem — that is the normal state of a partial
    /// file, and warning about it would be noise on every run.
    #[test]
    fn an_absent_key_is_not_reported() {
        let loaded = load_text(r#"{"status":{"hide_offline_note":true}}"#);
        assert!(loaded.settings.status.hide_offline_note);
        assert!(loaded.problems.is_empty(), "got {:?}", loaded.problems);
    }

    /// Keys we do not know about are left alone rather than reported: a file written by a
    /// newer version should still load its known keys quietly.
    #[test]
    fn unknown_keys_are_ignored_without_complaint() {
        let loaded = load_text(r#"{"status":{"future_flag":true},"extra":1}"#);
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.problems.is_empty(), "got {:?}", loaded.problems);
    }

    /// Booleans written as the strings a user might type are still accepted, because the
    /// setter accepts them.
    #[test]
    fn string_spellings_of_a_boolean_are_accepted() {
        let loaded = load_text(r#"{"status":{"hide_offline_note":"yes"}}"#);
        assert!(loaded.settings.status.hide_offline_note);
        assert!(loaded.problems.is_empty(), "got {:?}", loaded.problems);
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
