//! Cleanup of artefacts left behind by replacing Rake's own binary.
//!
//! Windows locks the image of a running process, so the running `rake.exe` can be
//! neither overwritten nor deleted. It can, however, be renamed aside, which frees the
//! original name for writing the replacement. The renamed file stays locked until this
//! process exits, so it is not removed in place — Scoop solves the same problem by
//! renaming its current directory to `old` and deleting that on the next run, and the
//! approach here mirrors that.
//!
//! `MoveFileEx` with `MOVEFILE_DELAY_UNTIL_REBOOT` was rejected because it needs
//! administrator rights, and `FileDispositionInfoEx` with POSIX semantics was refused
//! on the target system (win32err=24). Neither can delete a running image.

use std::path::{Path, PathBuf};

/// Suffix appended to the binary name while it is being replaced.
const STALE_SUFFIX: &str = ".old";

/// Name the binary is renamed to while it is being replaced.
pub fn stale_path_for(exe: &Path) -> Option<PathBuf> {
    let file_name = exe.file_name()?;
    let mut stale = file_name.to_os_string();
    stale.push(STALE_SUFFIX);
    Some(exe.with_file_name(stale))
}

/// Remove binaries left over from an earlier replacement.
///
/// A single removal attempt per name: the file is unlocked by the time any later Rake
/// runs, and if something still holds it there is nothing useful to do about it here.
/// Failures are ignored so a stale artefact can never block ordinary use.
pub fn sweep_stale_artifacts() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if let Some(stale) = stale_path_for(&exe) {
        let _ = std::fs::remove_file(stale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_name_is_derived_from_the_binary_name() {
        assert_eq!(
            stale_path_for(Path::new(r"C:\rake\bin\rake.exe")).unwrap(),
            PathBuf::from(r"C:\rake\bin\rake.exe.old")
        );
    }

    /// The binary may have been renamed, and the sweep must follow whatever it is
    /// actually called rather than assuming a fixed "rake.exe".
    #[test]
    fn stale_name_follows_a_renamed_binary() {
        assert_eq!(
            stale_path_for(Path::new(r"C:\tools\myrake.exe")).unwrap(),
            PathBuf::from(r"C:\tools\myrake.exe.old")
        );
    }

    #[test]
    fn no_stale_name_without_a_file_name() {
        assert_eq!(stale_path_for(Path::new(r"C:\")), None);
    }
}
