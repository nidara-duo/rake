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
pub fn apply(entries: &[PersistEntry], app_dir: &Path, persist_dir: &Path) -> Result<()> {
    for entry in entries {
        validate(entry)?;
        let source = app_dir.join(&entry.source);
        let target = persist_dir.join(&entry.target);

        if target.exists() {
            // Data exists from previous install — link it back
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            let _ = crate::infra::fs::remove_symlink(&source);
            link_path(&target, &source)?;
        } else if source.exists() {
            // First install — move existing data to persist, then link
            if let Some(parent) = target.parent() {
                crate::infra::fs::ensure_dir(parent)?;
            }
            std::fs::rename(&source, &target)?;
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            link_path(&target, &source)?;
        } else {
            // Neither exists — create target dir, link empty dir
            crate::infra::fs::ensure_dir(&target)?;
            crate::infra::fs::ensure_dir(source.parent().unwrap())?;
            link_path(&target, &source)?;
        }
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
    /// directory has no config yet, so persist is linked back in and the data must
    /// survive untouched.
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
