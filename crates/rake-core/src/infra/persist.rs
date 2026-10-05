use std::path::Path;

use crate::Result;

#[derive(Debug, Clone)]
pub struct PersistEntry {
    pub source: String,
    pub target: String,
}

pub fn parse_persist(
    persist: &rake_domain::one_or_many::OneOrMany<rake_domain::one_or_many::OneOrMany<String>>,
) -> Vec<PersistEntry> {
    let mut entries = Vec::new();
    for item in persist.iter() {
        let parts: Vec<&String> = item.iter().collect();
        if parts.is_empty() {
            continue;
        }
        let source = parts[0].clone();
        let target = parts
            .get(1)
            .map(|s| (*s).clone())
            .unwrap_or_else(|| source.clone());
        entries.push(PersistEntry { source, target });
    }
    entries
}

/// Apply persist: create junctions (dirs) or hardlinks (files) from app_dir/<source> ← persist_dir/<target>.
///
/// Both names come from the manifest's `persist` field, which is untrusted input
/// from a bucket. They are validated before anything touches the filesystem:
/// `Path::join` does not constrain them, so `"..\..\Startup\evil"` would otherwise
/// create a junction — or move a user's data — outside `apps/` and `persist/`.
/// `bin` has the same exposure and is guarded the same way.
///
/// The four cases mirror `persist_data` in Scoop's `lib/install.ps1`:
///
/// - persist has data, the new version ships its own — rename the shipped copy
///   to `<name>.original` and link persist back in. Scoop keeps that backup
///   deliberately, so a package's default config stays recoverable.
/// - persist has data, nothing shipped — just link it back.
/// - persist is empty, the version shipped data — move it into persist, then link.
/// - neither — create an empty persist directory and link it, so the app has
///   somewhere to write. A manifest that needs a *file* here must create it in
///   `pre_install`, which is what Scoop's comment says.
pub fn apply(entries: &[PersistEntry], app_dir: &Path, persist_dir: &Path) -> Result<()> {
    for entry in entries {
        validate(entry)?;
        let source = app_dir.join(trim_trailing_separators(&entry.source));
        let target = persist_dir.join(trim_trailing_separators(&entry.target));

        if target.exists() {
            if source.exists() || source.is_symlink() {
                // The new version brought its own copy. Keep it as `<name>.original`
                // rather than deleting it, so nothing shipped is lost.
                let backup = original_backup(&source);
                clear_path(&backup)?;
                std::fs::rename(&source, &backup)?;
            }
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            link_path(&target, &source)?;
        } else if source.exists() {
            // First install — move existing data to persist, then link.
            if let Some(parent) = target.parent() {
                crate::infra::fs::ensure_dir(parent)?;
            }
            std::fs::rename(&source, &target)?;
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            link_path(&target, &source)?;
        } else {
            // Neither exists — create target dir, link empty dir.
            crate::infra::fs::ensure_dir(&target)?;
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            link_path(&target, &source)?;
        }
    }
    Ok(())
}

/// Scoop trims trailing slashes before building the paths (`install.ps1:458`), so
/// `persist: ["config/"]` refers to the same place as `persist: ["config"]`.
fn trim_trailing_separators(name: &str) -> &str {
    name.trim_end_matches(['/', '\\'])
}

/// `<source>.original` — Scoop's name for the copy a new version shipped alongside
/// the user's persisted data.
fn original_backup(source: &Path) -> std::path::PathBuf {
    let mut name = source.as_os_str().to_os_string();
    name.push(".original");
    std::path::PathBuf::from(name)
}

