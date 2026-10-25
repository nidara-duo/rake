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

/// End-to-end staging tests: a real package, a real (local) cache, a real
/// version directory. No network, no PowerShell hooks, no shims — only the part
/// Iteration 3 rewrote.
#[cfg(test)]
mod staging_tests {
    use std::io::Write;
    use std::path::Path;

    use rake_domain::arch::Arch;
    use rake_domain::config::Config;
    use rake_domain::manifest::Manifest;
    use rake_domain::package::{Package, PackageIdent, PackageStatus};

    use crate::operations::download::DownloadedFile;
    use crate::operations::install::install_packages;
    use crate::session::Session;

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
        }
        writer.finish().unwrap();
    }

    fn manifest(json: serde_json::Value) -> Manifest {
        serde_json::from_value(json).expect("manifest should parse")
    }

    fn downloaded(
        ident: &PackageIdent,
        cache_path: &Path,
        target_filename: &str,
    ) -> DownloadedFile {
        DownloadedFile {
            ident: ident.clone(),
            url: format!("https://example.com/{target_filename}"),
            cache_path: cache_path.to_path_buf(),
            target_filename: target_filename.to_owned(),
            verified: true,
        }
    }

    /// The acceptance case: one archive with a nested `extract_dir`, plus a
    /// single file renamed by a `#/` fragment, both landing in the same version
    /// directory.
    #[tokio::test]
    async fn installs_an_archive_with_extract_dir_beside_a_renamed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scoop");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();

        let archive = cache.join("demo#1.2.3#app.zip");
        write_zip(
            &archive,
            &[
                ("nested/inner/tool.exe", b"tool"),
                ("nested/inner/share/data.txt", b"data"),
            ],
        );
        let single = cache.join("demo#1.2.3#helper.exe");
        std::fs::write(&single, b"helper").unwrap();

        let ident = PackageIdent::new("main", "demo");
        let pkg = Package::new(
            ident.clone(),
            manifest(serde_json::json!({
                "version": "1.2.3",
                "url": [
                    "https://example.com/app.zip",
                    "https://example.com/helper.exe#/renamed.exe"
                ],
                "extract_dir": "nested/inner"
            })),
            None,
            PackageStatus::NotInstalled,
        );

        let files = vec![
            downloaded(&ident, &archive, "app.zip"),
            downloaded(&ident, &single, "renamed.exe"),
        ];

        let session = Session::from_config(Config {
            root_path: Some(root.clone()),
            ..Default::default()
        });

        let installed = install_packages(&session, &[pkg], &files, Arch::Amd64)
            .await
            .expect("install should succeed");

        let version_dir = root.join("apps").join("demo").join("1.2.3");

        assert_eq!(installed.len(), 1);
        assert_eq!(
            std::fs::read(version_dir.join("tool.exe")).unwrap(),
            b"tool",
            "the contents of extract_dir must be moved to the version root"
        );
        assert_eq!(
            std::fs::read(version_dir.join("share").join("data.txt")).unwrap(),
            b"data"
        );
        assert_eq!(
            std::fs::read(version_dir.join("renamed.exe")).unwrap(),
            b"helper",
            "a `#/name` fragment decides the file name on disk"
        );

        assert!(
            !version_dir.join("app.zip").exists(),
            "the staged archive must be removed after extraction (Scoop's -Removal)"
        );
        assert!(!version_dir.join("helper.exe").exists());
        assert!(!version_dir.join("nested").exists());
        assert!(!version_dir.join("_tmp").exists());

        assert!(version_dir.join("scoop-install.json").is_file());
        assert!(version_dir.join("scoop-manifest.json").is_file());
        assert!(
            root.join("apps").join("demo").join("current").exists(),
            "the current junction must exist after a successful install"
        );
    }

    /// `extract_to` is the destination root for extraction; manifests write it
    /// with backslashes (`unxutils.json` uses `usr\local\wbin`).
    #[tokio::test]
    async fn extract_to_places_archive_contents_in_a_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scoop");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();

        let archive = cache.join("lib#1.0#lib.zip");
        write_zip(&archive, &[("tool.exe", b"tool")]);

        let ident = PackageIdent::new("main", "lib");
        let pkg = Package::new(
            ident.clone(),
            manifest(serde_json::json!({
                "version": "1.0",
                "url": "https://example.com/lib.zip",
                "extract_to": r"usr\local\wbin"
            })),
            None,
            PackageStatus::NotInstalled,
        );

        let session = Session::from_config(Config {
            root_path: Some(root.clone()),
            ..Default::default()
        });

        install_packages(
            &session,
            &[pkg],
            &[downloaded(&ident, &archive, "lib.zip")],
            Arch::Amd64,
        )
        .await
        .expect("install should succeed");

        let version_dir = root.join("apps").join("lib").join("1.0");
        assert_eq!(
            std::fs::read(
                version_dir
                    .join("usr")
                    .join("local")
                    .join("wbin")
                    .join("tool.exe")
            )
            .unwrap(),
            b"tool"
        );
        assert!(!version_dir.join("tool.exe").exists());
    }

    /// The architecture override has to reach the install stage, not just the
    /// manifest API: a 64-bit package whose `extract_dir` differs from the
    /// top-level one must use the 64-bit value.
    #[tokio::test]
    async fn install_uses_the_architecture_specific_extract_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scoop");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();

        let archive = cache.join("demo#2.0#demo.zip");
        write_zip(
            &archive,
            &[("arch/tool.exe", b"64bit"), ("top/other.exe", b"32bit")],
        );

        let ident = PackageIdent::new("main", "demo");
        let pkg = Package::new(
            ident.clone(),
            manifest(serde_json::json!({
                "version": "2.0",
                "url": "https://example.com/demo-32.zip",
                "extract_dir": "top",
                "architecture": {
                    "64bit": {
                        "url": "https://example.com/demo-64.zip",
                        "extract_dir": "arch"
                    }
                }
            })),
            None,
            PackageStatus::NotInstalled,
        );

        let session = Session::from_config(Config {
            root_path: Some(root.clone()),
            ..Default::default()
        });

        install_packages(
            &session,
            &[pkg],
            &[downloaded(&ident, &archive, "demo.zip")],
            Arch::Amd64,
        )
        .await
        .expect("install should succeed");

        let version_dir = root.join("apps").join("demo").join("2.0");
        assert_eq!(
            std::fs::read(version_dir.join("tool.exe")).unwrap(),
            b"64bit"
        );
        assert!(
            !version_dir.join("other.exe").exists(),
            "the top-level extract_dir must not be used when the arch overrides it"
        );
    }
}

