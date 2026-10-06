use crate::Result;
use crate::bucket::{BUILTIN_BUCKETS, Bucket, added_buckets};
use crate::infra::fs;
use crate::infra::git::GitService;
use crate::session::Session;

pub fn bucket_list(session: &Session) -> Result<Vec<Bucket>> {
    let mut buckets = added_buckets(session)?;
    buckets.sort_by_key(|b| b.name().to_owned());
    Ok(buckets)
}

pub fn bucket_list_known() -> Vec<(&'static str, &'static str)> {
    BUILTIN_BUCKETS.to_vec()
}

/// Which URL to clone from: the one given, or the builtin's when none was.
///
/// Split out so the lookup can be tested without a clone — `bucket_add` calls into a
/// blocking git clone and needs a Tokio runtime, which rules out a plain unit test.
fn resolve_bucket_url(name: &str, remote_url: &str) -> Result<String> {
    if !remote_url.is_empty() {
        return Ok(remote_url.to_owned());
    }
    BUILTIN_BUCKETS
        .iter()
        .find(|&&(n, _)| n == name)
        .map(|&(_, url)| url.to_owned())
        .ok_or_else(|| crate::Error::Domain(rake_domain::Error::BucketNotFound(name.to_owned())))
}

pub fn bucket_add(session: &Session, name: &str, remote_url: &str) -> Result<()> {
    let _guard = session.write_lock()?;

    // Git is provided by libgit2, which is linked into the binary, so there is nothing
    // to install first: a fresh Rake can clone its own bucket.
    let git = crate::infra::git_libgit2::Git::new();

    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .ok_or_else(|| crate::Error::Config("root_path not set".to_owned()))?;

    // The name is joined onto `<root>/buckets` unconstrained, so `..` or an absolute path
    // would clone a repository anywhere on the disk. Same guard as manifest `bin` and
    // `persist`, and for the same reason: `Path::join` does not constrain its argument.
    crate::infra::fs::validate_relative_path("bucket name", name)?;

    let bucket_dir = root.join("buckets").join(name);

    if bucket_dir.exists() {
        return Err(crate::Error::Domain(
            rake_domain::Error::BucketAlreadyExists(name.to_owned()),
        ));
    }

    let url = resolve_bucket_url(name, remote_url)?;

    fs::ensure_dir(&root.join("buckets"))?;

    let fut = git.clone(&url, &bucket_dir);
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))?;

    Ok(())
}

pub fn bucket_remove(session: &Session, name: &str) -> Result<()> {
    let _guard = session.write_lock()?;
    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .ok_or_else(|| crate::Error::Config("root_path not set".to_owned()))?;

    // Same reason as `bucket_add`, and worse here: this one deletes. `..` in a bucket name
    // would otherwise remove an arbitrary directory rather than a bucket.
    crate::infra::fs::validate_relative_path("bucket name", name)?;

    let bucket_dir = root.join("buckets").join(name);

    if !bucket_dir.exists() {
        return Err(crate::Error::Domain(rake_domain::Error::BucketNotFound(
            name.to_owned(),
        )));
    }

    fs::remove_dir(&bucket_dir)
}

pub fn bucket_hold(session: &Session, name: &str) -> Result<()> {
    let _guard = session.write_lock()?;
    let buckets = bucket_list(session)?;
    let bucket = buckets
        .iter()
        .find(|b| b.name() == name)
        .cloned()
        .ok_or_else(|| crate::Error::Domain(rake_domain::Error::BucketNotFound(name.to_owned())))?;

    let hold_path = bucket.path().join(".hold");
    if !hold_path.exists() {
        std::fs::File::create(hold_path)?;
    }
    Ok(())
}

pub fn bucket_unhold(session: &Session, name: &str) -> Result<()> {
    let _guard = session.write_lock()?;
    let buckets = bucket_list(session)?;
    let bucket = buckets
        .iter()
        .find(|b| b.name() == name)
        .cloned()
        .ok_or_else(|| crate::Error::Domain(rake_domain::Error::BucketNotFound(name.to_owned())))?;

    let hold_path = bucket.path().join(".hold");
    if hold_path.exists() {
        std::fs::remove_file(hold_path)?;
    }
    Ok(())
}

pub(crate) fn bucket_held_names_inner(session: &Session) -> Result<Vec<String>> {
    let buckets = bucket_list(session)?;
    Ok(buckets
        .into_iter()
        .filter(|b| b.is_held())
        .map(|b| b.name().to_owned())
        .collect())
}

