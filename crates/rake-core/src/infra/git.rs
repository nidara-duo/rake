use std::path::Path;

use async_trait::async_trait;

use crate::Result;

#[async_trait]
pub trait GitService: Send + Sync {
    async fn clone(&self, url: &str, path: &Path) -> Result<()>;

    /// Update `refs/remotes/origin/*` from the remote.
    ///
    /// This is the call that costs network time, and the one later commands get
    /// to reuse: after a fetch, bucket freshness is answerable entirely from
    /// local refs, so `rake update` and `rake status` do not repeat the work.
    async fn fetch(&self, path: &Path) -> Result<()>;

    async fn pull(&self, path: &Path) -> Result<()>;

    async fn reset_hard(&self, path: &Path) -> Result<()>;

    fn remote_url(&self, path: &Path) -> Result<Option<String>>;

    async fn is_installed(&self) -> bool;
}

pub struct ExternalGit;

impl ExternalGit {
    pub fn new() -> Self {
        Self
    }

    fn git_cmd() -> std::process::Command {
        let mut cmd = std::process::Command::new("git");
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd
    }
}

/// Translate a failure to launch `git` into something the user can act on.
///
/// When git is not installed the OS reports a bare "program not found", which says
/// nothing about what to do next. Buckets are git repositories, so git is a hard
/// requirement for every bucket operation — name it and say how to obtain it.
fn spawn_error(e: std::io::Error) -> crate::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        return crate::Error::GitNotFound;
    }
    crate::Error::Git(e.to_string())
}

impl Default for ExternalGit {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GitService for ExternalGit {
    async fn clone(&self, url: &str, path: &Path) -> Result<()> {
        let output = tokio::task::spawn_blocking({
            let path = path.to_owned();
            let url = url.to_owned();
            move || {
                let mut cmd = Self::git_cmd();
                cmd.arg("clone").arg("--depth=1").arg(&url).arg(&path);
                cmd.output()
            }
        })
        .await
        .map_err(|e| crate::Error::Git(e.to_string()))?
        .map_err(spawn_error)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Git(format!(
                "git clone failed: {}",
                stderr.trim()
            )));
        }

        Ok(())
    }

    async fn fetch(&self, path: &Path) -> Result<()> {
        let output = tokio::task::spawn_blocking({
            let path = path.to_owned();
            move || {
                let mut cmd = Self::git_cmd();
                cmd.current_dir(&path);
                cmd.arg("fetch").arg("origin");
                cmd.arg("--depth=1");
                cmd.output()
            }
        })
        .await
        .map_err(|e| crate::Error::Git(e.to_string()))?
        .map_err(spawn_error)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Git(format!(
                "git fetch failed: {}",
                stderr.trim()
            )));
        }

        Ok(())
    }

    async fn pull(&self, path: &Path) -> Result<()> {
        let output = tokio::task::spawn_blocking({
            let path = path.to_owned();
            move || {
                let mut cmd = Self::git_cmd();
                cmd.current_dir(&path);
                cmd.arg("pull").arg("-q");
                cmd.output()
            }
        })
        .await
        .map_err(|e| crate::Error::Git(e.to_string()))?
        .map_err(spawn_error)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Git(format!(
                "git pull failed: {}",
                stderr.trim()
            )));
        }

        Ok(())
    }

    async fn reset_hard(&self, path: &Path) -> Result<()> {
        tokio::task::spawn_blocking({
            let path = path.to_owned();
            move || -> Result<()> {
                // fetch all branches straight into local refs
                let mut cmd = Self::git_cmd();
                cmd.current_dir(&path);
                cmd.arg("fetch").arg("origin");
                cmd.arg("refs/heads/*:refs/heads/*");
                let out = cmd.output().map_err(spawn_error)?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(crate::Error::Git(format!(
                        "git fetch failed: {}",
                        stderr.trim()
                    )));
                }

                // hard reset to HEAD
                let mut cmd = Self::git_cmd();
                cmd.current_dir(&path);
                cmd.arg("reset").arg("--hard").arg("HEAD");
                let out = cmd.output().map_err(spawn_error)?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(crate::Error::Git(format!(
                        "git reset failed: {}",
                        stderr.trim()
                    )));
                }

                Ok(())
            }
        })
        .await
        .map_err(|e| crate::Error::Git(e.to_string()))??;

        Ok(())
    }

    fn remote_url(&self, path: &Path) -> Result<Option<String>> {
        let output = std::process::Command::new("git")
            .arg("remote")
            .arg("get-url")
            .arg("origin")
            .current_dir(path)
            .output()
            .map_err(spawn_error)?;

        if output.status.success() {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Ok(Some(url))
        } else {
            Ok(None)
        }
    }

    async fn is_installed(&self) -> bool {
        let output = tokio::task::spawn_blocking(|| {
            std::process::Command::new("git").arg("--version").output()
        })
        .await;

        match output {
            Ok(Ok(out)) => out.status.success(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a throwaway repository with one commit on `master`.
    fn init_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repo");
        std::fs::create_dir_all(&path).unwrap();

        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&path)
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
        run(&["config", "user.name", "test"]);
        std::fs::write(path.join("f.txt"), b"v1").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "first"]);

        (dir, path)
    }

    fn head_sha(path: &std::path::Path) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[test]
    fn fetch_updates_remote_tracking_ref() {
        // The whole design rests on this: after fetch, the remote-tracking ref
        // carries the upstream head, so freshness is answerable offline.
        let (_upstream_dir, upstream) = init_repo();
        let (_down_dir, down) = init_repo();

        let url = upstream.to_string_lossy().to_string();
        let out = std::process::Command::new("git")
            .args(["remote", "add", "origin", &url])
            .current_dir(&down)
            .output()
            .unwrap();
        assert!(out.status.success());

        // A second upstream commit that `down` has not seen.
        std::fs::write(upstream.join("f.txt"), b"v2").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "second"]] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&upstream)
                .output()
                .unwrap();
        }

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(ExternalGit::new().fetch(&down)).unwrap();

        let remote_sha = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "refs/remotes/origin/master"])
                .current_dir(&down)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };

        assert_eq!(remote_sha, head_sha(&upstream));
        assert_ne!(remote_sha, head_sha(&down), "local head should lag");
    }

    #[test]
    fn fetch_on_missing_remote_reports_error() {
        let (_dir, path) = init_repo();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert!(runtime.block_on(ExternalGit::new().fetch(&path)).is_err());
    }
}
