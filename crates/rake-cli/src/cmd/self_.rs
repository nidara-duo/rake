use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use rake_core::session::Session;

/// Manage Rake itself (install, update, uninstall)
#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    pub action: SelfAction,
}

#[derive(Debug, Parser)]
pub enum SelfAction {
    /// Install Rake (delegates to bootstrap.ps1)
    Install,
    /// Update Rake to the latest version
    Update,
    /// Uninstall Rake
    Uninstall,
}

fn bootstrap_script_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("Cannot determine executable path")?;
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    let candidate = dir.join("bootstrap.ps1");
    if candidate.exists() {
        return Ok(candidate);
    }
    anyhow::bail!(
        "bootstrap.ps1 not found alongside the executable.\n\
         Download the latest release from https://github.com/anomalyco/rake/releases"
    );
}

fn invoke_bootstrap(subcommand: &str) -> Result<()> {
    let script = bootstrap_script_path()?;
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "RemoteSigned",
            "-File",
            &script.to_string_lossy(),
            subcommand,
        ])
        .status()
        .context("Failed to execute bootstrap.ps1")?;

    if !status.success() {
        anyhow::bail!("bootstrap script failed (exit code: {:?})", status.code());
    }
    Ok(())
}

pub fn execute(args: Args, _session: &Session) -> Result<()> {
    match args.action {
        SelfAction::Install => invoke_bootstrap("install"),
        SelfAction::Update => invoke_bootstrap("update"),
        SelfAction::Uninstall => invoke_bootstrap("uninstall"),
    }
}
