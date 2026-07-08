#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_cleanup_removes_old_versions() {
        let dir = tempdir().unwrap();
        let apps_root = dir.path().join("apps");
        std::fs::create_dir_all(&apps_root).unwrap();
    }
}
