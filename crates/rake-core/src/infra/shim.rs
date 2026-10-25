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
///
/// The check itself lives in [`crate::infra::fs::validate_relative_path`],
/// shared with `persist`, which has the same exposure.
fn validate_manifest_name(name: &str) -> Result<()> {
    crate::infra::fs::validate_relative_path("bin name", name)
}

/// Escape a value for use inside a PowerShell **double**-quoted string.
///
/// PowerShell treats `"` and `` ` `` as the only structural characters there, so both
/// are escaped with a backtick — unlike the single-quoted form used for hook variables
/// in `infra/script.rs`, where doubling `'` is what works.
///
/// Not cosmetic: an unescaped `"` closes the string, and the remainder of the value is
/// parsed as code. That is reachable from a bucket manifest, since the app name and the
/// bin target both come from one.
fn escape_powershell_dquote(value: &str) -> String {
    value.replace('`', "``").replace('"', "`\"")
}

/// The path a shim is written for, **without** resolving the `current` junction.
///
/// `canonicalize` opens the file and resolves reparse points, so during a real
/// install — where `apps/<app>/current` *is* a junction — it rewrote every shim
/// target to `apps/<app>/<version>/...`. The shim then survived an update only
/// by accident and pointed at a directory that was about to be deleted. Scoop's
/// `Convert-Path` does not resolve links either, so neither does this.
///
/// `std::path::absolute` is the right primitive: it only normalizes separators
/// and `.` segments, and joins a relative path onto the working directory.
fn absolute_no_link_resolution(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).map_err(crate::Error::Io)
}

/// `<shims>/<name>`, lowercased like Scoop's `$name.tolower()`.
///
/// A trailing `.exe`/`.com` on the *shim name* is dropped: `bin: [["bun.exe",
/// "bunx.exe"]]` means the shim called `bunx`, not a second `.exe`. Scoop would
/// build `bunx.exe.exe` here, which no manifest relies on.
///
/// Everything after the first dot is kept otherwise. `Path::with_extension`
/// cannot be used for the suffixes below for exactly that reason: it replaces
/// the last extension, so a shim named `python3.11` would silently become
/// `python3.cmd`.
fn shim_base(shims_dir: &Path, name: &str) -> PathBuf {
    let lower = name.to_lowercase();
    let trimmed = lower
        .strip_suffix(".exe")
        .or_else(|| lower.strip_suffix(".com"))
        .unwrap_or(&lower);
    shims_dir.join(trimmed)
}

/// Append a literal suffix (`.cmd`, `.shim`, …) to a shim base name.
fn with_suffix(base: &Path, suffix: &str) -> PathBuf {
    let mut name = base.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// ` <args>` for the wrapper bodies, or nothing at all.
///
/// Scoop interpolates the argument list verbatim (`"`"$resolved_path`" $arg %*"`),
/// so quotes and spaces inside an argument survive as written.
fn args_suffix(args: Option<&[String]>) -> String {
    match args {
        Some(args) if !args.is_empty() => format!(" {}", args.join(" ")),
        _ => String::new(),
    }
}

/// Create a single shim for a given target file.
///
/// Mirrors `shim` in Scoop's `lib/core.ps1:949`: the wrapper is chosen by the
/// **target's** extension, not by the shim's name. `bin: [["code.cmd", "code"]]`
/// must produce a `.cmd` wrapper because the target is a batch file, however the
/// shim is called.
pub fn create_shim(
    target: &Path,
    name: &str,
    args: Option<&[String]>,
    shims_dir: &Path,
) -> Result<()> {
    validate_manifest_name(name)?;
    let target = absolute_no_link_resolution(target)?;
    let resolved_path = target.to_string_lossy().into_owned();
    let base = shim_base(shims_dir, name);
    let suffix = args_suffix(args);

    match target
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("exe" | "com") => create_exe_shim(&target, &base, args)?,
        Some("bat" | "cmd") => create_batch_shim(&resolved_path, &base, &suffix)?,
        Some("ps1") => create_powershell_shim(&resolved_path, &base, &suffix)?,
        Some("jar") => create_jar_shim(&resolved_path, &base, &suffix)?,
        Some("py") => create_python_shim(&resolved_path, &base, &suffix)?,
        // A target without a recognised extension is a shell script: Scoop runs
        // it through bash, translating the path with wslpath/cygpath.
        _ => create_bash_shim(&resolved_path, &base, &suffix)?,
    }

    Ok(())
}

