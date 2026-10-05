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

/// Conventional fetch refspec, used when a remote has none configured.
///
/// Without it a bucket cloned by an older build never gets remote-tracking refs,
/// and every freshness check then reads nothing.
const DEFAULT_FETCH_REFSPEC: [&str; 1] = ["+refs/heads/*:refs/remotes/origin/*"];

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

/// Fast-forward a bucket onto its remote branch while keeping HEAD attached.
///
/// The local branch is moved to the fetched commit and then checked out, which
/// is what Scoop does with `checkout -B <branch> -t origin/<branch>` followed
/// by `reset --hard` (ethalon libexec/scoop-update.ps1:133-137).
///
/// The earlier version called `set_head("refs/remotes/origin/<branch>")`.
/// libgit2 treats any ref outside `refs/heads/*` as "not a branch" and writes a
/// bare SHA into `.git/HEAD`, detaching HEAD. That silently broke every later
/// `git pull` — including Scoop's own `rake update` neighbour, `scoop update`,
/// which fails with "You are not currently on a branch". Buckets must stay on
/// a branch: they are shared with Scoop, which pulls them.
fn sync_to_remote(path: &Path) -> Result<()> {
    let repo = open_repo(path)?;

    let mut remote = repo
        .find_remote("origin")
        .map_err(|e| crate::Error::Git(e.to_string()))?;

    let refspecs = remote
        .fetch_refspecs()
        .map_err(|e| crate::Error::Git(e.to_string()))?;
    let configured: Vec<String> = refspecs.iter().flatten().map(str::to_owned).collect();
    let refspecs: Vec<&str> = if configured.is_empty() {
        DEFAULT_FETCH_REFSPEC.to_vec()
    } else {
        configured.iter().map(String::as_str).collect()
    };

    let mut opts = fetch_options();
    remote
        .fetch(&refspecs, Some(&mut opts), None)
        .map_err(|e| crate::Error::Git(e.to_string()))?;

    // Refuse before touching anything: a merge would refuse too, and silently
    // discarding someone's edits to a bucket would be worse than reporting them.
    ensure_clean(&repo)?;

    // A bucket left in detached HEAD by an older build has no local branch to
    // move. Recover by attaching to the branch that matches the remote, which is
    // what `git checkout master` would do.
    let branch_name = match repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_owned))
    {
        Some(name) if name != "HEAD" => name,
        _ => default_branch(&repo, &refspecs)?,
    };

    let target = format!("refs/remotes/origin/{branch_name}");
    let commit = repo
        .revparse_single(&target)
        .map_err(|e| crate::Error::Git(e.to_string()))?;

    // Move the local branch itself, not HEAD: this is what keeps the working
    // tree and HEAD in sync with the remote.
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.reset(&commit, ResetType::Hard, Some(&mut checkout))
        .map_err(|e| crate::Error::Git(e.to_string()))?;

    repo.set_head(&format!("refs/heads/{branch_name}"))
        .map_err(|e| crate::Error::Git(e.to_string()))?;

    Ok(())
}

