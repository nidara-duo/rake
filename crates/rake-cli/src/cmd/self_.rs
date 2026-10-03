use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::style::{Stylize, style};
use rake_core::operations::self_update;
use rake_core::session::Session;

/// Manage Rake itself (install, update, uninstall)
#[derive(Debug, Parser)]
#[command(disable_help_subcommand = true)]
pub struct Args {
    #[command(subcommand)]
    pub action: SelfAction,
}

#[derive(Debug, Subcommand)]
pub enum SelfAction {
    /// Install Rake
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

pub async fn execute(args: Args, session: &Session) -> Result<()> {
    match args.action {
        SelfAction::Install { local } => install(session, local.as_deref()).await,
        SelfAction::Update { local } => update(session, local.as_deref()).await,
        SelfAction::Uninstall => uninstall(session),
    }
}

async fn install(session: &Session, local: Option<&std::path::Path>) -> Result<()> {
    let outcome = self_update::install(session, local).await?;

    println!("{} Rake installed", style("✓").green());
    println!("  Binary: {}", outcome.exe.display());
    if outcome.path_added {
        println!("  Added to PATH: {}", self_update::bin_dir()?.display());
    }
    report_stale(&outcome.stale);
    println!("  Run 'rake --help' to get started.");
    Ok(())
}

async fn update(session: &Session, local: Option<&std::path::Path>) -> Result<()> {
    let outcome = self_update::update(session, local).await?;

    println!("{} Rake updated", style("✓").green());
    println!("  Binary: {}", outcome.exe.display());
    if outcome.path_added {
        println!("  Added to PATH: {}", self_update::bin_dir()?.display());
    }
    report_stale(&outcome.stale);
    if local.is_none() {
        println!("  Run 'rake --version' to confirm.");
    }
    Ok(())
}

fn uninstall(session: &Session) -> Result<()> {
    let outcome = self_update::uninstall(session)?;

    println!("{} Rake uninstalled", style("✓").green());
    if outcome.path_removed {
        println!("  Removed from PATH: {}", self_update::bin_dir()?.display());
    }
    match &outcome.deferred {
        Some(stale) => {
            println!(
                "  {} is still running and will be deleted as soon as this command exits.",
                stale.display()
            );
        }
        None => println!("  Install directory removed."),
    }
    Ok(())
}

/// Say plainly that the previous binary is still on disk, and when it goes away.
fn report_stale(stale: &Option<PathBuf>) {
    if let Some(stale) = stale {
        println!("  {} is removed on the next Rake run.", stale.display());
    }
}
