//! User preferences, as opposed to [`crate::config`] which is the resolved operational
//! configuration.
//!
//! The distinction matters: `Config` answers "where do packages live" and is derived from
//! the environment, while `Settings` answers "how should this tool behave for this user".
//! Settings therefore live in their own file (`~/.config/rake/settings.json`) rather than
//! in Scoop's, whose `set_config` rewrites the whole document and would drop anything we
//! put there.
//!
//! Every field carries a default that matches current behaviour, so an absent file and an
//! empty file both mean "nothing changed". That is what makes the file optional.

use serde::{Deserialize, Serialize};

/// `status.offline_by_default` — what `rake status` does when no flag is given.
///
/// Defaults to `true`, which is the behaviour the command has had since bucket freshness
/// stopped being fetched on every run.
pub const DEFAULT_OFFLINE_BY_DEFAULT: bool = true;

/// `status.hide_offline_note` — whether the "Bucket state is from the last …" line is
/// printed without `-q`.
///
/// Defaults to `false`, so the note shows; a user who finds it noisy sets this to `true`
/// rather than having to type `-q` forever.
pub const DEFAULT_HIDE_OFFLINE_NOTE: bool = false;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub status: StatusSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StatusSettings {
    /// Run without touching the network unless asked.
    pub offline_by_default: bool,
    /// Suppress the note explaining that the bucket verdict was not re-verified.
    pub hide_offline_note: bool,
}

impl Default for StatusSettings {
    /// Matches the shipped behaviour exactly, so a missing `status` block changes
    /// nothing. Written by hand rather than derived, because `#[derive(Default)]` would
    /// give `false` and silently flip `offline_by_default`.
    fn default() -> Self {
        Self {
            offline_by_default: DEFAULT_OFFLINE_BY_DEFAULT,
            hide_offline_note: DEFAULT_HIDE_OFFLINE_NOTE,
        }
    }
}

impl Settings {
    /// Every settable key with its current value and default, for `rake settings`.
    ///
    /// One list rather than a reflection over the struct, so a key that is documented but
    /// not wired up cannot drift silently.
    pub fn entries(&self) -> Vec<(&'static str, String, String, bool)> {
        vec![
            (
                "status.offline_by_default",
                render_bool(self.status.offline_by_default),
                render_bool(DEFAULT_OFFLINE_BY_DEFAULT),
                self.status.offline_by_default != DEFAULT_OFFLINE_BY_DEFAULT,
            ),
            (
                "status.hide_offline_note",
                render_bool(self.status.hide_offline_note),
                render_bool(DEFAULT_HIDE_OFFLINE_NOTE),
                self.status.hide_offline_note != DEFAULT_HIDE_OFFLINE_NOTE,
            ),
        ]
    }

    /// Dotted keys, for error messages and completion.
    pub fn keys() -> [&'static str; 2] {
        ["status.offline_by_default", "status.hide_offline_note"]
    }

    /// Read one key as the string a user would type back.
    pub fn get(&self, key: &str) -> Option<String> {
        match key {
            "status.offline_by_default" => Some(render_bool(self.status.offline_by_default)),
            "status.hide_offline_note" => Some(render_bool(self.status.hide_offline_note)),
            _ => None,
        }
    }

    /// Set one key from a string.
    ///
    /// Returns `Err` for an unknown key or an unparseable value, with the list of valid
    /// keys — a typo must not be silently ignored, and neither must `"yes"`.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), SettingsError> {
        let parsed = parse_bool(key, value)?;
        match key {
            "status.offline_by_default" => self.status.offline_by_default = parsed,
            "status.hide_offline_note" => self.status.hide_offline_note = parsed,
            _ => return Err(SettingsError::UnknownKey(key.to_owned())),
        }
        Ok(())
    }

    /// Restore one key to its default.
    pub fn reset(&mut self, key: &str) -> Result<(), SettingsError> {
        match key {
            "status.offline_by_default" => {
                self.status.offline_by_default = DEFAULT_OFFLINE_BY_DEFAULT
            }
            "status.hide_offline_note" => self.status.hide_offline_note = DEFAULT_HIDE_OFFLINE_NOTE,
            _ => return Err(SettingsError::UnknownKey(key.to_owned())),
        }
        Ok(())
    }
}

fn render_bool(value: bool) -> String {
    if value { "true" } else { "false" }.to_owned()
}