/// Create a shim for .exe / .com targets using shim.exe + .shim metadata.
fn create_exe_shim(target: &Path, base: &Path, args: Option<&[String]>) -> Result<()> {
    // The shim binary is compiled into rake itself (see SHIM_EXE), so there is
    // no external file to locate and no fallback path needed.
    std::fs::write(with_suffix(base, ".exe"), SHIM_EXE)?;

    // .shim metadata, in the format scoop's shim reads.
    let mut shim_content = format!("path = \"{}\"\r\n", target.display());
    if let Some(args) = args.filter(|a| !a.is_empty()) {
        shim_content.push_str(&format!("args = {}\r\n", args.join(" ")));
    }
    std::fs::write(with_suffix(base, ".shim"), shim_content)?;

    Ok(())
}

/// Create shim for .bat / .cmd scripts.
///
/// Scoop deliberately does **not** copy `shim.exe` here: a batch file cannot be
/// executed by the shim binary, so it generates a `.cmd` wrapper (usable from
/// cmd.exe and from programs that know nothing about PowerShell) and a POSIX
/// shell wrapper for MSYS/Git Bash.
fn create_batch_shim(resolved: &str, base: &Path, args: &str) -> Result<()> {
    let cmd_content = format!(
        "@rem {resolved}\r\n@\"{resolved}\"{args} %*\r\n",
        resolved = resolved,
        args = args,
    );
    std::fs::write(with_suffix(base, ".cmd"), cmd_content)?;

    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\nMSYS2_ARG_CONV_EXCL=/C cmd.exe /C \"{resolved}\"{args} \"$@\"",
        resolved = resolved,
        args = args,
    );
    std::fs::write(base, unix_content)?;

    Ok(())
}

/// Create shim for .ps1 scripts — ps1 wrapper plus cmd and POSIX launchers.
fn create_powershell_shim(resolved: &str, base: &Path, args: &str) -> Result<()> {
    // The path is embedded in PowerShell source, inside double quotes, so a `"`
    // in it would close the string and turn the rest into code. Verified by
    // execution, not by inspection: an app name of `demo"; New-Item ...; $x="`
    // really did run the injected command when this was interpolated raw.
    let quoted = escape_powershell_dquote(resolved);
    let ps1_content = format!(
        "# {comment}\r\n$path = \"{quoted}\"\r\nif ($MyInvocation.ExpectingInput) {{ $input | & $path{args} @args }} else {{ & $path{args} @args }}\r\nexit $LASTEXITCODE",
        comment = quoted,
        quoted = quoted,
        args = args,
    );
    std::fs::write(with_suffix(base, ".ps1"), ps1_content)?;

    // CMD launcher: pwsh when available, Windows PowerShell otherwise.
    let cmd_content = format!(
        "@rem {resolved}\r\n@echo off\r\nwhere /q pwsh.exe\r\nif %errorlevel% equ 0 (\r\n    pwsh -noprofile -ex unrestricted -file \"{resolved}\"{args} %*\r\n) else (\r\n    powershell -noprofile -ex unrestricted -file \"{resolved}\"{args} %*\r\n)",
        resolved = resolved,
        args = args,
    );
    std::fs::write(with_suffix(base, ".cmd"), cmd_content)?;

    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\nif command -v pwsh.exe > /dev/null 2>&1; then\n    pwsh.exe -noprofile -ex unrestricted -file \"{resolved}\"{args} \"$@\"\nelse\n    powershell.exe -noprofile -ex unrestricted -file \"{resolved}\"{args} \"$@\"\nfi",
        resolved = resolved,
        args = args,
    );
    std::fs::write(base, unix_content)?;

    Ok(())
}

/// Create shim for .jar files — runs `java -jar` from the jar's own directory,
/// because jars routinely resolve resources relative to the working directory.
fn create_jar_shim(resolved: &str, base: &Path, args: &str) -> Result<()> {
    let dir = Path::new(resolved)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let cmd_content = format!(
        "@rem {resolved}\r\n@pushd \"{dir}\"\r\n@java -jar \"{resolved}\"{args} %*\r\n@popd",
        resolved = resolved,
        dir = dir,
        args = args,
    );
    std::fs::write(with_suffix(base, ".cmd"), cmd_content)?;

    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\nif [ $WSL_INTEROP ]\nthen\n  cd $(wslpath -u '{dir}')\nelse\n  cd $(cygpath -u '{dir}')\nfi\njava.exe -jar \"{resolved}\"{args} \"$@\"",
        resolved = resolved,
        dir = dir,
        args = args,
    );
    std::fs::write(base, unix_content)?;

    Ok(())
}

