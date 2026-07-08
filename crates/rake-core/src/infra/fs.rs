use std::path::Path;

use crate::Result;

pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

pub fn remove_dir(path: &Path) -> Result<()> {
    if path.exists() {
        remove_dir_all::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn remove_symlink(path: &Path) -> Result<()> {
    if path.exists() || path.is_symlink() {
        // Scoop sets +R on junctions — clear read-only first via attrib
        let _ = std::process::Command::new("attrib")
            .args(["-R", "/L"])
            .arg(path)
            .output();
        std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path))?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn remove_symlink(path: &Path) -> Result<()> {
    if path.exists() || path.is_symlink() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn create_junction(target: &Path, link: &Path) -> Result<()> {
    junction::create(target, link).map_err(|e| crate::Error::Io(std::io::Error::other(e)))?;
    // Scoop sets +R on junctions to prevent accidental deletion
    let _ = std::process::Command::new("attrib")
        .args(["+R", "/L"])
        .arg(link)
        .output();
    Ok(())
}

#[cfg(unix)]
pub fn create_junction(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

pub fn empty_dir(path: &Path) -> Result<()> {
    if path.exists() {
        remove_dir_all::remove_dir_contents(path)?;
    }
    Ok(())
}

pub fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    ensure_dir(dest)?;
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.map_err(std::io::Error::other)?;
        let relative = entry.path().strip_prefix(src).unwrap();
        let target = dest.join(relative);
        if entry.file_type().is_dir() {
            ensure_dir(&target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
