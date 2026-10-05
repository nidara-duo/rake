use std::path::{Path, PathBuf};

use crate::Result;

#[derive(Debug, Clone)]
pub struct BinEntry {
    pub target: String,
    pub name: String,
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimType {
    Exe,
    Batch,
    PowerShell,
    Java,
    Python,
    Bash,
}

impl BinEntry {
    pub fn shim_type(&self) -> ShimType {
        match Path::new(&self.target)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("exe" | "com") => ShimType::Exe,
            Some("bat" | "cmd") => ShimType::Batch,
            Some("ps1") => ShimType::PowerShell,
            Some("jar") => ShimType::Java,
            Some("py") => ShimType::Python,
            _ => ShimType::Bash,
        }
    }
}

/// The Scoop-compatible shim executable, embedded into rake at compile time.
///
/// `build.rs` builds the `rake-shim-bin` crate (vendored from
/// `ScoopInstaller/Shim`) and copies the finished artifact into `OUT_DIR`, so a
/// separate `shim.exe` never has to be shipped next to `rake.exe` nor looked up
/// on disk at runtime. One embedded copy serves every shim.
static SHIM_EXE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shim.exe"));

/// Reject manifest-supplied names that could escape the intended output
/// directory via path traversal (e.g. a malicious bucket manifest
/// setting a `bin` name to `"..\\..\\Startup\\evil"`). Manifest data is
/// untrusted third-party input and must never be joined onto a
/// filesystem path without this check.
fn validate_manifest_name(name: &str) -> Result<()> {
    let p = Path::new(name);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "manifest supplied an unsafe name: '{name}' (absolute paths and '..' are not allowed)"
        ))));
    }
    Ok(())
}

/// Create a single shim for a given target file.
pub fn create_shim(
    target: &Path,
    name: &str,
    args: Option<&[String]>,
    app_name: &str,
    shims_dir: &Path,
) -> Result<()> {
    validate_manifest_name(name)?;
    let target = target.canonicalize()?;
    let resolved_path = target.to_string_lossy().into_owned();
    let shim_base = shims_dir.join(name.to_lowercase());

    match Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        // If name already has .exe extension, treat as exe target
        Some("exe" | "com") | None if is_exe_target(&target) => {
            create_exe_shim(&target, &shim_base, args)?;
        }
        Some("bat" | "cmd") => {
            create_batch_shim(&resolved_path, &shim_base)?;
        }
        Some("ps1") => {
            create_powershell_shim(&target, &resolved_path, &shim_base, app_name)?;
        }
        Some("jar") => {
            create_jar_shim(&resolved_path, &shim_base)?;
        }
        Some("py") => {
            create_python_shim(&resolved_path, &shim_base)?;
        }
        _ => {
            // Fallback: detect by existing target file
            if target
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase())
                .as_deref()
                == Some("exe")
            {
                create_exe_shim(&target, &shim_base, args)?;
            } else {
                create_batch_shim(&resolved_path, &shim_base)?;
            }
        }
    }

    Ok(())
}

fn is_exe_target(target: &Path) -> bool {
    target
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("exe") || s.eq_ignore_ascii_case("com"))
        .unwrap_or(false)
}

/// Create a shim for .exe / .com targets using shim.exe + .shim metadata.
fn create_exe_shim(target: &Path, shim_base: &Path, args: Option<&[String]>) -> Result<()> {
    let shim_exe_path = shim_base.with_extension("exe");
    let shim_file_path = shim_base.with_extension("shim");

    // The shim binary is compiled into rake itself (see SHIM_EXE), so there is
    // no external file to locate and no fallback path needed.
    std::fs::write(&shim_exe_path, SHIM_EXE)?;

    // .shim metadata, in the format scoop's shim reads.
    let mut shim_content = format!("path = \"{}\"\r\n", target.display());
    if let Some(args) = args.filter(|a| !a.is_empty()) {
        shim_content.push_str(&format!("args = {}\r\n", args.join(" ")));
    }
    std::fs::write(&shim_file_path, shim_content)?;

    Ok(())
}

/// Create shim for .bat / .cmd scripts — simple cmd wrapper.
fn create_batch_shim(resolved: &str, shim_base: &Path) -> Result<()> {
    let cmd_path = shim_base.with_extension("cmd");
    let content = format!(
        "@rem {resolved}\r\n@\"{resolved}\" %*\r\n",
        resolved = resolved
    );
    std::fs::write(&cmd_path, content)?;

    // Unix-compatible shim (no extension)
    let shim_path = shim_base.with_extension("");
    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\nMSYS2_ARG_CONV_EXCL=/C cmd.exe /C \"{resolved}\" \"$@\"\n",
        resolved = resolved
    );
    std::fs::write(&shim_path, unix_content)?;

    Ok(())
}

