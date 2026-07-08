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

pub fn bucket_add(session: &Session, name: &str, remote_url: &str) -> Result<()> {
    let _guard = session.write_lock()?;
    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .ok_or_else(|| crate::Error::Config("root_path not set".to_owned()))?;

    let bucket_dir = root.join("buckets").join(name);

    if bucket_dir.exists() {
        return Err(crate::Error::Domain(
            rake_domain::Error::BucketAlreadyExists(name.to_owned()),
        ));
    }

    let url = if remote_url.is_empty() {
        BUILTIN_BUCKETS
            .iter()
            .find(|&&(n, _)| n == name)
            .map(|&(_, url)| url)
            .ok_or_else(|| {
                crate::Error::Domain(rake_domain::Error::BucketNotFound(name.to_owned()))
            })?
    } else {
        remote_url
    };

    fs::ensure_dir(&root.join("buckets"))?;

    let fut = crate::infra::git::ExternalGit.clone(url, &bucket_dir);
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
