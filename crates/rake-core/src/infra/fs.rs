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

/// Reject a manifest-supplied name that could escape the directory it will be
/// joined onto.
///
/// Manifest `bin` and `persist` entries are untrusted third-party input, and
/// `Path::join` does not constrain them: joining `"..\..\Startup\evil"` onto an
/// app directory yields a path that still *reads* as being inside it, because the
/// `..` components are unresolved. `create_dir_all` then happily creates the
/// directory outside, and a junction or a data move follows.
///
/// Comparing path strings against a prefix is therefore not enough — the check has
/// to look at the components themselves, which is what this does.
///
/// `is_absolute()` alone is *also* not enough on Windows: `/etc/passwd` has no drive
/// prefix, so it is not "absolute", yet joining it onto `C:\apps\demo` yields
/// `C:/etc/passwd`, which escapes. Rooted paths are therefore rejected as well, by
/// refusing every component that is not a plain `Normal`.
pub fn validate_relative_path(field: &str, name: &str) -> Result<()> {
    let path = Path::new(name);

    let escapes = path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)));

    if escapes {
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "manifest supplied an unsafe {field}: '{name}' (absolute paths, rooted paths and '..' are not allowed)"
        ))));
    }

    Ok(())
}

#[cfg(test)]
mod path_guard_tests {
    use super::*;

    #[test]
    fn refuses_parent_dir() {
        assert!(validate_relative_path("bin name", "..\\..\\Startup\\evil").is_err());
        assert!(validate_relative_path("persist source", "../../etc/passwd").is_err());
        assert!(validate_relative_path("persist target", "a/../../b").is_err());
    }

    #[test]
    fn refuses_absolute() {
        assert!(validate_relative_path("persist source", "C:\\Windows\\System32\\evil").is_err());
    }

    /// A rooted path is not `is_absolute()` on Windows — it has no drive prefix — yet
    /// joining it onto `C:\apps\demo` produces `C:/etc/passwd`, which escapes. Found by
    /// this test rather than by reading, so it is worth stating explicitly.
    #[test]
    fn refuses_rooted_paths_that_are_not_absolute() {
        assert!(
            !Path::new("/etc/passwd").is_absolute(),
            "premise of the test"
        );
        assert!(validate_relative_path("persist source", "/etc/passwd").is_err());
        assert!(validate_relative_path("persist source", r"\Windows\evil").is_err());
        assert!(validate_relative_path("persist source", "sub/../../escape").is_err());
    }

    #[test]
    fn refuses_unc_prefix() {
        assert!(validate_relative_path("persist source", r"\\server\share\evil").is_err());
    }

    /// The reason a string-prefix check is not enough: the joined path still starts
    /// with the base directory as a string while resolving outside it.
    #[test]
    fn joined_path_still_looks_contained() {
        let base = Path::new("/root/apps/evil/current");
        let joined = base.join("..\\..\\..\\Startup\\pwned");
        assert!(
            joined.starts_with(base),
            "premise of the test: the string prefix matches"
        );
        assert!(
            validate_relative_path("persist source", "..\\..\\..\\Startup\\pwned").is_err(),
            "yet the component check still refuses it"
        );
    }

    #[test]
    fn accepts_ordinary_relative_paths() {
        for good in [
            "git",
            "config",
            "sub\\dir\\file",
            "a/b/c",
            "file.with.dots",
            "..leading-dots-name",
            "name..",
        ] {
            assert!(
                validate_relative_path("persist source", good).is_ok(),
                "{good} should be allowed"
            );
        }
    }
}
