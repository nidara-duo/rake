#[cfg(test)]
mod extraction_tests {
    use std::io::Write;
    use std::path::Path;

    use super::super::staging::{merge_move, normalize_manifest_dir};
    use super::super::tar::extract_tar;
    use super::super::zip::extract_zip;
    use super::super::{ArchiveFormat, NativeArchive, detect_format};

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap();
    }

    /// The ordinary case, so the guard tests below cannot pass by extraction simply
    /// being broken.
    #[test]
    fn extracts_a_zip_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("out");
        write_zip(&src, &[("app.exe", b"binary"), ("readme.txt", b"hello")]);

        extract_zip(&src, &dest, None, None).unwrap();

        assert_eq!(std::fs::read(dest.join("app.exe")).unwrap(), b"binary");
        assert_eq!(std::fs::read(dest.join("readme.txt")).unwrap(), b"hello");
    }

    #[test]
    fn extracts_nested_directories_and_directory_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("out");
        write_zip(
            &src,
            &[
                ("bin/", b""),
                ("bin/tool/", b""),
                ("bin/tool/deep.txt", b"x"),
            ],
        );

        extract_zip(&src, &dest, None, None).unwrap();

        assert_eq!(std::fs::read(dest.join("bin/tool/deep.txt")).unwrap(), b"x");
    }

    /// Zip-slip. Extraction holds because `extract_zip` goes through
    /// `ZipFile::enclosed_name()`, which returns `None` for a path that leaves the
    /// destination — and the code skips those entries.
    ///
    /// Worth pinning explicitly: swapping `enclosed_name()` for the raw `name()`
    /// would reintroduce the vulnerability with no other test noticing.
    #[test]
    fn zip_entries_escaping_the_destination_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        for evil in [
            "../escaped.txt",
            "../../escaped.txt",
            "sub/../../escaped.txt",
        ] {
            let src = tmp.path().join("evil.zip");
            write_zip(&src, &[(evil, b"pwned"), ("safe.txt", b"ok")]);

            extract_zip(&src, &dest, None, None).unwrap();

            assert!(
                !dest.parent().unwrap().join("escaped.txt").exists(),
                "{evil} must not be written outside the destination"
            );
            assert!(
                !tmp.path().join("escaped.txt").exists(),
                "{evil} must not be written anywhere above the destination"
            );
            // The harmless entry still extracted, so this is a real extraction.
            assert!(dest.join("safe.txt").is_file());
            std::fs::remove_file(dest.join("safe.txt")).unwrap();
        }
    }

    /// An absolute entry name is refused for the same reason.
    #[test]
    fn zip_entries_with_absolute_paths_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("evil.zip");
        let dest = tmp.path().join("out");

        let absolute = if cfg!(windows) {
            r"C:\escaped.txt"
        } else {
            "/escaped.txt"
        };
        write_zip(&src, &[(absolute, b"pwned"), ("safe.txt", b"ok")]);

        extract_zip(&src, &dest, None, None).unwrap();

        assert!(
            !Path::new(absolute).exists(),
            "an absolute entry must not be written"
        );
        assert!(dest.join("safe.txt").is_file());
    }

    /// Detection happens on the extension, so a manifest pointing at a URL with a
    /// query string must not be misread.
    #[test]
    fn query_strings_do_not_confuse_detection() {
        assert_eq!(
            detect_format("https://x/tool.zip?token=abc"),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            detect_format("HTTPS://X/TOOL.ZIP"),
            Some(ArchiveFormat::Zip),
            "detection should be case-insensitive"
        );
    }

    fn put_octal(field: &mut [u8], value: u64) {
        let digits = format!("{:0width$o}", value, width = field.len() - 1);
        field[..digits.len()].copy_from_slice(digits.as_bytes());
        field[field.len() - 1] = 0;
    }

    /// A hand-built ustar header, so the entry name can be anything at all — including
    /// a path that escapes. `tar::Builder` would not necessarily let us write one.
    fn tar_with_entry(name: &str, data: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        assert!(
            name_bytes.len() < 100,
            "test helper handles short names only"
        );
        header[..name_bytes.len()].copy_from_slice(name_bytes);
        put_octal(&mut header[100..108], 0o644); // mode
        put_octal(&mut header[108..116], 0); // uid
        put_octal(&mut header[116..124], 0); // gid
        put_octal(&mut header[124..136], data.len() as u64); // size
        put_octal(&mut header[136..148], 0); // mtime
        header[148..156].fill(b' '); // checksum placeholder
        header[156] = b'0'; // typeflag: regular file
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[265..269].copy_from_slice(b"root");

        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());

        let mut out = header.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(512), 0);
        out.resize(out.len() + 1024, 0); // two terminating zero blocks
        out
    }

    #[tokio::test]
    async fn extracts_a_tar_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.tar");
        let dest = tmp.path().join("out");
        std::fs::write(&src, tar_with_entry("app/bin/tool", b"hello")).unwrap();

        extract_tar(&src, &dest).await.unwrap();

        assert_eq!(
            std::fs::read(dest.join("app").join("bin").join("tool")).unwrap(),
            b"hello"
        );
    }

    /// The tar equivalent of zip-slip. Unlike zip, this one relies on `tar`
    /// refusing the path rather than on any check in this crate, so it is worth
    /// proving rather than assuming — a regression in the dependency would otherwise
    /// turn into a silent write outside the version directory.
    #[tokio::test]
    async fn tar_entries_escaping_the_destination_are_not_written_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let escaped = dest.parent().unwrap().join("escaped.txt");
        let _ = std::fs::remove_file(&escaped);

        let src = tmp.path().join("evil.tar");
        std::fs::write(&src, tar_with_entry("../escaped.txt", b"pwned")).unwrap();

        extract_tar(&src, &dest).await.unwrap();

        assert!(
            !escaped.exists(),
            "tar entry ../escaped.txt must not land outside the destination"
        );
    }

    /// And the deeper form, plus a check that ordinary entries in the same archive
    /// still extract — so this cannot pass by the whole archive being rejected.
    #[tokio::test]
    async fn extract_dir_merges_into_an_existing_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("version");
        std::fs::create_dir_all(&dest).unwrap();
        // What a previous URL of the same package already put there.
        std::fs::write(dest.join("first.txt"), b"from another url").unwrap();

        write_zip(
            &src,
            &[
                ("nested/inner/tool.exe", b"binary"),
                ("nested/inner/share/readme.txt", b"hello"),
                ("other/unrelated.txt", b"ignored"),
            ],
        );

        NativeArchive::new(None)
            .extract_with(&src, &dest, ArchiveFormat::Zip, Some("nested/inner"))
            .await
            .unwrap();

        assert_eq!(std::fs::read(dest.join("tool.exe")).unwrap(), b"binary");
        assert_eq!(
            std::fs::read(dest.join("share/readme.txt")).unwrap(),
            b"hello"
        );
        assert!(
            !dest.join("unrelated.txt").exists(),
            "only the contents of extract_dir belong in the version directory"
        );
        assert_eq!(
            std::fs::read(dest.join("first.txt")).unwrap(),
            b"from another url",
            "the merge must not wipe files that were already there"
        );
        assert!(!dest.join("_tmp").exists(), "_tmp must be removed");
        assert!(
            !dest.join("nested").exists(),
            "the extract_dir path must not survive the move"
        );
    }

    /// Manifests write `extract_dir` with either separator; `PFiles\Gossip\Gossip`
    /// is the real spelling in `extras/gossip.json`.
    #[tokio::test]
    async fn extract_dir_accepts_backslash_separators() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("version");
        write_zip(&src, &[("nested/inner/tool.exe", b"binary")]);

        NativeArchive::new(None)
            .extract_with(&src, &dest, ArchiveFormat::Zip, Some(r"nested\inner"))
            .await
            .unwrap();

        assert!(dest.join("tool.exe").is_file());
        assert!(!dest.join("_tmp").exists());
    }

    /// A manifest that names a directory the archive does not contain is broken,
    /// and silently leaving the files nested one level too deep would produce an
    /// app whose `bin` does not exist. Scoop aborts on `movedir`'s robocopy
    /// error; so does this.
    #[tokio::test]
    async fn a_missing_extract_dir_is_an_error_and_leaves_no_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("version");
        write_zip(&src, &[("elsewhere/tool.exe", b"binary")]);

        let err = NativeArchive::new(None)
            .extract_with(&src, &dest, ArchiveFormat::Zip, Some("nested/inner"))
            .await
            .expect_err("a missing extract_dir must fail the extraction");

        assert!(
            err.to_string().contains("not found"),
            "the error should name the missing directory, got: {err}"
        );
        assert!(!dest.join("_tmp").exists(), "_tmp must be cleaned up");
    }

    /// `extract_dir` is manifest input; it must not be able to move files out of
    /// the version directory.
    #[tokio::test]
    async fn an_extract_dir_escaping_the_destination_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.zip");
        let dest = tmp.path().join("version");
        write_zip(&src, &[("tool.exe", b"binary")]);

        for evil in ["../escape", r"..\..\Windows", "C:\\Windows"] {
            assert!(
                NativeArchive::new(None)
                    .extract_with(&src, &dest, ArchiveFormat::Zip, Some(evil))
                    .await
                    .is_err(),
                "{evil:?} must be refused"
            );
        }
    }

    /// Two archives into the same destination, the second with its own
    /// `extract_dir`: the first merge must not be undone by the second.
    #[tokio::test]
    async fn consecutive_extractions_accumulate_in_one_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("version");
        let first = tmp.path().join("first.zip");
        let second = tmp.path().join("second.zip");
        write_zip(&first, &[("one/a.txt", b"a")]);
        write_zip(&second, &[("two/b.txt", b"b")]);

        let archive = NativeArchive::new(None);
        archive
            .extract_with(&first, &dest, ArchiveFormat::Zip, Some("one"))
            .await
            .unwrap();
        archive
            .extract_with(&second, &dest, ArchiveFormat::Zip, Some("two"))
            .await
            .unwrap();

        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(dest.join("b.txt")).unwrap(), b"b");
    }

    /// The merge replaces a file that exists in both trees, the way
    /// `robocopy /e /move` does.
    #[test]
    fn merge_move_replaces_colliding_files() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from");
        let to = tmp.path().join("to");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("same.txt"), b"new").unwrap();
        std::fs::write(to.join("same.txt"), b"old").unwrap();

        merge_move(&from, &to).unwrap();

        assert_eq!(std::fs::read(to.join("same.txt")).unwrap(), b"new");
        assert!(!from.exists(), "the source directory is consumed");
    }

    #[test]
    fn normalize_manifest_dir_cleans_and_validates() {
        assert_eq!(
            normalize_manifest_dir("extract_dir", Some(".")).unwrap(),
            None
        );
        assert_eq!(
            normalize_manifest_dir("extract_dir", Some(r".\nested\inner\")).unwrap(),
            Some("nested/inner".to_owned())
        );
    }

    #[tokio::test]
    async fn deep_tar_traversal_is_refused_while_ordinary_entries_extract() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let escaped = tmp.path().join("escaped.txt");
        let _ = std::fs::remove_file(&escaped);

        let mut archive = tar_with_entry("good.txt", b"ok");
        let evil = tar_with_entry("../../escaped.txt", b"pwned");
        archive.extend_from_slice(&evil[..evil.len() - 1024]);

        let src = tmp.path().join("mixed.tar");
        std::fs::write(&src, &archive).unwrap();

        extract_tar(&src, &dest).await.unwrap();

        assert!(!escaped.exists(), "the traversal entry must not escape");
        assert!(
            dest.join("good.txt").is_file(),
            "the ordinary entry must still extract"
        );
    }
}

