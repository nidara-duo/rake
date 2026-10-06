//! Reading JSON files, tolerantly.
//!
//! Every JSON file Rake reads comes from outside the program: bucket manifests from a
//! git clone, `scoop-install.json` written by Scoop or Rake, `config.json` written by
//! Scoop. All of them may carry a UTF-8 BOM, because plenty of Windows tooling writes
//! one — older Notepad among them.
//!
//! `serde_json` rejects a BOM outright, and every one of these call sites handled that by
//! discarding the file, so a BOM'd manifest silently disappeared from `list` and `status`
//! while Scoop — which reads through .NET's `ReadAllLines` and strips the BOM — carried on
//! working. That is the same class of bug as the renamed metadata files: Scoop could read
//! something Rake could not, and Rake reported it as "not installed".
//!
//! Confirmed empirically rather than assumed: a `config.json` written with a BOM was
//! ignored, and the same file without one was honoured.
//!
//! The shim reader already stripped a BOM from `.shim` files for the same reason
//! (`crates/rake-shim-bin/src/main.rs:558`), so this brings the rest of the codebase in
//! line with behaviour that was already believed to be necessary.

use std::path::Path;

use serde::de::DeserializeOwned;

use crate::Result;

/// Remove a leading UTF-8 BOM, if present.
///
/// `serde_json` treats U+FEFF as an unexpected token rather than as whitespace.
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Read a JSON file, tolerating a UTF-8 BOM.
pub fn read_to_string(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)?;
    Ok(strip_bom(&raw).to_owned())
}

/// Read and parse a JSON file, tolerating a UTF-8 BOM.
///
/// The error carries the path, because "invalid type: map" without saying which of the
/// two metadata files it was is useless when an app is skipped.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = std::fs::read_to_string(path)?;
    let text = strip_bom(&raw);
    serde_json::from_str(text)
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("{}: {e}", path.display()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOM: &str = "\u{feff}";

    #[test]
    fn strip_bom_removes_only_the_leading_one() {
        assert_eq!(strip_bom(&format!("{BOM}{{\"a\":1}}")), "{\"a\":1}");
        assert_eq!(strip_bom("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(
            strip_bom(&format!("{BOM}{BOM}{{}}")),
            format!("{BOM}{{}}"),
            "only one is removed; a second would still break the parse"
        );
    }

    /// The regression this exists for: without the strip, `serde_json` refuses the file.
    #[test]
    fn parses_json_with_a_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        std::fs::write(&path, format!("{BOM}{{\"version\":\"1.0.0\"}}")).unwrap();

        let value: serde_json::Value = read(&path).unwrap();
        assert_eq!(value["version"], "1.0.0");
    }

    #[test]
    fn parses_json_without_a_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        std::fs::write(&path, r#"{"version":"1.0.0"}"#).unwrap();

        let value: serde_json::Value = read(&path).unwrap();
        assert_eq!(value["version"], "1.0.0");
    }

    #[test]
    fn read_to_string_also_strips_the_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw.json");
        std::fs::write(&path, format!("{BOM}hello")).unwrap();
        assert_eq!(read_to_string(&path).unwrap(), "hello");
    }

    /// A genuinely broken file must still fail, and the message must name the file —
    /// an app skipped by a bad manifest is otherwise invisible.
    #[test]
    fn invalid_json_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        std::fs::write(&path, "{not json").unwrap();

        let err = read::<serde_json::Value>(&path).unwrap_err().to_string();
        assert!(err.contains("broken.json"), "got: {err}");
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read::<serde_json::Value>(&dir.path().join("nope.json")).is_err());
    }

    /// A BOM before an array, which is what a `persist` or `bin` field looks like.
    #[test]
    fn parses_json_array_with_a_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("array.json");
        std::fs::write(&path, format!("{BOM}[\"git.exe\"]")).unwrap();

        let value: Vec<String> = read(&path).unwrap();
        assert_eq!(value, vec!["git.exe".to_owned()]);
    }
}
