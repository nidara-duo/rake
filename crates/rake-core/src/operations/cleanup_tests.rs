#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::tempdir;

    use rake_domain::config::Config;

    use crate::infra::fs::create_junction;
    use crate::operations::cleanup::cleanup_packages;
    use crate::session::Session;

    #[tokio::test]
    async fn test_cleanup_removes_old_versions() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let apps_root = root.join("apps");
        let app_dir = apps_root.join("demo");
        let v1_dir = app_dir.join("1.0.0");
        let v2_dir = app_dir.join("2.0.0");
        let current_link = app_dir.join("current");

        fs::create_dir_all(&v1_dir).unwrap();
        fs::create_dir_all(&v2_dir).unwrap();
        fs::write(v1_dir.join("manifest.json"), r#"{"version":"1.0.0"}"#).unwrap();
        fs::write(v2_dir.join("manifest.json"), r#"{"version":"2.0.0"}"#).unwrap();
        fs::write(v1_dir.join("app.exe"), b"v1 binary").unwrap();
        fs::write(v2_dir.join("app.exe"), b"v2 binary").unwrap();

        // Active version is 2.0.0
        create_junction(&v2_dir, &current_link).unwrap();

        let session = Session::from_config(Config {
            root_path: Some(root),
            ..Default::default()
        });

        let results = cleanup_packages(&session, &["demo".to_owned()], &[]).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "demo");
        assert_eq!(results[0].removed_versions, vec!["1.0.0".to_owned()]);
        assert!(results[0].failed_versions.is_empty());

        assert!(!v1_dir.exists(), "version 1.0.0 should be removed");
        assert!(v2_dir.exists(), "active version 2.0.0 should remain");
        assert!(current_link.exists(), "current junction should remain");
        assert!(current_link.join("app.exe").is_file());
    }
}