/// Create shim for .ps1 scripts — ps1 wrapper + cmd fallback.
fn create_powershell_shim(
    target: &Path,
    resolved: &str,
    shim_base: &Path,
    app_name: &str,
) -> Result<()> {
    // PowerShell wrapper
    let ps1_path = shim_base.with_extension("ps1");
    let ps1_content = format!(
        "# {resolved}\n$path = Join-Path $PSScriptRoot \"..\\..\\apps\\{app}\\current\\{target_rel}\"\nif ($MyInvocation.ExpectingInput) {{ $input | & $path @args }} else {{ & $path @args }}\nexit $LASTEXITCODE\n",
        resolved = resolved,
        app = app_name,
        target_rel = target.file_name().and_then(|s| s.to_str()).unwrap_or(""),
    );
    std::fs::write(&ps1_path, ps1_content)?;

    // CMD fallback (tries pwsh.exe first, then powershell.exe)
    let cmd_path = shim_base.with_extension("cmd");
    let cmd_content = format!(
        "@rem {resolved}\n@echo off\nwhere /q pwsh.exe\nif %errorlevel% equ 0 (\n    pwsh -noprofile -ex unrestricted -file \"{resolved}\" %*\n) else (\n    powershell -noprofile -ex unrestricted -file \"{resolved}\" %*\n)\n",
        resolved = resolved,
    );
    std::fs::write(&cmd_path, cmd_content)?;

    // Unix shim
    let shim_path = shim_base.with_extension("");
    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\nif command -v pwsh.exe > /dev/null 2>&1; then\n    pwsh.exe -noprofile -ex unrestricted -file \"{resolved}\" \"$@\"\nelse\n    powershell.exe -noprofile -ex unrestricted -file \"{resolved}\" \"$@\"\nfi\n",
        resolved = resolved,
    );
    std::fs::write(&shim_path, unix_content)?;

    Ok(())
}

/// Create shim for .jar files — cmd wrapper with java -jar.
fn create_jar_shim(resolved: &str, shim_base: &Path) -> Result<()> {
    let cmd_path = shim_base.with_extension("cmd");
    let content = format!(
        "@rem {resolved}\n@pushd \"{dir}\"\n@java -jar \"{resolved}\" %*\n@popd\n",
        resolved = resolved,
        dir = Path::new(resolved).parent().map(|p| p.display()).unwrap(),
    );
    std::fs::write(&cmd_path, content)?;

    let shim_path = shim_base.with_extension("");
    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\ncd \"{dir}\"\njava.exe -jar \"{resolved}\" \"$@\"\n",
        resolved = resolved,
        dir = Path::new(resolved).parent().map(|p| p.display()).unwrap(),
    );
    std::fs::write(&shim_path, unix_content)?;

    Ok(())
}

/// Create shim for .py files — cmd wrapper with python.
fn create_python_shim(resolved: &str, shim_base: &Path) -> Result<()> {
    let cmd_path = shim_base.with_extension("cmd");
    let content = format!(
        "@rem {resolved}\n@python \"{resolved}\" %*\n",
        resolved = resolved,
    );
    std::fs::write(&cmd_path, content)?;

    let shim_path = shim_base.with_extension("");
    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\npython.exe \"{resolved}\" \"$@\"\n",
        resolved = resolved,
    );
    std::fs::write(&shim_path, unix_content)?;

    Ok(())
}