#[cfg(test)]
mod format_detection_tests {
    use super::super::{ArchiveFormat, detect_format, detect_format_for_url};

    #[test]
    fn detects_sfx_7z_via_url_fragment() {
        let url = "https://github.com/git-for-windows/git/releases/download/v2.55.0.windows.1/PortableGit-2.55.0-64-bit.7z.exe#/dl.7z";
        assert_eq!(detect_format_for_url(url), Some(ArchiveFormat::SevenZ));
    }

    #[test]
    fn detects_plain_zip_without_fragment() {
        let url = "https://example.com/tool-1.0.0.zip";
        assert_eq!(detect_format_for_url(url), Some(ArchiveFormat::Zip));
    }

    #[test]
    fn falls_back_to_base_url_when_fragment_has_no_extension() {
        let url = "https://example.com/tool-1.0.0.zip#/somelabel";
        assert_eq!(detect_format_for_url(url), Some(ArchiveFormat::Zip));
    }

    #[test]
    fn returns_none_for_unrecognized_extension() {
        let url = "https://example.com/tool-1.0.0.exe";
        assert_eq!(detect_format_for_url(url), None);
    }

    /// `Invoke-Extraction` switches on the file name in the version directory,
    /// i.e. `url_filename(url)` — the `#/name` fragment wins over the remote
    /// name. This is the pair that decides how `Setup.exe#/dl.7z` is unpacked.
    #[test]
    fn detection_uses_the_renamed_target_not_the_remote_extension() {
        assert_eq!(detect_format("dl.7z"), Some(ArchiveFormat::SevenZ));
        assert_eq!(
            detect_format("Setup.exe"),
            None,
            "a bare .exe is payload unless the manifest sets innosetup"
        );
        assert_eq!(detect_format("app-win64.exe"), None);
    }