/// End-to-end tests for the post-extraction lifecycle: shims, persist, update
/// and rollback. No network; every download is a local zip.
#[cfg(test)]
mod lifecycle_tests {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use rake_domain::arch::Arch;
    use rake_domain::config::Config;
    use rake_domain::manifest::Manifest;
    use rake_domain::package::{InstallState, Package, PackageIdent, PackageStatus};

    use crate::infra::install_meta;
    use crate::operations::download::DownloadedFile;
    use crate::operations::install::install_packages;
    use crate::operations::update::{UpdateSpec, update_packages};
    use crate::session::Session;

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    fn manifest(json: serde_json::Value) -> Manifest {
        serde_json::from_value(json).expect("manifest should parse")
    }

    fn downloaded(ident: &PackageIdent, cache_path: &Path, target: &str) -> DownloadedFile {
        DownloadedFile {
            ident: ident.clone(),
            url: format!("https://example.com/{target}"),
            cache_path: cache_path.to_path_buf(),
            target_filename: target.to_owned(),
            verified: true,
        }
    }

    /// A session whose env writes are recorded, never performed.
    fn session(root: &Path) -> Session {
        let (session, _recorder) = Session::from_config_recording_env(Config {
            root_path: Some(root.to_path_buf()),
            ..Default::default()
        });
        session
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{} should be readable: {e}", path.display()))
    }

    fn installed_package(ident: &PackageIdent, manifest: Manifest) -> Package {
        let version = manifest.version.clone();
        Package::new(
            ident.clone(),
            manifest,
            None,
            PackageStatus::Installed(InstallState {
                version,
                bucket: Some(ident.bucket.clone()),
                arch: "64bit".to_owned(),
                held: false,
                url: None,
            }),
        )
    }

    /// `bin` dispatch by **target** extension, as Scoop's `shim` does
    /// (lib/core.ps1:963): an `.exe` gets `shim.exe` + `.shim` metadata, a `.cmd`
    /// gets generated wrappers and no `shim.exe` at all. Arguments from the
    /// three-element form must reach the wrapper.
    #[tokio::test]
    async fn installs_exe_and_cmd_bins_with_alias_and_args() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scoop");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();

        let archive = cache.join("demo#1.0.0#demo.zip");
        write_zip(
            &archive,
            &[("tool.exe", b"MZ"), ("code.cmd", b"@echo off\r\n")],
        );

        let ident = PackageIdent::new("main", "demo");
        let pkg = Package::new(
            ident.clone(),
            manifest(serde_json::json!({
                "version": "1.0.0",
                "url": "https://example.com/demo.zip",
                "bin": [
                    ["tool.exe", "demo", "--flag"],
                    ["code.cmd", "code"]
                ]
            })),
            None,
            PackageStatus::NotInstalled,
        );

        install_packages(
            &session(&root),
            &[pkg],
            &[downloaded(&ident, &archive, "demo.zip")],
            Arch::Amd64,
        )
        .await
        .expect("install should succeed");

        let shims = root.join("shims");