/// Remove a single shim by name (removes .exe, .shim, .cmd, .ps1).
pub fn remove_shim(name: &str, shims_dir: &Path) -> Result<()> {
    validate_manifest_name(name)?;
    let lower = name.to_lowercase();
    let base = shims_dir.join(&lower);
    for ext in &["exe", "shim", "cmd", "ps1"] {
        let path = base.with_extension(ext);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    // No-extension shim (Unix compat)
    let noext = base.with_extension("");
    if noext.exists() {
        std::fs::remove_file(&noext)?;
    }
    Ok(())
}

/// Create shims for all bin entries in a manifest.
///
/// `app_dir` is the directory the `bin` targets are resolved against. During install
/// that is the `current` junction, not the version directory, so the recorded shim
/// target reads `apps/<name>/current/<exe>` and keeps following `current` across
/// updates — the same layout scoop produces.
pub fn create_shims(entries: &[BinEntry], app_dir: &Path, shims_dir: &Path) -> Result<()> {
    let app_name = app_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    for entry in entries {
        let target = app_dir.join(&entry.target);
        if !target.exists() {
            continue;
        }
        create_shim(
            &target,
            &entry.name,
            entry.args.as_deref(),
            app_name,
            shims_dir,
        )?;
    }
    Ok(())
}

/// Remove shims for all bin entries.
pub fn remove_shims(entries: &[BinEntry], shims_dir: &Path) -> Result<()> {
    for entry in entries {
        remove_shim(&entry.name, shims_dir)?;
    }
    Ok(())
}

/// Parse the manifest `bin` field into BinEntry list.
///
/// Format (matching Scoop):
///   - `"git.exe"` → target=git.exe, name=git
///   - `["git.exe", "git"]` → target=git.exe, name=git
///   - `["git.exe", "git", "--arg"]` → target=git.exe, name=git, args=[--arg]
///   - `[["git.exe", "git"], ...]` → multiple entries
pub fn parse_bin(
    bin: &rake_domain::one_or_many::OneOrMany<rake_domain::one_or_many::OneOrMany<String>>,
) -> Vec<BinEntry> {
    let mut entries = Vec::new();
    for item in bin.iter() {
        let parts: Vec<&String> = item.iter().collect();
        if parts.is_empty() {
            continue;
        }
        let target = parts[0].clone();
        let name = parts
            .get(1)
            .map(|s| (*s).clone())
            .unwrap_or_else(|| strip_ext(&target));
        let args = if parts.len() > 2 {
            Some(parts[2..].iter().map(|s| (*s).clone()).collect())
        } else {
            None
        };
        entries.push(BinEntry { target, name, args });
    }
    entries
}

fn strip_ext(filename: &str) -> String {
    let path = PathBuf::from(filename);
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename)
        .to_owned()
}

#[cfg(test)]
mod path_safety_tests {
    use super::*;

    #[test]
    fn rejects_parent_dir_traversal() {
        assert!(validate_manifest_name("..\\..\\Startup\\evil").is_err());
        assert!(validate_manifest_name("../../etc/passwd").is_err());
    }

    #[test]
    fn rejects_absolute_path() {
        assert!(validate_manifest_name("C:\\Windows\\System32\\evil").is_err());
    }

    #[test]
    fn accepts_plain_name() {
        assert!(validate_manifest_name("git").is_ok());
        assert!(validate_manifest_name("my-tool").is_ok());
    }

    /// The embedded binary must always be present. A zero-length payload would make
    /// every shim a silent no-op at runtime, long after the build succeeded.
    #[test]
    fn embedded_shim_is_not_empty() {
        assert!(!SHIM_EXE.is_empty());
        // An x64 PE starts with the DOS signature "MZ".
        assert_eq!(&SHIM_EXE[..2], b"MZ");
    }

