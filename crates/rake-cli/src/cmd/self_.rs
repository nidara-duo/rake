use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use rake_core::session::Session;

/// Manage Rake itself (install, update, uninstall)
#[derive(Debug, Parser)]
#[command(disable_help_subcommand = true)]
pub struct Args {
    #[command(subcommand)]
    pub action: SelfAction,
}

#[derive(Debug, Parser)]
pub enum SelfAction {
    /// Install Rake (delegates to bootstrap.ps1)
    Install {
        /// Install from a local rake.exe instead of downloading a release
        #[arg(long, value_name = "PATH")]
        local: Option<PathBuf>,
    },
    /// Update Rake to the latest version
    Update {
        /// Update from a local rake.exe instead of downloading a release
        #[arg(long, value_name = "PATH")]
        local: Option<PathBuf>,
    },
    /// Uninstall Rake
    Uninstall,
}

/// Locate `bootstrap.ps1`.
///
/// Installed builds ship the script next to `rake.exe`. Development builds live in
/// `target/<profile>/`, so walk up looking for the repository's `scripts/` directory.
fn bootstrap_script_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("Cannot determine executable path")?;
    let dir = exe.parent().context("Executable has no parent directory")?;

    let mut candidates = vec![dir.join("bootstrap.ps1")];
    for up in 2..=4 {
        if let Some(root) = dir.ancestors().nth(up) {
            candidates.push(root.join("scripts").join("bootstrap.ps1"));
        }
    }

    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }

    let searched = candidates
        .iter()
        .map(|p| format!("  - {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n");

    anyhow::bail!(
        "bootstrap.ps1 not found. Searched:\n{searched}\n\
         Download the latest release from https://github.com/nidara-duo/rake/releases"
    );
}

/// Run bootstrap.ps1, letting its output stream straight through.
fn invoke_bootstrap(action: &str, local: Option<&std::path::Path>) -> Result<()> {
    let script = bootstrap_script_path()?;
    let mut args: Vec<String> = vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "RemoteSigned".into(),
        "-File".into(),
        script.to_string_lossy().into_owned(),
        action.into(),
    ];
    if let Some(path) = local {
        args.push("-Source".into());
        args.push(path.to_string_lossy().into_owned());
    }

    let status = std::process::Command::new("powershell")
        .args(&args)
        .status()
        .context("Failed to execute bootstrap.ps1")?;

    if !status.success() {
        anyhow::bail!("bootstrap script failed (exit code: {:?})", status.code());
    }
    Ok(())
}

pub fn execute(args: Args, _session: &Session) -> Result<()> {
    match args.action {
        SelfAction::Install { local } => invoke_bootstrap("install", local.as_deref()),
        SelfAction::Update { local } => invoke_bootstrap("update", local.as_deref()),
        SelfAction::Uninstall => invoke_bootstrap("uninstall", None),
    }
}