/// Accept what a user would plausibly type. `scoop config` passes the string straight
/// through and lets PowerShell coerce it, which means `shim = maybe` becomes the literal
/// string "maybe" and fails much later; here a bad value is rejected at the point of
/// setting.
fn parse_bool(key: &str, value: &str) -> Result<bool, SettingsError> {
    match value.trim().to_ascii_lowercase().as_str() {
        v @ ("true" | "yes" | "on" | "1") => Ok(v == "true" || v == "yes" || v == "on" || v == "1"),
        v @ ("false" | "no" | "off" | "0") => Ok(!matches!(v, "false" | "no" | "off" | "0")),
        _ => Err(SettingsError::BadValue {
            key: key.to_owned(),
            value: value.to_owned(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    UnknownKey(String),
    BadValue { key: String, value: String },
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::UnknownKey(k) => write!(
                f,
                "unknown setting '{k}'. Known settings: {}",
                Settings::keys().join(", ")
            ),
            SettingsError::BadValue { key, value } => write!(
                f,
                "'{value}' is not a boolean for '{key}'. Use true or false."
            ),
        }
    }
}

impl std::error::Error for SettingsError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file is optional: an absent one must mean "nothing changed". The defaults are
    /// the behaviour the command already had, so a fresh install is unaffected.
    #[test]
    fn defaults_match_shipped_behaviour() {
        let s = Settings::default();
        assert!(s.status.offline_by_default);
        assert!(!s.status.hide_offline_note);
    }

    /// An empty document deserialises to the defaults, so a hand-created `{}` does not
    /// flip `offline_by_default` to `false` and start hitting the network on every run.
    #[test]
    fn empty_json_yields_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.status.offline_by_default);
        assert!(!s.status.hide_offline_note);
    }

    /// A partial document keeps the defaults for what it does not mention.
    #[test]
    fn partial_document_keeps_other_defaults() {
        let s: Settings = serde_json::from_str(r#"{"status":{"hide_offline_note":true}}"#).unwrap();
        assert!(
            s.status.hide_offline_note,
            "the stated key should take effect"
        );
        assert!(
            s.status.offline_by_default,
            "the unstated key must keep its default"
        );
    }

    /// An unknown key must not fail the whole file — a settings file written by a newer
    /// version should still load its known keys.
    #[test]
    fn unknown_keys_are_ignored_on_load() {
        let s: Settings =
            serde_json::from_str(r#"{"status":{"future_flag":true},"extra":1}"#).unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn round_trips_through_json() {
        let mut s = Settings::default();
        s.set("status.hide_offline_note", "true").unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
    }

    #[test]
    fn set_and_get_agree() {
        let mut s = Settings::default();
        s.set("status.offline_by_default", "false").unwrap();
        assert!(!s.status.offline_by_default);
        assert_eq!(s.get("status.offline_by_default").as_deref(), Some("false"));
    }

    /// Spelling accepted, because these are the forms people actually type.
    #[test]
    fn accepts_the_usual_spellings_of_a_boolean() {
        let mut s = Settings::default();
        for (text, expected) in [
            ("true", true),
            ("TRUE", true),
            ("True", true),
            ("yes", true),
            ("on", true),
            ("1", true),
            ("false", false),
            ("no", false),
            ("off", false),
            ("0", false),
        ] {
            s.set("status.hide_offline_note", text).unwrap();
            assert_eq!(s.status.hide_offline_note, expected, "value {text:?}");
        }
    }

    /// Unlike Scoop, a value that is not a boolean is refused here rather than stored as
    /// a string and failing later.
    #[test]
    fn refuses_a_value_that_is_not_a_boolean() {
        let mut s = Settings::default();
        for junk in ["maybe", "", "tru", "2"] {
            assert!(
                s.set("status.hide_offline_note", junk).is_err(),
                "{junk:?} should be rejected"
            );
        }
        assert!(
            !s.status.hide_offline_note,
            "a rejected value must leave the setting alone"
        );
    }

    #[test]
    fn refuses_an_unknown_key_and_names_the_real_ones() {
        let mut s = Settings::default();
        let err = s.set("status.nope", "true").unwrap_err().to_string();
        assert!(err.contains("status.nope"), "got: {err}");
        assert!(err.contains("status.offline_by_default"), "got: {err}");
        assert!(s.reset("status.nope").is_err());
        assert!(s.get("status.nope").is_none());
    }

    #[test]
    fn reset_restores_the_default() {
        let mut s = Settings::default();
        s.set("status.offline_by_default", "false").unwrap();
        s.reset("status.offline_by_default").unwrap();
        assert!(s.status.offline_by_default);

        s.set("status.hide_offline_note", "true").unwrap();
        s.reset("status.hide_offline_note").unwrap();
        assert!(!s.status.hide_offline_note);
    }

    /// `entries` feeds `rake settings`, so the default column and the changed marker have
    /// to be right — that is the whole point of the display.
    #[test]
    fn entries_report_value_default_and_whether_it_changed() {
        let mut s = Settings::default();
        assert!(s.entries().iter().all(|e| !e.3), "nothing changed yet");

        s.set("status.hide_offline_note", "true").unwrap();
        let changed: Vec<_> = s.entries().iter().filter(|e| e.3).map(|e| e.0).collect();
        assert_eq!(changed, vec!["status.hide_offline_note"]);

        let offline = s
            .entries()
            .into_iter()
            .find(|e| e.0 == "status.offline_by_default")
            .unwrap();
        assert_eq!((offline.1.as_str(), offline.2.as_str()), ("true", "true"));
    }

    /// The key list and the accessor list must not drift apart, or a documented key would
    /// not be settable.
    #[test]
    fn every_advertised_key_is_gettable_and_settable() {
        let mut s = Settings::default();
        for key in Settings::keys() {
            assert!(s.get(key).is_some(), "{key} is advertised but unreadable");
            s.set(key, "true").unwrap();
            s.reset(key).unwrap();
        }
        assert_eq!(s, Settings::default());
    }
}
