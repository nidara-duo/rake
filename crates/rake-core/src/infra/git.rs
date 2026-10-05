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

    /// Head SHA the remote currently advertises for `branch`.
    ///
    /// This is a metadata-only query (`git ls-remote`) that never touches the
    /// working tree or any local ref, so it is safe to call during a read-only
    /// `rake status`.
    async fn remote_head_sha(&self, path: &Path, branch: &str) -> Result<Option<String>>;

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

    async fn remote_head_sha(&self, path: &Path, branch: &str) -> Result<Option<String>> {
        let branch = branch.to_owned();
        let path = path.to_owned();

        let output = tokio::task::spawn_blocking(move || {
            let mut cmd = Self::git_cmd();
            cmd.current_dir(&path);
            cmd.arg("ls-remote")
                .arg("--exit-code")
                .arg("origin")
                .arg(format!("refs/heads/{branch}"));
            cmd.output()
        })
        .await
        .map_err(|e| crate::Error::Git(e.to_string()))?
        .map_err(spawn_error)?;

        // Exit code 2 means "no such ref", which is a legitimate answer
        // (branch gone upstream) rather than an error.
        if !output.status.success() && output.status.code() != Some(2) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Git(format!(
                "git ls-remote failed: {}",
                stderr.trim()
            )));
        }

        Ok(parse_ls_remote_sha(&String::from_utf8_lossy(
            &output.stdout,
        )))
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

/// Pull the SHA out of a `git ls-remote` line: `<sha>\t<ref>`.
fn parse_ls_remote_sha(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .find(|sha| sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::parse_ls_remote_sha;

    #[test]
    fn parses_single_ref() {
        let out = "aaaaaaaabbbbbbbbccccccccddddddddeeeeeeee\trefs/heads/master\n";
        assert_eq!(
            parse_ls_remote_sha(out).as_deref(),
            Some("aaaaaaaabbbbbbbbccccccccddddddddeeeeeeee")
        );
    }

    #[test]
    fn parses_padded_ref() {
        let out = "1111111122222222333333334444444455555555\trefs/heads/main\n";
        assert!(parse_ls_remote_sha(out).is_some());
    }

    #[test]
    fn empty_output_yields_none() {
        assert!(parse_ls_remote_sha("").is_none());
    }

    #[test]
    fn rejects_non_sha_lines() {
        assert!(parse_ls_remote_sha("not a ref\nwarning: something\n").is_none());
    }

    #[test]
    fn picks_the_sha_not_the_ref_name() {
        let out = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\trefs/heads/main\n";
        let sha = parse_ls_remote_sha(out).unwrap();
        assert_eq!(sha, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    }
}
