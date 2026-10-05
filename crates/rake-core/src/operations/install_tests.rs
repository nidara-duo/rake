#[cfg(test)]
mod tests {
    use rake_domain::package::InstallRecord;

    use super::super::install::{is_in_dir, resolve_env_add_paths};
    use rake_domain::arch::Arch;
    use std::path::{Path, PathBuf};

    fn manifest_with_env_add_path(json: &str) -> rake_domain::manifest::Manifest {
        serde_json::from_str(json).expect("manifest should parse")
    }

    /// `env_add_path` scoped to an architecture.
    ///
    /// Built through `serde_json::json!` rather than a `format!` template: a Windows
    /// separator is not a valid JSON escape, so interpolating one by hand yields a
    /// parse error instead of the case under test. The first version of these tests
    /// failed exactly that way.
    fn manifest_arch_env_add_path(arch_key: &str, entry: &str) -> rake_domain::manifest::Manifest {
        let json = serde_json::json!({
            "version": "1",
            "architecture": { arch_key: { "env_add_path": entry } }
        });
        serde_json::from_value(json).expect("manifest should parse")
    }

    fn app_dir() -> PathBuf {
        PathBuf::from(r"C:\rake\apps\demo\current")
    }

    /// The ordinary case: a relative name is joined onto the app directory and kept.
    #[test]
    fn env_add_path_resolves_relative_names_under_the_app_directory() {
        let m = manifest_with_env_add_path(r#"{"version":"1","env_add_path":"bin"}"#);
        let resolved = resolve_env_add_paths(&m, Arch::Amd64, &app_dir());
        assert_eq!(
            resolved,
            vec![PathBuf::from(r"C:\rake\apps\demo\current\bin")]
        );
    }

    #[test]
    fn env_add_path_accepts_several_entries() {
        let m =
            manifest_with_env_add_path(r#"{"version":"1","env_add_path":["bin","lib","share"]}"#);
        let resolved = resolve_env_add_paths(&m, Arch::Amd64, &app_dir());
        assert_eq!(resolved.len(), 3);
        assert!(resolved.iter().all(|p| p.starts_with(app_dir())));
    }

    /// The hole this closes. `is_in_dir` is a *string* prefix test, and a joined path
    /// with unresolved `..` still starts with the base directory, so these entries pass
    /// it — while resolving to somewhere else entirely.
    ///
    /// Verified that the prefix really does match before the fix, so the test is not
    /// asserting the guard is necessary when it is not:
    ///   join  = C:\rake\apps\demo\current\..\..\..\..\Startup\pwned
    ///   StartsWith(dir) = True
    ///   resolves to      = C:\Startup\pwned
    ///
    /// A PATH entry is persistent and user-global, so this outlives the install.
    #[test]
    fn env_add_path_refuses_names_escaping_the_app_directory() {
        for evil in [
            r"..\..\..\..\Startup\pwned",
            r"..\..\shims",
            r"..\..\..\..\Windows\System32",
            "../evil",
            r"sub\..\..\escape",
        ] {
            let m = manifest_arch_env_add_path("64bit", evil);
            let resolved = resolve_env_add_paths(&m, Arch::Amd64, &app_dir());
            assert!(
                resolved.is_empty(),
                "{evil:?} must not become a PATH entry, got {resolved:?}"
            );
        }
    }

    #[test]
    fn env_add_path_refuses_absolute_names() {
        for evil in [r"C:\Windows\System32", r"\absolute\evil", "/etc/passwd"] {
            let m = manifest_arch_env_add_path("64bit", evil);
            assert!(
                resolve_env_add_paths(&m, Arch::Amd64, &app_dir()).is_empty(),
                "{evil:?} must be refused"
            );
        }
    }

    /// A refusal must not take the legitimate entries down with it.
    #[test]
    fn a_bad_entry_does_not_discard_the_good_ones() {
        let json =
            serde_json::json!({"version": "1", "env_add_path": ["bin", "..\\..\\escape", "lib"]});
        let m: rake_domain::manifest::Manifest = serde_json::from_value(json).unwrap();
        let resolved = resolve_env_add_paths(&m, Arch::Amd64, &app_dir());
        assert_eq!(
            resolved,
            vec![
                PathBuf::from(r"C:\rake\apps\demo\current\bin"),
                PathBuf::from(r"C:\rake\apps\demo\current\lib"),
            ]
        );
    }

    /// Nested relative names are legitimate and must keep working.
    #[test]
    fn nested_relative_names_are_allowed() {
        let m =
            manifest_with_env_add_path(r#"{"version":"1","env_add_path":["share\\bin","a/b"]}"#);
        let resolved = resolve_env_add_paths(&m, Arch::Amd64, &app_dir());
        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|p| p.starts_with(app_dir())));
    }

    #[test]
    fn blank_entries_are_skipped() {
        let m = manifest_with_env_add_path(r#"{"version":"1","env_add_path":["bin","","  "]}"#);
        assert_eq!(
            resolve_env_add_paths(&m, Arch::Amd64, &app_dir()),
            vec![PathBuf::from(r"C:\rake\apps\demo\current\bin")]
        );
    }

    #[test]
    fn no_env_add_path_yields_nothing() {
        let m = manifest_with_env_add_path(r#"{"version":"1"}"#);
        assert!(resolve_env_add_paths(&m, Arch::Amd64, &app_dir()).is_empty());
    }

    /// `is_in_dir` itself, pinned including the boundary case that a naive prefix test
    /// gets wrong. It mirrors scoop's `is_in_dir` (lib/core.ps1:695).
    #[test]
    fn is_in_dir_checks_the_separator_boundary() {
        let dir = Path::new(r"C:\apps\b");
        assert!(is_in_dir(dir, Path::new(r"C:\apps\b")), "the dir itself");
        assert!(
            is_in_dir(dir, Path::new(r"C:\apps\b\bc")),
            "a real child matches"
        );
        assert!(
            !is_in_dir(dir, Path::new(r"C:\apps\bc")),
            "C:\\apps\\bc must not count as inside C:\\apps\\b — this is the \
             sibling-prefix case the trailing separator exists for"
        );
        assert!(is_in_dir(dir, Path::new(r"C:\Apps\B")), "case-insensitive");
        assert!(!is_in_dir(dir, Path::new(r"C:\apps")), "the parent");
    }

    /// Scoop's `manifest()` function (lib/manifest.ps1) gives `url` precedence
    /// over `bucket` — if `install.json` has a non-null `url`, Scoop tries to
    /// fetch and parse that URL as a manifest JSON, which breaks for binary
    /// asset URLs that rake previously stored here.
    ///
    /// For bucket-sourced packages, `url` MUST be absent so Scoop falls
    /// back to reading the manifest from the local bucket directory.
    #[test]
    fn bucket_install_writes_null_url_for_scoop_compat() {
        let record = InstallRecord {
            version: "1.0.0".to_owned(),
            bucket: Some("main".to_owned()),
            arch: "64bit".to_owned(),
            held: false,
            url: None,
        };

        let json = serde_json::to_string_pretty(&record).unwrap();

        // Must match Scoop's output exactly — no "url" key, no null values.
        // Scoop uses "architecture" (not "arch") and "hold" (not "held").
        let expected = r#"{
  "version": "1.0.0",
  "bucket": "main",
  "architecture": "64bit",
  "hold": false
}"#;
        assert_eq!(
            json, expected,
            "install.json format must match expected Scoop-compatible output"
        );

        // Verify round-trip: deserialise back must produce the same record.
        let deserialized: InstallRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.version, record.version);
        assert_eq!(deserialized.bucket, record.bucket);
        assert_eq!(deserialized.arch, record.arch);
        assert_eq!(deserialized.held, record.held);
        assert_eq!(deserialized.url, record.url);
    }

    /// URL-sourced installs SHOULD have a non-null url in install.json.
    #[test]
    fn url_install_writes_url_field() {
        let record = InstallRecord {
            version: "2.0.0".to_owned(),
            bucket: None,
            arch: "32bit".to_owned(),
            held: true,
            url: Some("https://example.com/manifest.json".to_owned()),
        };

        let json = serde_json::to_string_pretty(&record).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["url"], "https://example.com/manifest.json");
        assert!(value.get("bucket").is_none() || value["bucket"].is_null());
    }

