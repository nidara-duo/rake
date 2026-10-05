use std::path::Path;

use crate::Result;

fn quote_powershell_string(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

/// Script hook context — variable substitution for `$dir`, `$persist_dir`,
/// `$original_dir`, `$version`.
pub struct HookContext<'a> {
    pub version_dir: &'a Path,
    pub persist_dir: &'a Path,
    pub original_dir: &'a Path,
    pub version: &'a str,

    /// Rake root and global root. Present only when the hook needs scoop's PowerShell
    /// library; `None` runs the script standalone.
    ///
    /// Manifest `installer.script` bodies call scoop functions (`Add-Path`,
    /// `Invoke-ExternalCommand`, `Expand-InnoArchive`, ...), which resolve only when
    /// scoop's `lib/` is dot-sourced. Callers pass the roots to enable that.
    pub scoop_roots: Option<(&'a Path, &'a Path)>,
}

impl<'a> HookContext<'a> {
    /// A hook that runs standalone, without scoop's PowerShell library.
    ///
    /// Right for `pre_install`, `post_install`, `pre_uninstall` and `post_uninstall`:
    /// those are usually plain PowerShell, and loading the library costs ~0.7s per run.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version_dir: &'a Path,
        persist_dir: &'a Path,
        original_dir: &'a Path,
        version: &'a str,
    ) -> Self {
        Self {
            version_dir,
            persist_dir,
            original_dir,
            version,
            scoop_roots: None,
        }
    }

    /// A hook that runs with scoop's PowerShell library loaded.
    ///
    /// Required for `installer.script` and `uninstaller.script`, whose bodies call
    /// scoop functions such as `Add-Path` and `Invoke-ExternalCommand`.
    #[allow(clippy::too_many_arguments)]
    pub fn with_scoop_lib(
        version_dir: &'a Path,
        persist_dir: &'a Path,
        original_dir: &'a Path,
        version: &'a str,
        root: &'a Path,
        global_root: &'a Path,
    ) -> Self {
        Self {
            version_dir,
            persist_dir,
            original_dir,
            version,
            scoop_roots: Some((root, global_root)),
        }
    }
}

/// Execute a list of PowerShell script lines (e.g. `pre_install`, `post_install`).
///
/// Writes the hook body to a temporary `.ps1` file with safely quoted variables,
/// then runs it via `powershell.exe -File`. This avoids command-line injection
/// from unquoted filesystem paths.
pub fn run_powershell_script(lines: &[String], ctx: &HookContext) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    let dir = quote_powershell_string(&ctx.version_dir.to_string_lossy());
    let persist_dir = quote_powershell_string(&ctx.persist_dir.to_string_lossy());
    let original_dir = quote_powershell_string(&ctx.original_dir.to_string_lossy());
    let version = quote_powershell_string(ctx.version);

    let script_body = if cfg!(windows) {
        lines.join("\r\n")
    } else {
        lines.join("\n")
    };

    // When scoop's library is requested, its prelude replaces these four assignments:
    // it defines the same variables and additionally loads the functions a manifest
    // hook is likely to call.
    let header = match ctx.scoop_roots {
        Some((root, global_root)) => {
            let lib = crate::infra::scoop_lib::ensure_library(root)?;
            crate::infra::scoop_lib::build_prelude(
                &lib,
                root,
                global_root,
                ctx.version_dir,
                ctx.persist_dir,
                ctx.original_dir,
                ctx.version,
            )
        }
        None => format!(
            "$dir = {dir}\r\n$persist_dir = {persist_dir}\r\n$original_dir = {original_dir}\r\n$version = {version}\r\n\r\n",
            dir = dir,
            persist_dir = persist_dir,
            original_dir = original_dir,
            version = version,
        ),
    };

    let content = format!("{header}{script_body}");

    let mut tmp = tempfile::Builder::new()
        .prefix("rake-hook-")
        .suffix(".ps1")
        .tempfile()
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("create hook script: {e}"))))?;

    std::io::Write::write_all(&mut tmp, content.as_bytes())
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("write hook script: {e}"))))?;

    // Persist the temp file on disk and hand the path to PowerShell.
    // It is auto-removed when `path` goes out of scope.
    let path = tmp.into_temp_path();
    let script_path = path.to_string_lossy();
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            script_path.as_ref(),
        ])
        .output();

    let out = match out {
        Ok(o) => o,
        Err(e) => {
            return Err(crate::Error::Io(std::io::Error::other(format!(
                "powershell script: {e}"
            ))));
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "script hook failed: {stderr}"
        ))));
    }

    Ok(())
}