    #[test]
    fn nupkg_is_a_zip() {
        assert_eq!(detect_format("package.nupkg"), Some(ArchiveFormat::Zip));
    }

    /// Scoop copies both of these, and extracting either breaks a real package:
    /// `.cab` files belong next to the `.msi` that references them (`msival2`,
    /// `orca`, `visualc`), and `plantuml` ships a runnable `.jar`.
    #[test]
    fn cab_and_jar_are_payload_not_archives() {
        assert_eq!(detect_format("8e4755178e6b5bcba8d3538c3630b7a5.cab"), None);
        assert_eq!(detect_format("plantuml.jar"), None);
    }

    /// `Test-7zipRequirement` (lib/depends.ps1:134) routes these to 7-Zip, and
    /// no Rust crate reads them.
    #[test]
    fn formats_only_seven_zip_reads_are_marked_external() {
        for name in [
            "tool.zst",
            "archive.tar.zst",
            "tool.lzma",
            "tool.lzh",
            "image.iso",
            "image.img",
            "archive.7z.001",
        ] {
            assert_eq!(
                detect_format(name),
                Some(ArchiveFormat::External7z),
                "{name} needs 7-Zip"
            );
        }
    }
}

#[cfg(test)]
#[cfg(windows)]
mod innosetup_extraction_tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    use super::super::ArchiveFormat;
    use super::super::inno::extract_innosetup_with_script;
    use super::super::sevenz::{find_7z, run_7z_stream_fallback};
    use super::super::staging::StagedExtraction;

    /// Regression: rake must never delete the cache file after InnoSetup
    /// extraction.  Official Scoop copies from cache into the version
    /// directory first, then extracts from the copy (the cache survives).
    /// Rake extracts directly from the cache, so remove_file(src) would
    /// break the cache-reuse guarantee.
    #[test]
    fn cache_file_survives_innosetup_extraction() {
        let dir = tempdir().unwrap();

        // Create a mock innounp.exe that find_helper can discover
        let innounp_dir = dir.path().join("apps").join("innounp").join("current");
        fs::create_dir_all(&innounp_dir).unwrap();
        fs::write(innounp_dir.join("innounp.exe"), b"").unwrap();

        // Create a fake cache file
        let cache_file = dir.path().join("cache").join("test-installer.exe");
        fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
        fs::write(&cache_file, b"fake installer content").unwrap();

        let dest = dir.path().join("extracted");
        fs::create_dir_all(&dest).unwrap();

        // Empty lines means no Expand-InnoArchive lines — innounp is
        // resolved but never invoked.  The function returns Ok after
        // an empty loop.  Without the fix, the old code would still
        // unconditionally remove src in extract_innosetup, or
        // conditionally remove it in extract_innosetup_with_script
        // if any line had -Removal.
        let lines: Vec<String> = vec![];
        let result = extract_innosetup_with_script(&lines, &cache_file, &dest, Some(dir.path()));

        assert!(result.is_ok(), "empty script should succeed");
        assert!(
            cache_file.exists(),
            "cache file must survive InnoSetup extraction"
        );
    }

    #[cfg(windows)]
    fn write_sample_tar_gz(path: &Path) {
        let tar_gz_file = std::fs::File::create(path).unwrap();
        let enc = flate2::write::GzEncoder::new(tar_gz_file, flate2::Compression::default());
        let mut tar_builder = tar::Builder::new(enc);

        for (entry_path, data) in [
            ("package/bin/app.js", &b"console.log('hello from app');"[..]),
            ("ignored/skip.txt", &b"should not be extracted"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar_builder
                .append_data(&mut header, entry_path, data)
                .unwrap();
        }
        let enc = tar_builder.into_inner().unwrap();
        enc.finish().unwrap();
    }

    #[cfg(windows)]
    fn create_fake_7z_helper(dir: &Path) -> (PathBuf, PathBuf) {
        let log_path = dir.join("fake-7z-calls.log");
        let ps1_path = dir.join("fake-7z.ps1");
        let cmd_path = dir.join("fake-7z.cmd");

        let ps1 = r#"$ErrorActionPreference = 'Stop'
$rawArgs = @(
    $env:RAKE_FAKE7Z_ARG1,
    $env:RAKE_FAKE7Z_ARG2,
    $env:RAKE_FAKE7Z_ARG3,
    $env:RAKE_FAKE7Z_ARG4,
    $env:RAKE_FAKE7Z_ARG5,
    $env:RAKE_FAKE7Z_ARG6
) | Where-Object { $_ }
Add-Content -LiteralPath (Join-Path $PSScriptRoot 'fake-7z-calls.log') -Value ($rawArgs -join '|')
$src = $rawArgs[1]
$dest = ($rawArgs | Where-Object { $_ -like '-o*' } | Select-Object -First 1).Substring(2)
$include = $rawArgs | Where-Object { $_ -like '-ir!*' } | Select-Object -First 1
New-Item -ItemType Directory -Force -Path $dest | Out-Null

if ($src.ToLowerInvariant().EndsWith('.tar.gz')) {
    $outTar = Join-Path $dest 'archive.tar'
    $inStream = [System.IO.File]::OpenRead($src)
    try {
        $gz = New-Object System.IO.Compression.GzipStream(
            $inStream,
            [System.IO.Compression.CompressionMode]::Decompress
        )
        try {
            $outStream = [System.IO.File]::Create($outTar)
            try {
                $gz.CopyTo($outStream)
            } finally {
                $outStream.Dispose()
            }
        } finally {
            $gz.Dispose()
        }
    } finally {
        $inStream.Dispose()
    }
} else {
    $tarArgs = @('-xf', $src, '-C', $dest)
    if ($include) {
        $sub = $include.Substring(4) -replace '\\\*$', '' -replace '\\', '/'
        $tarArgs += $sub
    }
    & tar @tarArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
"#;
        std::fs::write(&ps1_path, ps1).unwrap();
        let cmd = concat!(
            "@echo off\r\n",
            "set \"RAKE_FAKE7Z_ARG1=%~1\"\r\n",
            "set \"RAKE_FAKE7Z_ARG2=%~2\"\r\n",
            "set \"RAKE_FAKE7Z_ARG3=%~3\"\r\n",
            "set \"RAKE_FAKE7Z_ARG4=%~4\"\r\n",
            "set \"RAKE_FAKE7Z_ARG5=%~5\"\r\n",
            "set \"RAKE_FAKE7Z_ARG6=%~6\"\r\n",
            "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"%~dp0fake-7z.ps1\"\r\n",
        );
        std::fs::write(&cmd_path, cmd).unwrap();

        (cmd_path, log_path)
    }

    #[cfg(windows)]
    async fn run_compound_tar_gz_fallback_test(helper: &Path, work_dir: &Path) {
        let src_tar_gz = work_dir.join("archive.tar.gz");
        write_sample_tar_gz(&src_tar_gz);

        let dest = work_dir.join("dest");
        let staged = StagedExtraction::new(&dest, Some("package")).unwrap();

        run_7z_stream_fallback(
            helper,
            &src_tar_gz,
            &staged.staging,
            ArchiveFormat::TarGz,
            staged.extract_dir.as_deref(),
        )
        .unwrap();

        staged.finish().unwrap();

        assert_eq!(
            std::fs::read(dest.join("bin").join("app.js")).unwrap(),
            b"console.log('hello from app');"
        );
        assert!(
            !dest.join("ignored").exists() && !dest.join("skip.txt").exists(),
            "entries outside extract_dir must not end up in dest"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn compound_tar_gz_7z_fallback_unpacks_both_layers_and_honours_extract_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let helper = find_7z(None).unwrap_or_else(|| create_fake_7z_helper(tmp.path()).0);

        run_compound_tar_gz_fallback_test(&helper, tmp.path()).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn compound_tar_gz_fallback_runs_two_passes_with_mock_helper() {
        let tmp = tempfile::tempdir().unwrap();
        let (helper, calls_log) = create_fake_7z_helper(tmp.path());

        run_compound_tar_gz_fallback_test(&helper, tmp.path()).await;

        let log = std::fs::read_to_string(&calls_log).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "expected outer and inner passes, got: {lines:?}"
        );
        assert!(
            lines[0].contains("archive.tar.gz") && !lines[0].contains("-ir!"),
            "first pass must decompress outer .tar.gz without -ir! filter: {}",
            lines[0]
        );
        assert!(
            lines[1].contains("rake-tar-")
                && lines[1].contains(".tar")
                && lines[1].contains(r"-ir!package\*"),
            "second pass must unpack inner .tar with -ir!package\\*: {}",
            lines[1]
        );
    }
}