/// Create shim for .py files.
fn create_python_shim(resolved: &str, base: &Path, args: &str) -> Result<()> {
    let cmd_content = format!(
        "@rem {resolved}\r\n@python \"{resolved}\"{args} %*\r\n",
        resolved = resolved,
        args = args,
    );
    std::fs::write(with_suffix(base, ".cmd"), cmd_content)?;

    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\npython.exe \"{resolved}\"{args} \"$@\"",
        resolved = resolved,
        args = args,
    );
    std::fs::write(base, unix_content)?;

    Ok(())
}

/// Create shim for a target with no recognised extension.
///
/// Scoop treats these as shell scripts and runs them through bash, translating
/// the Windows path with `wslpath` (inside WSL) or `cygpath` (MSYS/Git Bash).
fn create_bash_shim(resolved: &str, base: &Path, args: &str) -> Result<()> {
    // Scoop quotes each argument individually in this branch, because the whole
    // command line is reassembled as one string for `bash -c`.
    let quoted_args = args
        .split_whitespace()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let quoted_args = if quoted_args.is_empty() {
        String::new()
    } else {
        format!(" {quoted_args}")
    };

    let cmd_content = format!(
        "@rem {resolved}\r\n@echo off\r\nbash -c \"command -v wslpath >/dev/null\"\r\nif %errorlevel% equ 0 (\r\n  bash \"$(wslpath -u '{resolved}')\"{quoted_args} %*\r\n) else (\r\n  set args={quoted_args} %*\r\n  setlocal enabledelayedexpansion\r\n  if not \"!args!\"==\"\" set args=!args:\"=\"\"!\r\n  bash -c \"$(cygpath -u '{resolved}') !args!\"\r\n)",
        resolved = resolved,
        quoted_args = quoted_args,
    );
    std::fs::write(with_suffix(base, ".cmd"), cmd_content)?;

    let unix_content = format!(
        "#!/bin/sh\n# {resolved}\nif [ $WSL_INTEROP ]\nthen\n  \"$(wslpath -u '{resolved}')\"{args} \"$@\"\nelse\n  \"$(cygpath -u '{resolved}')\"{args} \"$@\"\nfi",
        resolved = resolved,
        args = args,
    );
    std::fs::write(base, unix_content)?;

    Ok(())
}