    /// Regression: `bin: ["bun.exe", "bunx", "x"]` has to reach the `.shim` file as
    /// `args = x`. It used to be parsed into BinEntry and then dropped on the floor, so
    /// the `bunx` shim launched plain `bun`.
    #[test]
    fn writes_args_into_shim_metadata() {
        use rake_domain::one_or_many::OneOrMany;
        // `"bin": [["bun.exe", "bunx", "x"]]`
        let json = r#"[["bun.exe","bunx","x"]]"#;
        let bin: OneOrMany<OneOrMany<String>> = serde_json::from_str(json).unwrap();
        let entries = parse_bin(&bin);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "bunx");
        assert_eq!(
            entries[0].args.as_deref(),
            Some(["x".to_owned()].as_slice())
        );
    }

    /// Lay out `apps/<app>/current` with a binary plus an empty `shims` dir, and return
    /// both. Mirrors the paths an install produces.
    fn layout(dir: &Path, app: &str, exe: &str) -> (PathBuf, PathBuf) {
        let app_dir = dir.join("apps").join(app).join("current");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join(exe), b"MZ").unwrap();
        let shims_dir = dir.join("shims");
        std::fs::create_dir_all(&shims_dir).unwrap();
        (app_dir, shims_dir)
    }

    fn entry(target: &str, name: &str, args: Option<&str>) -> BinEntry {
        BinEntry {
            target: target.to_owned(),
            name: name.to_owned(),
            args: args.map(|a| vec![a.to_owned()]),
        }
    }

    /// End-to-end for the bug this file was written around: a manifest `bin` carrying
    /// arguments has to produce a `.shim` that actually contains them.
    ///
    /// The parse-only test above cannot catch a regression here, because the args used
    /// to parse correctly and were then dropped when the file was written. This one
    /// checks the bytes on disk.
    #[test]
    fn manifest_args_reach_the_shim_file() {
        use rake_domain::one_or_many::OneOrMany;

        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout(dir.path(), "bun", "bun.exe");

        let bin: OneOrMany<OneOrMany<String>> =
            serde_json::from_str(r#"[["bun.exe","bunx","x"]]"#).unwrap();
        create_shims(&parse_bin(&bin), &app_dir, &shims_dir).unwrap();

        let content = std::fs::read_to_string(shims_dir.join("bunx.shim")).unwrap();
        assert!(content.contains("path = "), "path missing in:\n{content}");
        assert!(content.contains("args = x"), "args missing in:\n{content}");
    }

    /// Scoop writes arguments verbatim, quotes and spaces included — a real example on
    /// disk is `args = --user-data-dir="...brave\current\User Data"`. Re-joining the
    /// parts must not re-quote or re-escape them.
    #[test]
    fn shim_args_keep_quotes_and_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout(dir.path(), "brave", "brave.exe");

        let arg = r#"--user-data-dir="C:\some path\User Data""#;
        create_shims(
            &[entry("brave.exe", "brave", Some(arg))],
            &app_dir,
            &shims_dir,
        )
        .unwrap();

        let content = std::fs::read_to_string(shims_dir.join("brave.shim")).unwrap();
        assert!(
            content.contains(&format!("args = {arg}")),
            "expected the argument verbatim in:\n{content}"
        );
    }

    /// The shim executable must be the embedded payload, not a stray file found
    /// elsewhere on disk.
    #[test]
    fn shim_exe_is_the_embedded_payload() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout(dir.path(), "git", "git.exe");

        create_shims(&[entry("git.exe", "git", None)], &app_dir, &shims_dir).unwrap();

        let written = std::fs::read(shims_dir.join("git.exe")).unwrap();
        assert_eq!(written, SHIM_EXE);
        assert_eq!(&written[..2], b"MZ");
    }

    /// No `args` line at all when the manifest has none — Scoop omits the key rather than
    /// writing an empty one.
    #[test]
    fn shim_without_args_omits_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout(dir.path(), "git", "git.exe");

        create_shims(&[entry("git.exe", "git", None)], &app_dir, &shims_dir).unwrap();

        let content = std::fs::read_to_string(shims_dir.join("git.shim")).unwrap();
        assert!(!content.contains("args"), "unexpected args in:\n{content}");
        assert!(content.contains("path = "));
    }

    /// The recorded path has to go through `current`, not the version directory, so a
    /// later update that only repoints the junction leaves the shim valid.
    #[test]
    fn shim_path_goes_through_current() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout(dir.path(), "brave", "brave.exe");

        create_shims(&[entry("brave.exe", "brave", None)], &app_dir, &shims_dir).unwrap();

        let content = std::fs::read_to_string(shims_dir.join("brave.shim")).unwrap();
        assert!(
            content.contains(r"current\brave.exe"),
            "expected the current junction in:\n{content}"
        );
    }

    /// A bin entry naming a file that was never installed is skipped silently instead of
    /// failing the whole install.
    #[test]
    fn missing_target_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout(dir.path(), "bun", "bun.exe");

        create_shims(
            &[
                entry("bun.exe", "bun", None),
                entry("not-installed.exe", "ghost", None),
            ],
            &app_dir,
            &shims_dir,
        )
        .unwrap();

        assert!(shims_dir.join("bun.shim").is_file());
        assert!(!shims_dir.join("ghost.shim").exists());
    }

    /// `bin` may name the shim with or without the `.exe` suffix; both must land on the
    /// same shim names.
    #[test]
    fn bin_name_with_exe_suffix_is_accepted() {
        use rake_domain::one_or_many::OneOrMany;

        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout(dir.path(), "bun", "bun.exe");

        let bin: OneOrMany<OneOrMany<String>> =
            serde_json::from_str(r#"[["bun.exe","bunx.exe","x"]]"#).unwrap();
        create_shims(&parse_bin(&bin), &app_dir, &shims_dir).unwrap();

        let content = std::fs::read_to_string(shims_dir.join("bunx.shim")).unwrap();
        assert!(content.contains("args = x"), "args missing in:\n{content}");
        assert!(shims_dir.join("bunx.exe").is_file());
    }

    /// With a single part the shim name is the target minus its extension.
    #[test]
    fn bin_name_defaults_to_stripped_target() {
        use rake_domain::one_or_many::OneOrMany;

        let bin: OneOrMany<OneOrMany<String>> = serde_json::from_str(r#"["git.exe"]"#).unwrap();
        let entries = parse_bin(&bin);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "git");
        assert_eq!(entries[0].args, None);
    }

    /// A manifest-supplied name must not escape the shims directory, whatever it claims.
    #[test]
    fn traversal_in_bin_name_is_refused_at_creation() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout(dir.path(), "evil", "evil.exe");

        let err = create_shims(
            &[entry("evil.exe", r"..\..\Startup\evil", None)],
            &app_dir,
            &shims_dir,
        );
        assert!(err.is_err(), "traversal must be refused");
    }
}
