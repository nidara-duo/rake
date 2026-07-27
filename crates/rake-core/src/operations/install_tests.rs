#[cfg(test)]
mod tests {
    use rake_domain::package::InstallRecord;

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