        // .exe: shim.exe plus metadata, alias and arguments included.
        assert!(
            shims.join("demo.exe").is_file(),
            "shim.exe for an exe target"
        );
        let metadata = read(&shims.join("demo.shim"));
        assert!(
            metadata.contains("tool.exe") && metadata.contains("current"),
            "the shim must target the junction, not the version directory:\n{metadata}"
        );
        assert!(
            metadata.contains("args = --flag"),
            "the third bin element is the argument list:\n{metadata}"
        );

        // .cmd: generated wrappers, never a copied shim.exe.
        let cmd = read(&shims.join("code.cmd"));
        assert!(
            cmd.contains("code.cmd") && cmd.contains("%*"),
            "the .cmd wrapper must launch the target and forward arguments:\n{cmd}"
        );
        assert!(
            shims.join("code").is_file(),
            "the POSIX wrapper must exist as well"
        );
        assert!(
            !shims.join("code.exe").exists() && !shims.join("code.shim").exists(),
            "a batch target must not get shim.exe/.shim"
        );
    }

    /// Persist in all three manifest shapes — bare string, flat list and
    /// `[source, target]` pair — then an update that must keep every byte of the
    /// user's data, keep the old version directory, and carry `hold` over.
    #[tokio::test]
    async fn update_preserves_persisted_data_and_carries_hold() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scoop");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();

        let v1 = cache.join("demo#1.0.0#demo.zip");
        write_zip(
            &v1,
            &[
                ("config/default.cfg", b"v1-default"),
                ("settings.ini", b"v1-settings"),
                ("cache/placeholder.txt", b"v1-cache"),
            ],
        );
        let v2 = cache.join("demo#2.0.0#demo.zip");
        write_zip(
            &v2,
            &[
                ("config/default.cfg", b"v2-default"),
                ("settings.ini", b"v2-settings"),
                ("cache/placeholder.txt", b"v2-cache"),
            ],
        );

        let ident = PackageIdent::new("main", "demo");
        let manifest_v1 = manifest(serde_json::json!({
            "version": "1.0.0",
            "url": "https://example.com/demo.zip",
            "persist": ["config", "settings.ini", ["cache", "cache-data"]]
        }));
        let manifest_v2 = manifest(serde_json::json!({
            "version": "2.0.0",
            "url": "https://example.com/demo.zip",
            "persist": ["config", "settings.ini", ["cache", "cache-data"]]
        }));

        let session = session(&root);
        let v1_pkg = Package::new(
            ident.clone(),
            manifest_v1.clone(),
            None,
            PackageStatus::NotInstalled,
        );
        install_packages(
            &session,
            &[v1_pkg],
            &[downloaded(&ident, &v1, "demo.zip")],
            Arch::Amd64,
        )
        .await
        .expect("v1 install should succeed");

        let app_dir = root.join("apps").join("demo");
        let old_version_dir = app_dir.join("1.0.0");
        let new_version_dir = app_dir.join("2.0.0");
        let persist_dir = root.join("persist").join("demo");

        // What the application writes once it runs.
        std::fs::write(persist_dir.join("config").join("user.cfg"), b"mine").unwrap();
        std::fs::write(persist_dir.join("settings.ini"), b"mine").unwrap();
        std::fs::write(persist_dir.join("cache-data").join("user.cache"), b"mine").unwrap();

        // The old version is held; the flag has to survive the update.
        let mut record = install_meta::read_install_record(&old_version_dir)
            .unwrap()
            .expect("v1 install record");
        record.held = true;
        install_meta::write_install_record(&old_version_dir, &record).unwrap();

        let updated = update_packages(
            &session,
            &[UpdateSpec {
                installed: installed_package(&ident, manifest_v1.clone()),
                candidate: Package::new(
                    ident.clone(),
                    manifest_v2.clone(),
                    None,
                    PackageStatus::NotInstalled,
                ),
                arch: Arch::Amd64,
                downloaded: vec![downloaded(&ident, &v2, "demo.zip")],
            }],
        )
        .await
        .expect("update should succeed");

        assert_eq!(updated.len(), 1, "the package must be reported as updated");
        assert!(
            updated[0].status.is_held(),
            "hold must be carried over from the old install.json"
        );

        // The user's data, untouched.
        assert_eq!(
            std::fs::read(persist_dir.join("config").join("user.cfg")).unwrap(),
            b"mine",
            "the persisted directory must survive the update"
        );
        assert_eq!(
            std::fs::read(persist_dir.join("settings.ini")).unwrap(),
            b"mine",
            "the persisted file must survive the update"
        );
        assert_eq!(
            std::fs::read(persist_dir.join("cache-data").join("user.cache")).unwrap(),
            b"mine",
            "the renamed persist target must survive the update"
        );

        // And it is reachable through the new version's junction.
        assert_eq!(
            std::fs::read(app_dir.join("current").join("config").join("user.cfg")).unwrap(),
            b"mine",
            "the new version must be linked to the persisted data"
        );

        // The shipped defaults are set aside, not merged into user data.
        assert_eq!(
            std::fs::read(new_version_dir.join("config.original").join("default.cfg")).unwrap(),
            b"v2-default"
        );
        assert_eq!(
            std::fs::read(persist_dir.join("config").join("default.cfg")).unwrap(),
            b"v1-default",
            "the new version's shipped default must not overwrite the persisted copy"
        );

        // The old version directory is kept, with its metadata.
        assert!(
            old_version_dir.join("scoop-install.json").is_file(),
            "the old version directory must be left in place"
        );
        assert_eq!(
            install_meta::read_install_record(&old_version_dir)
                .unwrap()
                .unwrap()
                .version,
            "1.0.0"
        );

        // The new version is the active one and carries the hold flag.
        assert!(new_version_dir.join("scoop-install.json").is_file());
        let new_record = install_meta::read_install_record(&new_version_dir)
            .unwrap()
            .expect("v2 install record");
        assert_eq!(new_record.version, "2.0.0");
        assert!(
            new_record.held,
            "hold must be written to the new install.json"
        );
    }

    /// A failure after the version directory is populated must leave nothing
    /// behind: no `install.json`, no `current` junction, no shims — and the
    /// persisted data must not be collateral damage.
    #[tokio::test]
    async fn a_failed_post_install_rolls_back_and_writes_no_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scoop");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();

        let archive = cache.join("demo#1.0.0#demo.zip");
        write_zip(
            &archive,
            &[("tool.exe", b"MZ"), ("config/default.cfg", b"d")],
        );

        let ident = PackageIdent::new("main", "demo");
        let pkg = Package::new(
            ident.clone(),
            manifest(serde_json::json!({
                "version": "1.0.0",
                "url": "https://example.com/demo.zip",
                "bin": "tool.exe",
                "persist": "config",
                "post_install": "exit 1"
            })),
            None,
            PackageStatus::NotInstalled,
        );

        let result = install_packages(
            &session(&root),
            &[pkg],
            &[downloaded(&ident, &archive, "demo.zip")],
            Arch::Amd64,
        )
        .await;

        assert!(
            result.is_err(),
            "a failing post_install must fail the install"
        );

        let app_dir = root.join("apps").join("demo");
        let version_dir = app_dir.join("1.0.0");

        assert!(
            !version_dir.join("scoop-install.json").exists()
                && !version_dir.join("install.json").exists(),
            "install.json must never be written for a failed install"
        );
        assert!(
            !version_dir.exists(),
            "the half-installed version directory must be removed"
        );
        assert!(
            !app_dir.join("current").exists(),
            "the current junction must not survive a failed install"
        );
        assert!(
            !root.join("shims").join("tool.exe").exists()
                && !root.join("shims").join("tool.shim").exists(),
            "shims created by the failed install must be removed"
        );
        assert_eq!(
            std::fs::read(
                root.join("persist")
                    .join("demo")
                    .join("config")
                    .join("default.cfg")
            )
            .unwrap(),
            b"d",
            "rolling back must unlink persist, never delete the stored data"
        );
    }

    /// `installer.file` runs with `$dir`/`$version` substituted and is deleted
    /// afterwards unless `keep` is set (lib/install.ps1:106-138). A non-zero
    /// exit fails the install.
    #[tokio::test]
    async fn installer_file_runs_with_substituted_args_and_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scoop");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();

        // A batch installer that records its arguments and succeeds.
        let installer = cache.join("demo#1.0.0#setup.bat");
        std::fs::write(
            &installer,
            b"@echo off\r\necho %1^|%2 > \"%~dp0args.txt\"\r\nexit /b 0\r\n",
        )
        .unwrap();

        let ident = PackageIdent::new("main", "demo");
        let pkg = Package::new(
            ident.clone(),
            manifest(serde_json::json!({
                "version": "1.0.0",
                "url": "https://example.com/setup.bat",
                "installer": { "args": ["$dir", "$version"] }
            })),
            None,
            PackageStatus::NotInstalled,
        );

        install_packages(
            &session(&root),
            &[pkg],
            &[downloaded(&ident, &installer, "setup.bat")],
            Arch::Amd64,
        )
        .await
        .expect("installer should succeed");

        let version_dir: PathBuf = root.join("apps").join("demo").join("1.0.0");
        let recorded = read(&version_dir.join("args.txt"));
        assert_eq!(
            recorded.trim_end(),
            format!("{}|1.0.0", version_dir.display()),
            "$dir and $version must be substituted before the installer runs"
        );
        assert!(
            !version_dir.join("setup.bat").exists(),
            "the installer is removed unless `keep` is true"
        );
    }
}