    /// Scoop reads `$install.architecture` (not `arch`) in uninstall and
    /// other libexec scripts.  rake must write the key that Scoop expects.
    #[test]
    fn serializes_architecture_not_arch() {
        let record = InstallRecord {
            version: "4.0.0".to_owned(),
            bucket: Some("main".to_owned()),
            arch: "32bit".to_owned(),
            held: false,
            url: None,
        };

        let json = serde_json::to_string_pretty(&record).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(
            value.as_object().unwrap().contains_key("architecture"),
            "install.json must use 'architecture' key (Scoop compat), got: {json}"
        );
        assert!(
            !value.as_object().unwrap().contains_key("arch"),
            "install.json must NOT use 'arch' key, got: {json}"
        );
        assert_eq!(value["architecture"], "32bit");
    }

    /// Scoop reads `$install.hold` (not `held`) in hold/unhold/list/status.
    /// rake must write the key that Scoop expects.
    #[test]
    fn serializes_hold_not_held() {
        let record = InstallRecord {
            version: "5.0.0".to_owned(),
            bucket: Some("main".to_owned()),
            arch: "64bit".to_owned(),
            held: true,
            url: None,
        };

        let json = serde_json::to_string_pretty(&record).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(
            value.as_object().unwrap().contains_key("hold"),
            "install.json must use 'hold' key (Scoop compat), got: {json}"
        );
        assert!(
            !value.as_object().unwrap().contains_key("held"),
            "install.json must NOT use 'held' key, got: {json}"
        );
        assert_eq!(value["hold"], true);
    }

    /// serde must never emit `"url": null`.  Scoop distinguishes between
    /// absent fields and null values — null is NOT valid Scoop input.
    #[test]
    fn null_url_is_absent_not_null() {
        let record = InstallRecord {
            version: "3.0.0".to_owned(),
            bucket: Some("main".to_owned()),
            arch: "64bit".to_owned(),
            held: false,
            url: None,
        };

        let json = serde_json::to_string_pretty(&record).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        // url must be absent from the JSON object entirely
        assert!(
            !value.as_object().unwrap().contains_key("url"),
            "url field must be absent (not null) for bucket installs, got JSON: {json}"
        );
        // bucket must still be present
        assert!(value.as_object().unwrap().contains_key("bucket"));
    }
}