/// Remove a single shim by name (removes .exe, .shim, .cmd, .ps1 and the bare
/// POSIX wrapper).
pub fn remove_shim(name: &str, shims_dir: &Path) -> Result<()> {
    validate_manifest_name(name)?;
    let base = shim_base(shims_dir, name);
    for suffix in [".exe", ".shim", ".cmd", ".ps1"] {
        let path = with_suffix(&base, suffix);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    if base.exists() {
        std::fs::remove_file(&base)?;
    }
    Ok(())
}

/// Create shims for all bin entries in a manifest, returning the names that were
/// actually written (an entry whose target is missing is skipped).
///
/// `app_dir` is the directory the `bin` targets are resolved against. During install
/// that is the `current` junction, not the version directory, so the recorded shim
/// target reads `apps/<name>/current/<exe>` and keeps following `current` across
/// updates — the same layout scoop produces.
pub fn create_shims(entries: &[BinEntry], app_dir: &Path, shims_dir: &Path) -> Result<Vec<String>> {
    let mut created = Vec::new();
    for entry in entries {
        let target = app_dir.join(&entry.target);
        if !target.exists() {
            continue;
        }
        create_shim(&target, &entry.name, entry.args.as_deref(), shims_dir)?;
        created.push(entry.name.clone());
    }
    Ok(created)
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

    /// A double quote in the app name closes the string in the generated `.ps1` shim
    /// and turns the rest of the value into code. Confirmed by execution, not by
    /// reading: an app name of `demo"; <command>; $x="` really did run that command
    /// when the generated shim was launched.
    #[test]
    fn dquote_escaping_defuses_a_ps1_injection() {
        let escaped = escape_powershell_dquote(r#"demo"; New-Item evil; $x=""#);
        assert!(
            escaped.contains(r#"`""#),
            "the closing quote must be escaped, got: {escaped}"
        );
        assert_eq!(
            escaped.matches('"').count(),
            2,
            "only the escaped pair may remain: {escaped}"
        );
    }

    /// Inside a double-quoted string the backtick is the escape character, so it has to
    /// be doubled first — otherwise the backtick a value already contains would escape
    /// the backslash we add in front of a quote.
    #[test]
    fn backticks_are_escaped_before_quotes() {
        assert_eq!(escape_powershell_dquote("a`b"), "a``b");
        assert_eq!(escape_powershell_dquote(r#"a"b"#), "a`\"b");
        assert_eq!(escape_powershell_dquote("a`\"b"), "a```\"b");
    }

    /// Ordinary names must pass through untouched, or every existing shim would break.
    #[test]
    fn ordinary_app_name_is_unchanged_by_escaping() {
        assert_eq!(escape_powershell_dquote("demo"), "demo");
        assert_eq!(escape_powershell_dquote("my-app"), "my-app");
        assert_eq!(escape_powershell_dquote("My App 1.0"), "My App 1.0");
        assert_eq!(
            escape_powershell_dquote(r"C:\path\to\file.ps1"),
            r"C:\path\to\file.ps1"
        );
    }

    /// The single-quoted form used for hook variables in `infra/script.rs` is a
    /// different escape and must not be confused with the double-quoted one above:
    /// in single quotes a backtick is literal and `'` is doubled.
    #[test]
    fn single_quoted_escaping_is_a_separate_rule() {
        assert_eq!(
            crate::infra::script::quote_powershell_string("a'b"),
            "'a''b'"
        );
        assert_eq!(
            crate::infra::script::quote_powershell_string("a\"b`c"),
            "'a\"b`c'",
            "a double quote and a backtick are literal inside single quotes"
        );
        assert_ne!(
            crate::infra::script::quote_powershell_string("a`b"),
            format!("'{}'", escape_powershell_dquote("a`b")),
            "the two rules must not coincide"
        );
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

/// The wrapper shapes `shim` in Scoop's `lib/core.ps1:949` produces. The target's
/// extension decides the wrapper — not the shim's name, and not the download URL.
#[cfg(test)]
mod wrapper_tests {
    use super::*;

    fn layout_file(dir: &Path, app: &str, file: &str) -> (PathBuf, PathBuf) {
        let app_dir = dir.join("apps").join(app).join("current");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join(file), b"payload").unwrap();
        let shims_dir = dir.join("shims");
        std::fs::create_dir_all(&shims_dir).unwrap();
        (app_dir, shims_dir)
    }

    fn entry(target: &str, name: &str, args: Option<&[&str]>) -> BinEntry {
        BinEntry {
            target: target.to_owned(),
            name: name.to_owned(),
            args: args.map(|a| a.iter().map(|s| (*s).to_owned()).collect()),
        }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{} should exist: {e}", path.display()))
    }

    /// `bin: [["code.cmd", "code"]]` (vscode and 200-odd other manifests) must
    /// produce a `.cmd` wrapper. Copying `shim.exe` here — which the previous
    /// implementation did whenever the *shim name* had no extension — produced a
    /// shim that could not launch a batch file at all.
    #[test]
    fn a_cmd_target_gets_a_cmd_wrapper_and_no_shim_exe() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout_file(dir.path(), "vscode", "code.cmd");

        create_shims(&[entry("code.cmd", "code", None)], &app_dir, &shims_dir).unwrap();

        let cmd = read(&shims_dir.join("code.cmd"));
        assert!(
            cmd.contains(r#"@"C:"#) && cmd.contains("code.cmd"),
            "the wrapper must launch the target:\n{cmd}"
        );
        assert!(cmd.contains("%*"), "arguments must be forwarded:\n{cmd}");
        assert!(
            shims_dir.join("code").is_file(),
            "the POSIX wrapper must exist too"
        );
        assert!(
            !shims_dir.join("code.exe").exists(),
            "a batch file must not get a shim.exe"
        );
        assert!(
            !shims_dir.join("code.shim").exists(),
            "a batch file must not get .shim metadata"
        );
    }

    /// Arguments go into every wrapper of the `.cmd` target, not just the `.cmd`.
    #[test]
    fn a_cmd_wrapper_carries_the_manifest_args() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout_file(dir.path(), "demo", "run.cmd");

        create_shims(
            &[entry("run.cmd", "run", Some(&["--flag", "x"]))],
            &app_dir,
            &shims_dir,
        )
        .unwrap();

        let cmd = read(&shims_dir.join("run.cmd"));
        assert!(cmd.contains("--flag x %*"), "args missing in:\n{cmd}");
        let sh = read(&shims_dir.join("run"));
        assert!(
            sh.contains("--flag x \"$@\""),
            "args missing from the POSIX wrapper:\n{sh}"
        );
    }

    /// A `.ps1` target needs all three launchers, each forwarding the arguments.
    #[test]
    fn a_ps1_target_writes_ps1_cmd_and_posix_wrappers_with_args() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout_file(dir.path(), "concfg", "concfg.ps1");

        create_shims(
            &[entry("concfg.ps1", "concfg", Some(&["-Force"]))],
            &app_dir,
            &shims_dir,
        )
        .unwrap();

        let ps1 = read(&shims_dir.join("concfg.ps1"));
        assert!(ps1.contains("& $path -Force @args"), "ps1 body:\n{ps1}");
        assert!(
            ps1.contains("$path = \"C:"),
            "the target must be an absolute path:\n{ps1}"
        );

        let cmd = read(&shims_dir.join("concfg.cmd"));
        assert!(cmd.contains("-Force %*"), "cmd body:\n{cmd}");
        assert!(cmd.contains("pwsh"), "cmd body:\n{cmd}");

        let sh = read(&shims_dir.join("concfg"));
        assert!(sh.starts_with("#!/bin/sh"), "posix body:\n{sh}");
        assert!(sh.contains("-Force \"$@\""), "posix body:\n{sh}");
    }

    #[test]
    fn jar_and_py_targets_carry_args() {
        let dir = tempfile::tempdir().unwrap();

        let (app_dir, shims_dir) = layout_file(dir.path(), "bfg", "bfg.jar");
        create_shims(
            &[entry("bfg.jar", "bfg", Some(&["--noisy"]))],
            &app_dir,
            &shims_dir,
        )
        .unwrap();
        let jar = read(&shims_dir.join("bfg.cmd"));
        assert!(
            jar.contains("java -jar") && jar.contains("--noisy %*"),
            "{jar}"
        );
        assert!(
            jar.contains("@pushd"),
            "the jar's directory must be the cwd: {jar}"
        );

        let (app_dir, shims_dir) = layout_file(dir.path(), "tool", "tool.py");
        create_shims(
            &[entry("tool.py", "tool", Some(&["-v"]))],
            &app_dir,
            &shims_dir,
        )
        .unwrap();
        let py = read(&shims_dir.join("tool.cmd"));
        assert!(py.contains("python") && py.contains("-v %*"), "{py}");
        let py_sh = read(&shims_dir.join("tool"));
        assert!(py_sh.contains("-v \"$@\""), "{py_sh}");
    }

    /// `Path::with_extension("cmd")` would turn `python3.11` into `python3.cmd`.
    /// The suffix has to be appended, not substituted.
    #[test]
    fn a_dotted_shim_name_keeps_everything_after_the_first_dot() {
        let dir = tempfile::tempdir().unwrap();
        let (app_dir, shims_dir) = layout_file(dir.path(), "python", "python.exe");

        create_shims(
            &[entry("python.exe", "python3.11", None)],
            &app_dir,
            &shims_dir,
        )
        .unwrap();

        assert!(shims_dir.join("python3.11.shim").is_file());
        assert!(shims_dir.join("python3.11.exe").is_file());
        assert!(
            !shims_dir.join("python3.shim").exists(),
            "the last extension must not be replaced"
        );

        remove_shim("python3.11", &shims_dir).unwrap();
        assert!(!shims_dir.join("python3.11.shim").exists());
        assert!(!shims_dir.join("python3.11.exe").exists());
    }

    /// The bug this pins: `canonicalize` resolves the `current` junction, so every
    /// shim was written against `apps/<app>/<version>/...` and a later update left
    /// it pointing at a directory that had just been deleted.
    #[test]
    fn the_current_junction_is_recorded_as_written() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().join("apps").join("demo");
        let version_dir = app_dir.join("1.0.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("tool.exe"), b"MZ").unwrap();
        crate::infra::fs::create_junction(&version_dir, &app_dir.join("current")).unwrap();
        let shims_dir = dir.path().join("shims");
        std::fs::create_dir_all(&shims_dir).unwrap();

        create_shims(
            &[entry("tool.exe", "tool", None)],
            &app_dir.join("current"),
            &shims_dir,
        )
        .unwrap();

        let content = read(&shims_dir.join("tool.shim"));
        assert!(
            content.contains(r"current\tool.exe"),
            "the shim must follow the junction, not the version directory:\n{content}"
        );
        assert!(
            !content.contains("1.0.0"),
            "the version directory must not be baked in:\n{content}"
        );
    }
}