pub fn bucket_held_names(session: &Session) -> Result<Vec<String>> {
    let _guard = session.read_lock()?;
    bucket_held_names_inner(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::config::Config;
    use std::path::{Path, PathBuf};

    fn session_at(root: &Path) -> Session {
        std::fs::create_dir_all(root.join("buckets")).unwrap();
        Session::from_config(Config {
            root_path: Some(root.to_path_buf()),
            ..Default::default()
        })
    }

    fn make_bucket(root: &Path, name: &str) -> PathBuf {
        let p = root.join("buckets").join(name);
        std::fs::create_dir_all(p.join("bucket")).unwrap();
        p
    }

    /// Names come from the command line and are joined onto `<root>/buckets` unconstrained.
    /// Without the guard, `..` places the directory outside the buckets folder entirely.
    #[test]
    fn bucket_add_refuses_a_name_that_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session_at(tmp.path());

        for evil in [
            "..\\..\\evil",
            "../../evil",
            r"C:\Windows\evil",
            r"\absolute\evil",
        ] {
            assert!(
                bucket_add(&session, evil, "https://example.invalid/r.git").is_err(),
                "{evil:?} must be refused"
            );
        }
        // Nothing was created anywhere.
        assert!(!tmp.path().join("evil").exists());
        assert!(!tmp.path().parent().unwrap().join("evil").exists());
    }

    /// The same hole on the deleting side, which is worse: `..` would remove an arbitrary
    /// directory rather than a bucket.
    #[test]
    fn bucket_remove_refuses_a_name_that_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let session = session_at(root);

        // Something valuable sitting next to `buckets`, which must survive.
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::write(root.join("apps").join("keepme.txt"), b"precious").unwrap();

        for evil in ["..\\apps", "../apps", "..\\..\\.."] {
            assert!(
                bucket_remove(&session, evil).is_err(),
                "{evil:?} must be refused"
            );
        }

        assert!(
            root.join("apps").join("keepme.txt").exists(),
            "nothing outside buckets/ may be touched"
        );
    }

    #[test]
    fn bucket_add_refuses_an_existing_bucket() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session_at(tmp.path());
        make_bucket(tmp.path(), "main");

        let err = bucket_add(&session, "main", "https://example.invalid/r.git")
            .unwrap_err()
            .to_string();
        assert!(err.to_lowercase().contains("exists"), "got: {err}");
    }

    /// Reached before any network access, so an unknown builtin name is reported rather
    /// than hanging on a clone attempt.
    #[test]
    fn bucket_add_without_a_url_needs_a_known_name() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session_at(tmp.path());
        assert!(bucket_add(&session, "nosuchbucket", "").is_err());
    }

    /// An empty URL means "look up the builtin", so a builtin name resolves and a custom
    /// one does not. An explicit URL always wins.
    #[test]
    fn url_resolution_prefers_the_given_one() {
        assert_eq!(
            resolve_bucket_url("main", "https://example.invalid/mine.git").unwrap(),
            "https://example.invalid/mine.git"
        );
        assert_eq!(
            resolve_bucket_url("main", "").unwrap(),
            "https://github.com/ScoopInstaller/Main",
            "an empty URL resolves against the builtin table"
        );
        assert!(resolve_bucket_url("nosuchbucket", "").is_err());
    }

    /// A builtin name must pass the same traversal guard a user-supplied one does,
    /// otherwise `rake bucket add main` would be rejected by its own validation.
    #[test]
    fn builtin_names_survive_the_guard() {
        for (name, _) in bucket_list_known() {
            assert!(
                crate::infra::fs::validate_relative_path("bucket name", name).is_ok(),
                "builtin bucket {name} would be rejected"
            );
        }
    }

    #[test]
    fn bucket_remove_deletes_the_bucket_and_only_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let session = session_at(root);
        let main = make_bucket(root, "main");
        make_bucket(root, "extras");

        bucket_remove(&session, "main").unwrap();

        assert!(!main.exists(), "the named bucket should be gone");
        assert!(root.join("buckets").join("extras").exists(), "others stay");
    }

    #[test]
    fn bucket_remove_reports_an_unknown_bucket() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session_at(tmp.path());
        assert!(bucket_remove(&session, "nosuchbucket").is_err());
    }

    #[test]
    fn hold_and_unhold_are_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session_at(tmp.path());
        make_bucket(tmp.path(), "main");

        assert!(bucket_held_names(&session).unwrap().is_empty());

        bucket_hold(&session, "main").unwrap();
        bucket_hold(&session, "main").unwrap(); // twice must not fail
        assert_eq!(
            bucket_held_names(&session).unwrap(),
            vec!["main".to_owned()]
        );

        bucket_unhold(&session, "main").unwrap();
        bucket_unhold(&session, "main").unwrap();
        assert!(bucket_held_names(&session).unwrap().is_empty());
    }

    #[test]
    fn hold_reports_an_unknown_bucket() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session_at(tmp.path());
        assert!(bucket_hold(&session, "nosuchbucket").is_err());
        assert!(bucket_unhold(&session, "nosuchbucket").is_err());
    }

    /// Listing is sorted, so output order does not depend on the filesystem.
    #[test]
    fn bucket_list_is_sorted_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session_at(tmp.path());
        for name in ["versions", "main", "extras"] {
            make_bucket(tmp.path(), name);
        }

        let names: Vec<_> = bucket_list(&session)
            .unwrap()
            .iter()
            .map(|b| b.name().to_owned())
            .collect();
        assert_eq!(names, vec!["extras", "main", "versions"]);
    }

    #[test]
    fn no_buckets_directory_yields_an_empty_list() {
        let tmp = tempfile::tempdir().unwrap();
        let session = Session::from_config(Config {
            root_path: Some(tmp.path().to_path_buf()),
            ..Default::default()
        });
        assert!(bucket_list(&session).unwrap().is_empty());
        assert!(bucket_held_names(&session).unwrap().is_empty());
    }

    /// Every builtin name must survive the same guard the user-supplied ones do.
    #[test]
    fn builtin_urls_are_https() {
        for (name, url) in bucket_list_known() {
            assert!(url.starts_with("https://"), "{name}: {url}");
        }
    }
}