/// Best-effort branch name for a detached or unborn checkout.
///
/// Prefers what `refs/remotes/origin/HEAD` points at, then whatever single
/// branch the fetch refspecs mention. Buckets are cloned with a real branch, so
/// this only runs to repair a damaged checkout.
fn default_branch(repo: &Repository, refspecs: &[&str]) -> Result<String> {
    if let Ok(head) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Some(name) = head
            .symbolic_target()
            .and_then(|t| t.strip_prefix("refs/remotes/origin/"))
    {
        return Ok(name.to_owned());
    }

    for spec in refspecs {
        if let Some((_, dst)) = spec.split_once(':')
            && let Some(name) = dst
                .strip_prefix("refs/remotes/origin/")
                .and_then(|n| n.strip_suffix('*'))
            && !name.is_empty()
        {
            return Ok(name.to_owned());
        }
    }

    // Every Scoop bucket is `master` or `main`; `master` is the older default and
    // what Scoop itself assumes when it has to guess.
    Ok(if repo.find_reference("refs/remotes/origin/main").is_ok() {
        "main".to_owned()
    } else {
        "master".to_owned()
    })
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

                // Fetch the remote's configured refspecs explicitly. Relying on the
                // config would leave the remote-tracking refs behind whenever a
                // bucket was cloned by an older build, and every freshness check
                // then silently reads a stale SHA.
                let configured = remote
                    .fetch_refspecs()
                    .map_err(|e| crate::Error::Git(e.to_string()))?;

                // `StringArray` iterates as `Option<&str>`, where `None` marks a
                // non-UTF-8 entry — a refspec git would choke on anyway.
                let configured: Vec<String> =
                    configured.iter().flatten().map(str::to_owned).collect();
                let refspecs: Vec<&str> = if configured.is_empty() {
                    DEFAULT_FETCH_REFSPEC.to_vec()
                } else {
                    configured.iter().map(String::as_str).collect()
                };

                let mut opts = fetch_options();
                remote
                    .fetch(&refspecs, Some(&mut opts), None)
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
            move || sync_to_remote(&p)
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

    /// Always true: libgit2 is linked into the binary, so Git is available even on a
    /// machine where nothing has been installed.
    async fn is_installed(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "-b", "master"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(path.join("f.txt"), b"v1").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "one"]);
    }

    fn clone(upstream: &Path, down: &Path) {
        let out = std::process::Command::new("git")
            .args([
                "clone",
                "-q",
                upstream.to_str().unwrap(),
                down.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "clone failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit_upstream(path: &Path, body: &[u8], msg: &str) {
        std::fs::write(path.join("f.txt"), body).unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", msg]] {
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(path)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        }
    }

    fn head_file(path: &Path) -> String {
        String::from_utf8_lossy(&std::fs::read(path.join(".git/HEAD")).unwrap())
            .trim()
            .to_owned()
    }

    fn abbrev_head(path: &Path) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn git_pull(path: &Path) -> std::process::Output {
        std::process::Command::new("git")
            .args(["pull", "-q"])
            .current_dir(path)
            .output()
            .unwrap()
    }

    /// Documents the libgit2 behaviour that caused the bug.
    ///
    /// `set_head` with a ref outside `refs/heads/*` detaches HEAD: `.git/HEAD`
    /// becomes a bare SHA and `git pull` then fails with "You are not currently
    /// on a branch". This is the trap the old `pull()` fell into, and the reason
    /// `sync_to_remote` moves the local branch instead.
    #[test]
    fn set_head_on_remote_ref_detaches_head() {
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        let down = dir.path().join("down");
        init_repo(&upstream);
        clone(&upstream, &down);

        assert!(
            head_file(&down).starts_with("ref: refs/heads/master"),
            "clone should be on a branch, got {}",
            head_file(&down)
        );

        let repo = Repository::open(&down).unwrap();
        repo.set_head("refs/remotes/origin/master").unwrap();
        drop(repo);

        assert_eq!(abbrev_head(&down), "HEAD", "HEAD is now detached");
        assert!(
            !head_file(&down).starts_with("ref:"),
            ".git/HEAD should hold a bare SHA, got {}",
            head_file(&down)
        );

        // And this is exactly what `scoop update` then hits.
        let out = git_pull(&down);
        assert!(
            !out.status.success(),
            "git pull fails on a detached HEAD, as scoop reported"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("not currently on a branch"),
            "unexpected git error: {stderr}"
        );
    }

    /// The fix: move the local branch, and HEAD stays attached.
    #[test]
    fn sync_to_remote_keeps_head_attached() {
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        let down = dir.path().join("down");
        init_repo(&upstream);
        commit_upstream(&upstream, b"v2", "two");
        clone(&upstream, &down);

        sync_to_remote(&down).unwrap();

        assert_eq!(abbrev_head(&down), "master", "HEAD must stay on branch");
        assert!(head_file(&down).starts_with("ref: refs/heads/master"));

        let rev = |p: &Path| {
            String::from_utf8_lossy(
                &std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(p)
                    .output()
                    .unwrap()
                    .stdout,
            )
            .trim()
            .to_owned()
        };
        assert_eq!(rev(&down), rev(&upstream), "branch was fast-forwarded");
        assert_eq!(
            std::fs::read_to_string(down.join("f.txt")).unwrap(),
            "v2",
            "working tree followed the branch"
        );

        // The real acceptance criterion: a later `git pull` still works.
        let out = git_pull(&down);
        assert!(
            out.status.success(),
            "git pull must work afterwards: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A bucket already detached by the old code must be repaired, not deepened.
    #[test]
    fn sync_to_remote_recovers_a_detached_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        let down = dir.path().join("down");
        init_repo(&upstream);
        clone(&upstream, &down);

        // Reproduce the damage the old code did.
        let repo = Repository::open(&down).unwrap();
        repo.set_head("refs/remotes/origin/master").unwrap();
        drop(repo);
        assert_eq!(abbrev_head(&down), "HEAD");

        sync_to_remote(&down).unwrap();

        assert_eq!(abbrev_head(&down), "master", "should re-attach");
        let out = git_pull(&down);
        assert!(out.status.success(), "git pull works after repair");
    }

    /// A dirty bucket is refused rather than silently overwritten.
    #[test]
    fn sync_to_remote_refuses_a_dirty_tree() {
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        let down = dir.path().join("down");
        init_repo(&upstream);
        clone(&upstream, &down);

        std::fs::write(down.join("f.txt"), b"locally edited").unwrap();

        assert!(
            sync_to_remote(&down).is_err(),
            "local edits must not be silently discarded"
        );
        assert_eq!(
            std::fs::read_to_string(down.join("f.txt")).unwrap(),
            "locally edited"
        );
    }
}
