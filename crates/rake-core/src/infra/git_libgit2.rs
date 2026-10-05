//! Git built on libgit2, with the system `git` binary as a fallback.
//!
//! Buckets are git repositories, so Rake needs Git. Requiring users to install it
//! separately made a freshly installed Rake unusable: the only way to obtain a bucket is
//! to clone one, and cloning needs Git. `libgit2` is already linked into the binary for
//! status checks, and on Windows it speaks HTTPS through WinHTTP — which means it uses
//! the Windows certificate store and needs no CA bundle shipped alongside.
//!
//! The system binary is kept as a fallback because it handles some environments better,
//! notably corporate proxies, where its proxy configuration differs from WinHTTP's.

use std::path::Path;

use async_trait::async_trait;
use git2::{Cred, FetchOptions, RemoteCallbacks, Repository, ResetType};

use crate::Result;
use crate::infra::git::{ExternalGit, GitService};

/// Run a blocking libgit2 call off the async runtime.
async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => Err(crate::Error::Git(e.to_string())),
    }
}

/// Anonymous callbacks: buckets are public repositories, so no credentials are needed.
///
/// libgit2's default credential handles anonymous access, but it rejects some HTTPS
/// setups, so a bare username is offered as the second attempt.
fn callbacks() -> RemoteCallbacks<'static> {
    let mut cb = RemoteCallbacks::new();
    cb.credentials(|_url, username, _allowed| {
        Cred::default().or_else(|_| Cred::username(username.unwrap_or("git")))
    });
    cb
}

fn fetch_options() -> FetchOptions<'static> {
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(callbacks());
    opts
}

/// Clone with a depth of one.
///
/// Buckets are read-only manifest sources that are only ever pulled, so full history is
/// pure waste.
fn shallow_clone(url: &str, path: &Path) -> Result<()> {
    let mut opts = fetch_options();
    opts.depth(1);

    git2::build::RepoBuilder::new()
        .fetch_options(opts)
        .clone(url, path)
        .map(|_| ())
        .map_err(|e| crate::Error::Git(e.to_string()))?;

    Ok(())
}

fn open_repo(path: &Path) -> Result<Repository> {
    Repository::open(path).map_err(|e| crate::Error::Git(e.to_string()))
}

/// Refuse to sync a bucket whose working tree has uncommitted changes.
fn ensure_clean(repo: &Repository) -> Result<()> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(false).include_ignored(false);

    let dirty = repo
        .statuses(Some(&mut opts))
        .map_err(|e| crate::Error::Git(e.to_string()))?
        .iter()
        .any(|s| {
            s.status().is_wt_modified() || s.status().is_wt_new() || s.status().is_wt_deleted()
        });

    if dirty {
        return Err(crate::Error::Git(
            "bucket has local changes; commit or discard them before syncing".to_owned(),
        ));
    }
    Ok(())
}

/// Fetch every branch into local refs, then hard reset to HEAD.
///
/// Mirrors the `git fetch origin refs/heads/*:refs/heads/*` followed by
/// `git reset --hard HEAD` pair used to force a bucket back to the remote state,
/// discarding local edits.
fn fetch_all_then_reset(path: &Path) -> Result<()> {
    let repo = open_repo(path)?;
    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| crate::Error::Git(e.to_string()))?;

    let refspecs: Vec<String> = repo
        .branches(Some(git2::BranchType::Remote))
        .map_err(|e| crate::Error::Git(e.to_string()))?
        .flatten()
        .filter_map(|b| b.0.name().ok().flatten().map(str::to_owned))
        .filter_map(|name| {
            let short = name.strip_prefix("origin/")?;
            Some(format!("refs/remotes/origin/{short}:refs/heads/{short}"))
        })
        .collect();

    let spec_refs: Vec<&str> = if refspecs.is_empty() {
        vec!["refs/remotes/origin/*:refs/heads/*"]
    } else {
        refspecs.iter().map(String::as_str).collect()
    };

    let mut opts = fetch_options();
    remote
        .fetch(&spec_refs, Some(&mut opts), None)
        .map_err(|e| crate::Error::Git(e.to_string()))?;

    let head = repo.head().map_err(|e| crate::Error::Git(e.to_string()))?;
    let commit = head
        .peel(git2::ObjectType::Commit)
        .map_err(|e| crate::Error::Git(e.to_string()))?;
    repo.reset(&commit, ResetType::Hard, None)
        .map_err(|e| crate::Error::Git(e.to_string()))?;
    Ok(())
}

/// Git backed by libgit2 first, falling back to the `git` binary.
pub struct Git {
    fallback: ExternalGit,
}

impl Git {
    pub fn new() -> Self {
        Self {
            fallback: ExternalGit::new(),
        }
    }
}

