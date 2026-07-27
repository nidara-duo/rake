#[cfg(test)]
mod tests {
    use rake_domain::package::InstallRecord;

    /// Scoop's `manifest()` function (lib/manifest.ps1) gives `url` precedence
    /// over `bucket` — if `install.json` has a non-null `url`, Scoop tries to
    /// fetch and parse that URL as a manifest JSON, which breaks for binary
    /// asset URLs that rake previously stored here.
    ///
    /// For bucket-sourced packages, `url` MUST be null/absent so Scoop falls
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
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["bucket"], "main");
        match value.get("url") {
            None => {} // absent — equally valid as null
            Some(v) => assert!(
                v.is_null(),
                "expected null/absent url for bucket install, got: {v}"
            ),
        }
    }
}
