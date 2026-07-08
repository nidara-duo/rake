use std::path::Path;

use async_trait::async_trait;

use crate::Result;

#[async_trait]
pub trait GitService: Send + Sync {
    async fn clone(&self, url: &str, path: &Path) -> Result<()>;

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
        .map_err(|e| crate::Error::Git(e.to_string()))??;

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
        .map_err(|e| crate::Error::Git(e.to_string()))??;

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
        .map_err(|e| crate::Error::Git(e.to_string()))??;

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
                let out = cmd.output().map_err(|e| crate::Error::Git(e.to_string()))?;
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
                let out = cmd.output().map_err(|e| crate::Error::Git(e.to_string()))?;
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
            .map_err(|e| crate::Error::Git(e.to_string()))?;

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
