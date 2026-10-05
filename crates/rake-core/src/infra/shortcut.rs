use std::path::{Path, PathBuf};

use crate::Result;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct ShortcutEntry {
    pub target: String,
    pub name: String,
    pub arguments: Option<String>,
    pub icon: Option<String>,
}

/// Create Start Menu shortcuts for a Scoop app.
#[cfg(windows)]
pub fn create_shortcuts(
    entries: &[ShortcutEntry],
    version_dir: &Path,
    global: bool,
) -> Result<Vec<String>> {
    let folder = shortcut_folder(global)?;
    let mut warnings = Vec::new();
    for entry in entries {
        match create_single_shortcut(entry, version_dir, &folder) {
            Ok(()) => {}
            Err(e) => warnings.push(e.to_string()),
        }
    }
    Ok(warnings)
}

#[cfg(not(windows))]
pub fn create_shortcuts(
    _entries: &[ShortcutEntry],
    _version_dir: &Path,
    _global: bool,
) -> Result<Vec<String>> {
    Ok(Vec::new())
}

#[cfg(windows)]
fn create_single_shortcut(
    entry: &ShortcutEntry,
    version_dir: &Path,
    start_menu_dir: &Path,
) -> Result<()> {
    // Both names come from the manifest and both are joined onto a directory
    // unconstrained, so both need the shared guard — see infra/fs.rs for why
    // comparing against a prefix is not enough.
    crate::infra::fs::validate_relative_path("shortcut name", &entry.name)?;
    crate::infra::fs::validate_relative_path("shortcut target", &entry.target)?;
    let target = version_dir.join(&entry.target);
    if !target.exists() {
        let mut diag = format!(
            "Shortcut target not found: manifest declared '{}', resolved to {}",
            entry.target,
            target.display()
        );
        let entries: Vec<_> = WalkDir::new(version_dir)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() || e.file_type().is_dir())
            .take(20)
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();
        if entries.is_empty() {
            diag.push_str(&format!(
                " — directory {} is empty or missing",
                version_dir.display()
            ));
        } else {
            diag.push_str(" — version_dir contains (depth ≤2):");
            for e in entries {
                diag.push_str(&format!("\n  {}", e));
            }
        }
        return Err(crate::Error::Io(std::io::Error::other(diag)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_shortcut_target_returns_warning_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let version_dir = tmp.path();
        let entries = vec![ShortcutEntry {
            target: "nonexistent.exe".to_string(),
            name: "Foo".to_string(),
            arguments: None,
            icon: None,
        }];
        let result = create_shortcuts(&entries, version_dir, false);
        assert!(result.is_ok());
        let warnings = result.unwrap();
        assert_eq!(warnings.len(), 1);
        let w = &warnings[0];
        assert!(
            w.contains("nonexistent.exe"),
            "warning should mention the manifest-declared target, got: {w}"
        );
    }

    #[test]
    #[cfg(windows)]
    fn valid_shortcut_created_and_invalid_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let version_dir = tmp.path();
        fs::write(version_dir.join("real.exe"), b"").unwrap();

        let start_menu = shortcut_folder(false).unwrap();
        let _ = std::fs::remove_file(start_menu.join("Valid.lnk"));

        let entries = vec![
            ShortcutEntry {
                target: "real.exe".to_string(),
                name: "Valid".to_string(),
                arguments: None,
                icon: None,
            },
            ShortcutEntry {
                target: "missing.exe".to_string(),
                name: "Missing".to_string(),
                arguments: None,
                icon: None,
            },
        ];
        let result = create_shortcuts(&entries, version_dir, false);
        assert!(result.is_ok());
        let warnings = result.unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing.exe"));

        let valid_link = start_menu.join("Valid.lnk");
        assert!(valid_link.exists(), "valid shortcut should be created");

        // The Start Menu folder is the real one, so the test has to take its
        // shortcut back out again rather than leave it on the user's machine.
        std::fs::remove_file(&valid_link).unwrap();
    }

    /// A shortcut name is joined onto the Start Menu directory the same unsafe way a
    /// `bin` name is joined onto `shims`, so it gets the same guard.
    ///
    /// `create_shortcuts` reports a per-entry failure as a warning rather than an
    /// error, so the refusal shows up there — and, since the guard runs before
    /// anything is written, nothing reaches the Start Menu at all.
    #[test]
    #[cfg(windows)]
    fn shortcut_name_escaping_the_start_menu_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let version_dir = tmp.path().join("current");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("real.exe"), b"x").unwrap();
        let start_menu = shortcut_folder(false).unwrap();

        for bad in [
            "..\\..\\..\\Startup\\evil",
            "/Windows/evil",
            r"\\server\share\evil",
            r"C:\Windows\System32\evil",
        ] {
            let entries = vec![ShortcutEntry {
                target: "real.exe".to_string(),
                name: bad.to_string(),
                arguments: None,
                icon: None,
            }];
            let warnings = create_shortcuts(&entries, &version_dir, false).unwrap();
            assert_eq!(
                warnings.len(),
                1,
                "shortcut name {bad:?} should produce exactly one warning, got {warnings:?}"
            );
            assert!(
                warnings[0].contains("unsafe shortcut name"),
                "warning should name the reason, got: {}",
                warnings[0]
            );
        }

        assert!(
            !tmp.path().join("Startup").exists(),
            "nothing may be created outside the Start Menu directory"
        );
        assert!(
            fs::read_dir(&start_menu)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .all(|e| !e.file_name().to_string_lossy().contains("evil")),
            "no shortcut for the rejected names may appear in the Start Menu"
        );
    }

    /// The target is joined onto the version directory and was not validated at all
    /// before: `..` in it let a shortcut point anywhere on the disk.
    #[test]
    #[cfg(windows)]
    fn shortcut_target_escaping_the_version_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let version_dir = tmp.path().join("current");
        std::fs::create_dir_all(&version_dir).unwrap();
        // A real file outside the version directory, so only the guard can refuse it.
        std::fs::write(tmp.path().join("outside.exe"), b"x").unwrap();
        let start_menu = shortcut_folder(false).unwrap();
        let _ = fs::remove_file(start_menu.join("Escape.lnk"));

        let entries = vec![ShortcutEntry {
            target: "..\\outside.exe".to_string(),
            name: "Escape".to_string(),
            arguments: None,
            icon: None,
        }];
        let warnings = create_shortcuts(&entries, &version_dir, false).unwrap();

        assert_eq!(warnings.len(), 1, "got {warnings:?}");
        assert!(
            warnings[0].contains("unsafe shortcut target"),
            "warning should name the reason, got: {}",
            warnings[0]
        );
        assert!(
            !start_menu.join("Escape.lnk").exists(),
            "a shortcut pointing outside the version directory must not be created"
        );
    }
}