/// Remove whatever sits at `path`, without ever following a junction into the data
/// it points at.
///
/// `remove_dir` takes a junction entry itself and fails on a real non-empty
/// directory, so trying it first is what keeps this from reaching persist data. Only
/// if that fails do we treat the path as a genuine directory and recurse.
fn clear_path(path: &Path) -> Result<()> {
    if !path.exists() && !path.is_symlink() {
        return Ok(());
    }
    if crate::infra::fs::remove_symlink(path).is_ok() {
        return Ok(());
    }
    if path.is_dir() {
        remove_dir_all::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Unlink persist junctions/symlinks/hardlinks (remove whatever is at source).
pub fn unlink(entries: &[PersistEntry], app_dir: &Path) -> Result<()> {
    for entry in entries {
        validate(entry)?;
        let source = app_dir.join(&entry.source);
        if source.exists() || source.is_symlink() {
            crate::infra::fs::remove_symlink(&source)?;
        }
    }
    Ok(())
}

/// Refuse a persist entry that names a path outside its directory.
fn validate(entry: &PersistEntry) -> Result<()> {
    crate::infra::fs::validate_relative_path("persist source", &entry.source)?;
    crate::infra::fs::validate_relative_path("persist target", &entry.target)?;
    Ok(())
}

/// Create a junction (dir) or hardlink (file) from target → source.
fn link_path(target: &Path, source: &Path) -> Result<()> {
    if target.is_dir() {
        crate::infra::fs::create_junction(target, source)?;
    } else {
        std::fs::hard_link(target, source)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::one_or_many::OneOrMany;
    use std::path::PathBuf;

    fn parse(json: &str) -> Vec<PersistEntry> {
        let bin: OneOrMany<OneOrMany<String>> = serde_json::from_str(json).unwrap();
        parse_persist(&bin)
    }

    #[test]
    fn parses_source_and_target() {
        let entries = parse(r#"[["config","userdata"]]"#);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "config");
        assert_eq!(entries[0].target, "userdata");
    }

    #[test]
    fn target_defaults_to_source() {
        let entries = parse(r#"[["config"]]"#);
        assert_eq!(entries[0].target, "config");
    }

    #[test]
    fn skips_empty_entries() {
        assert!(parse(r#"[[]]"#).is_empty());
    }

    fn apply_one(entry: PersistEntry) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();
        let _ = apply(std::slice::from_ref(&entry), &app_dir, &persist_dir);
        (tmp, app_dir, persist_dir)
    }

    /// First install: existing data in the app directory is moved into persist and
    /// linked back, so an update cannot lose it.
    #[test]
    fn first_install_moves_existing_data_into_persist() {
        let entry = PersistEntry {
            source: "config".to_owned(),
            target: "config".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(app_dir.join("config")).unwrap();
        std::fs::write(app_dir.join("config").join("user.cfg"), b"mine").unwrap();

        apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).unwrap();

        let moved = persist_dir.join("config").join("user.cfg");
        assert!(moved.is_file(), "data should live in persist");
        assert_eq!(std::fs::read(&moved).unwrap(), b"mine");

        // And it is reachable again through the app directory.
        let back = app_dir.join("config").join("user.cfg");
        assert!(back.is_file(), "app dir should link back to persist");
        assert_eq!(std::fs::read(&back).unwrap(), b"mine");
    }

    /// The upgrade path: persist already holds the user's data and the new version
    /// ships its own copy of the same directory. The user's data must survive and be
    /// reachable again, and the shipped copy must be kept aside rather than lost.
    ///
    /// This is the case that used to fail with `AlreadyExists`: the old code tried to
    /// strip `source` with `remove_symlink`, which cannot touch a real directory, and
    /// then asked for a junction on a path that was still occupied.
    #[test]
    fn shipped_config_is_set_aside_and_persist_relinked() {
        let entry = PersistEntry {
            source: "config".to_owned(),
            target: "config".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");

        std::fs::create_dir_all(persist_dir.join("config")).unwrap();
        std::fs::write(persist_dir.join("config").join("user.cfg"), b"mine").unwrap();

        // The new version ships a config directory of its own, which is non-empty.
        std::fs::create_dir_all(app_dir.join("config")).unwrap();
        std::fs::write(app_dir.join("config").join("default.cfg"), b"shipped").unwrap();

        apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).unwrap();

        // The user's data survives, and is reachable through the app directory.
        assert_eq!(
            std::fs::read(persist_dir.join("config").join("user.cfg")).unwrap(),
            b"mine"
        );
        assert_eq!(
            std::fs::read(app_dir.join("config").join("user.cfg")).unwrap(),
            b"mine",
            "the app directory must now be a link into persist"
        );

        // The shipped copy is kept aside under Scoop's name rather than deleted.
        let backup = app_dir.join("config.original");
        assert!(
            backup.is_dir(),
            "the shipped copy should be kept as config.original"
        );
        assert_eq!(
            std::fs::read(backup.join("default.cfg")).unwrap(),
            b"shipped",
            "and must not be empty or corrupted"
        );

        // The shipped default must not have leaked into the user's persist data.
        assert!(
            !persist_dir.join("config").join("default.cfg").exists(),
            "the shipped default must not displace user data"
        );
    }

    /// A second upgrade over the same `config.original` must not fail: `Move-Item
    /// -Force` overwrites, and so must this.
    #[test]
    fn a_previous_original_backup_is_replaced() {
        let entry = PersistEntry {
            source: "config".to_owned(),
            target: "config".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(persist_dir.join("config")).unwrap();
        std::fs::create_dir_all(app_dir.join("config.original")).unwrap();
        std::fs::write(app_dir.join("config.original").join("stale"), b"old").unwrap();

        std::fs::create_dir_all(app_dir.join("config")).unwrap();
        std::fs::write(app_dir.join("config").join("fresh"), b"new").unwrap();

        apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).unwrap();

        let backup = app_dir.join("config.original");
        assert!(!backup.join("stale").exists(), "stale backup replaced");
        assert_eq!(std::fs::read(backup.join("fresh")).unwrap(), b"new");
    }

    /// A manifest may name the source with a trailing slash; Scoop trims it, so both
    /// spellings must end up at the same place rather than one creating `config\` and
    /// the other `config`.
    #[test]
    fn trailing_separators_are_trimmed_like_scoop() {
        let entry = PersistEntry {
            source: "config\\".to_owned(),
            target: "config/".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(app_dir.join("config")).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();
        std::fs::write(app_dir.join("config").join("a.cfg"), b"x").unwrap();

        apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).unwrap();

        assert_eq!(
            std::fs::read(persist_dir.join("config").join("a.cfg")).unwrap(),
            b"x"
        );
        assert!(
            app_dir.join("config").join("a.cfg").is_file(),
            "and it links back"
        );
    }

    /// The user's data survives, and is reachable again, when the new version ships
    /// nothing at all.
    #[test]
    fn existing_persist_data_is_relinked_and_preserved() {
        let entry = PersistEntry {
            source: "config".to_owned(),
            target: "config".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(persist_dir.join("config")).unwrap();
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(persist_dir.join("config").join("keep.cfg"), b"precious").unwrap();

        apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).unwrap();

        assert_eq!(
            std::fs::read(persist_dir.join("config").join("keep.cfg")).unwrap(),
            b"precious"
        );
        assert_eq!(
            std::fs::read(app_dir.join("config").join("keep.cfg")).unwrap(),
            b"precious",
            "and it is reachable again through the app directory"
        );
    }

    /// Neither side exists: create the persist directory and link an empty one, so the
    /// app has somewhere to write.
    #[test]
    fn missing_on_both_sides_creates_persist_dir() {
        let entry = PersistEntry {
            source: "config".to_owned(),
            target: "config".to_owned(),
        };
        let (_tmp, app_dir, persist_dir) = apply_one(entry);
        assert!(persist_dir.join("config").is_dir());
        assert!(app_dir.join("config").is_dir());
    }

    /// A file gets a hardlink, not a directory junction.
    #[test]
    fn file_entries_are_hardlinked() {
        let entry = PersistEntry {
            source: "settings.ini".to_owned(),
            target: "settings.ini".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();
        std::fs::write(app_dir.join("settings.ini"), b"cfg").unwrap();

        apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).unwrap();

        assert!(persist_dir.join("settings.ini").is_file());
        assert_eq!(std::fs::read(app_dir.join("settings.ini")).unwrap(), b"cfg");
    }

    /// Unlinking removes the link, never the persisted data. This is the difference
    /// between an uninstall and data loss.
    #[test]
    fn unlink_removes_the_link_but_keeps_the_data() {
        let entry = PersistEntry {
            source: "config".to_owned(),
            target: "config".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(app_dir.join("config")).unwrap();
        std::fs::write(app_dir.join("config").join("user.cfg"), b"mine").unwrap();

        apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).unwrap();
        unlink(std::slice::from_ref(&entry), &app_dir).unwrap();

        assert!(!app_dir.join("config").exists(), "link should be gone");
        assert!(
            persist_dir.join("config").join("user.cfg").is_file(),
            "user data must survive"
        );
    }

    #[test]
    fn unlink_is_safe_when_nothing_was_linked() {
        let entry = PersistEntry {
            source: "config".to_owned(),
            target: "config".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        std::fs::create_dir_all(&app_dir).unwrap();
        assert!(unlink(std::slice::from_ref(&entry), &app_dir).is_ok());
    }

    /// The hole this guard closes: a manifest could otherwise move data and create a
    /// junction outside apps/ and persist/.
    #[test]
    fn refuses_source_escaping_the_app_directory() {
        let entry = PersistEntry {
            source: "..\\..\\..\\..\\Startup\\pwned".to_owned(),
            target: "same".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("evil").join("current");
        let persist_dir = tmp.path().join("persist").join("evil");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();

        let outside = tmp.path().join("Startup").join("pwned");

        assert!(apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).is_err());
        assert!(
            !outside.exists(),
            "nothing may be created outside the app directory"
        );

        assert!(unlink(std::slice::from_ref(&entry), &app_dir).is_err());
    }

    #[test]
    fn refuses_target_escaping_the_persist_directory() {
        let entry = PersistEntry {
            source: "config".to_owned(),
            target: "..\\..\\evil-target".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("evil").join("current");
        let persist_dir = tmp.path().join("persist").join("evil");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();

        assert!(apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).is_err());
    }

    #[test]
    fn refuses_absolute_names() {
        let entry = PersistEntry {
            source: r"C:\Windows\System32\evil".to_owned(),
            target: "config".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("evil").join("current");
        let persist_dir = tmp.path().join("persist").join("evil");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();
        assert!(apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).is_err());
    }

    #[test]
    fn allows_nested_relative_paths() {
        let entry = PersistEntry {
            source: "sub\\dir\\config".to_owned(),
            target: "config".to_owned(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("apps").join("demo").join("current");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(app_dir.join("sub").join("dir").join("config")).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();

        assert!(apply(std::slice::from_ref(&entry), &app_dir, &persist_dir).is_ok());
        assert!(persist_dir.join("config").is_dir());
    }
}
