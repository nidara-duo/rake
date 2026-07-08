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

/// Find the shim binary (shim.exe / rake-shim-bin.exe) next to the current executable.
fn find_shim_bin() -> Option<PathBuf> {
    let self_dir = std::env::current_exe().ok()?.parent()?.to_owned();
    for name in &["shim.exe", "rake-shim-bin.exe"] {
        let path = self_dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

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
pub fn create_shim(target: &Path, name: &str, app_name: &str, shims_dir: &Path) -> Result<()> {
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
            create_exe_shim(&target, &shim_base)?;
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
                create_exe_shim(&target, &shim_base)?;
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
fn create_exe_shim(target: &Path, shim_base: &Path) -> Result<()> {
    let shim_exe_path = shim_base.with_extension("exe");
    let shim_file_path = shim_base.with_extension("shim");

    // Copy shim.exe to the shim name
    if let Some(shim_bin) = find_shim_bin() {
        std::fs::copy(&shim_bin, &shim_exe_path)?;
    } else {
        // Fallback: write a basic cmd wrapper
        let cmd_path = shim_base.with_extension("cmd");
        let cmd_content = format!("@\"{target}\" %*\r\n", target = target.display());
        std::fs::write(&cmd_path, cmd_content)?;
        return Ok(());
    }

    // Write .shim metadata (Scoop-compatible format)
    let mut shim_content = String::new();
    shim_content.push_str(&format!("path = \"{}\"\r\n", target.display()));
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
        create_shim(&target, &entry.name, app_name, shims_dir)?;
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
}
