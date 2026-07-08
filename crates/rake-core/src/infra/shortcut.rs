use std::path::{Path, PathBuf};

use crate::Result;

#[derive(Debug, Clone)]
pub struct ShortcutEntry {
    pub target: String,
    pub name: String,
    pub arguments: Option<String>,
    pub icon: Option<String>,
}

/// Create Start Menu shortcuts for a Scoop app.
#[cfg(windows)]
pub fn create_shortcuts(entries: &[ShortcutEntry], version_dir: &Path, global: bool) -> Result<()> {
    let folder = shortcut_folder(global)?;
    for entry in entries {
        create_single_shortcut(entry, version_dir, &folder)?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn create_shortcuts(
    _entries: &[ShortcutEntry],
    _version_dir: &Path,
    _global: bool,
) -> Result<()> {
    Ok(())
}

/// See the identical helper and rationale in infra/shim.rs — manifest
/// `shortcuts` names must be validated the same way `bin` names are,
/// since they are joined onto `start_menu_dir` the same unsafe way.
#[cfg(windows)]
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

#[cfg(windows)]
fn create_single_shortcut(
    entry: &ShortcutEntry,
    version_dir: &Path,
    start_menu_dir: &Path,
) -> Result<()> {
    validate_manifest_name(&entry.name)?;
    let target = version_dir.join(&entry.target);
    if !target.exists() {
        return Err(crate::Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Shortcut target not found: {}", target.display()),
        )));
    }

    let target_abs = target.canonicalize()?;
    let working_dir = target_abs
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let shortcut_name = Path::new(&entry.name);
    let shortcut_file = if let Some(parent) = shortcut_name.parent() {
        let dir = start_menu_dir.join(parent);
        std::fs::create_dir_all(&dir)?;
        dir.join(
            shortcut_name
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("shortcut"),
        )
        .with_extension("lnk")
    } else {
        start_menu_dir
            .join(
                shortcut_name
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("shortcut"),
            )
            .with_extension("lnk")
    };

    let target_str = target_abs.to_string_lossy().to_string();

    let mut script = String::new();
    script.push_str("$s = New-Object -ComObject WScript.Shell; ");
    script.push_str(&format!(
        "$c = $s.CreateShortcut('{}'); ",
        shortcut_file.to_string_lossy().replace('\'', "''")
    ));
    script.push_str(&format!(
        "$c.TargetPath = '{}'; ",
        target_str.replace('\'', "''")
    ));
    script.push_str(&format!(
        "$c.WorkingDirectory = '{}'; ",
        working_dir.replace('\'', "''")
    ));
    if let Some(ref args) = entry.arguments
        && !args.is_empty()
    {
        script.push_str(&format!("$c.Arguments = '{}'; ", args.replace('\'', "''")));
    }
    if let Some(ref icon) = entry.icon {
        let icon_path = version_dir.join(icon);
        if icon_path.exists() {
            script.push_str(&format!(
                "$c.IconLocation = '{}'; ",
                icon_path
                    .canonicalize()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
                    .replace('\'', "''")
            ));
        }
    }
    script.push_str("$c.Save()");

    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .map_err(|e| {
            crate::Error::Io(std::io::Error::other(format!("powershell shortcut: {e}")))
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "shortcut creation failed: {stderr}"
        ))));
    }

    Ok(())
}

/// Remove Start Menu shortcuts for a Scoop app.
#[cfg(windows)]
pub fn remove_shortcuts(entries: &[ShortcutEntry], global: bool) -> Result<()> {
    let folder = shortcut_folder(global)?;
    for entry in entries {
        let shortcut_file = shortcut_path(&entry.name, &folder);
        if shortcut_file.exists() {
            std::fs::remove_file(&shortcut_file)?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn remove_shortcuts(_entries: &[ShortcutEntry], _global: bool) -> Result<()> {
    Ok(())
}

/// Full path to the .lnk file for a given shortcut name.
fn shortcut_path(name: &str, start_menu_dir: &Path) -> PathBuf {
    let shortcut_name = Path::new(name);
    if let Some(parent) = shortcut_name.parent() {
        start_menu_dir
            .join(parent)
            .join(
                shortcut_name
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("shortcut"),
            )
            .with_extension("lnk")
    } else {
        start_menu_dir
            .join(
                shortcut_name
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("shortcut"),
            )
            .with_extension("lnk")
    }
}

#[cfg(windows)]
fn shortcut_folder(global: bool) -> Result<PathBuf> {
    let folder = if global {
        std::env::var("ALLUSERSPROFILE")
            .map(|p| PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs\Scoop Apps"))
            .map_err(|_| crate::Error::Io(std::io::Error::other("ALLUSERSPROFILE not set")))?
    } else {
        let appdata = std::env::var("APPDATA")
            .map_err(|_| crate::Error::Io(std::io::Error::other("APPDATA not set")))?;
        PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Scoop Apps")
    };
    std::fs::create_dir_all(&folder)?;
    Ok(folder)
}