impl Default for Git {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GitService for Git {
    async fn clone(&self, url: &str, path: &Path) -> Result<()> {
        let (owned_url, owned_path) = (url.to_owned(), path.to_owned());

        let result = blocking({
            let u = owned_url.clone();
            let p = owned_path.clone();
            move || shallow_clone(&u, &p)
        })
        .await;

        match result {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::debug!("libgit2 clone failed, falling back to git: {e}");
                // The fallback writes to the same destination, so clear whatever libgit2
                // managed to create first.
                let _ = std::fs::remove_dir_all(&owned_path);
                self.fallback.clone(&owned_url, &owned_path).await
            }
        }
    }

    async fn fetch(&self, path: &Path) -> Result<()> {
        let result = blocking({
            let p = path.to_owned();
            move || {
                let repo = open_repo(&p)?;
                let mut remote = repo
                    .find_remote("origin")
                    .map_err(|e| crate::Error::Git(e.to_string()))?;
                let mut opts = fetch_options();
                remote
                    .fetch(&[] as &[&str], Some(&mut opts), None)
                    .map_err(|e| crate::Error::Git(e.to_string()))
            }
        })
        .await;

        match result {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::debug!("libgit2 fetch failed, falling back to git: {e}");
                self.fallback.fetch(path).await
            }
        }
    }

    async fn pull(&self, path: &Path) -> Result<()> {
        let result = blocking({
            let p = path.to_owned();
            move || {
                let repo = open_repo(&p)?;
                let mut remote = repo
                    .find_remote("origin")
                    .map_err(|e| crate::Error::Git(e.to_string()))?;
                let mut opts = fetch_options();
                remote
                    .fetch(&[] as &[&str], Some(&mut opts), None)
                    .map_err(|e| crate::Error::Git(e.to_string()))?;

                // Buckets are read-only mirrors, so the sync is a checkout of the fetched
                // branch rather than a three-way merge. Refuse first if the tree has local
                // changes: `git pull` would refuse too, and silently discarding someone's
                // edits would be worse than reporting them.
                ensure_clean(&repo)?;

                let head = repo.head().map_err(|e| crate::Error::Git(e.to_string()))?;
                let branch = head
                    .shorthand()
                    .map(str::to_owned)
                    .unwrap_or_else(|| "master".to_owned());
                let target = format!("refs/remotes/origin/{branch}");
                let obj = repo
                    .revparse_single(&target)
                    .map_err(|e| crate::Error::Git(e.to_string()))?;

                let mut checkout = git2::build::CheckoutBuilder::new();
                checkout.force();
                repo.checkout_tree(&obj, Some(&mut checkout))
                    .map_err(|e| crate::Error::Git(e.to_string()))?;
                repo.set_head(&target)
                    .map_err(|e| crate::Error::Git(e.to_string()))?;
                Ok(())
            }
        })
        .await;

        match result {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::debug!("libgit2 pull failed, falling back to git: {e}");
                self.fallback.pull(path).await
            }
        }
    }

    async fn reset_hard(&self, path: &Path) -> Result<()> {
        let result = blocking({
            let p = path.to_owned();
            move || fetch_all_then_reset(&p)
        })
        .await;

        match result {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::debug!("libgit2 reset_hard failed, falling back to git: {e}");
                self.fallback.reset_hard(path).await
            }
        }
    }

    fn remote_url(&self, path: &Path) -> Result<Option<String>> {
        match Repository::open(path) {
            Ok(repo) => Ok(repo
                .find_remote("origin")
                .ok()
                .and_then(|r| r.url().map(str::to_owned))),
            Err(_) => self.fallback.remote_url(path),
        }
    }

    async fn remote_head_sha(&self, path: &Path, branch: &str) -> Result<Option<String>> {
        let result = tokio::task::spawn_blocking({
            let path = path.to_owned();
            let branch = branch.to_owned();
            move || -> Result<Option<String>> {
                let repo = open_repo(&path)?;
                let mut remote = match repo.find_remote("origin") {
                    Ok(r) => r,
                    Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
                    Err(e) => return Err(crate::Error::Git(e.to_string())),
                };

                remote
                    .connect_auth(git2::Direction::Fetch, Some(callbacks()), None)
                    .map_err(|e| crate::Error::Git(e.to_string()))?;
                // `Remote::list` takes no ref filter and returns every advertised
                // head, so filter locally.
                let wanted = format!("refs/heads/{branch}");
                let sha = remote
                    .list()
                    .map_err(|e| crate::Error::Git(e.to_string()))?
                    .iter()
                    .find(|head| head.name() == wanted)
                    .map(|head| head.oid().to_string());
                drop(remote);
                Ok(sha)
            }
        })
        .await
        .map_err(|e| crate::Error::Git(e.to_string()))?;

        match result {
            Ok(sha) => Ok(sha),
            Err(e) => {
                tracing::debug!("libgit2 ls_remote failed, falling back to git: {e}");
                self.fallback.remote_head_sha(path, branch).await
            }
        }
    }

    /// Always true: libgit2 is linked into the binary, so Git is available even on a
    /// machine where nothing has been installed.
    async fn is_installed(&self) -> bool {
        true
    }
}
