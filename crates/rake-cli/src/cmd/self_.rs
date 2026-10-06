use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::style::{Stylize, style};
use rake_core::operations::self_update::{self, NoUpgrade, ReleaseChannel, UpdateOutcome};
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
        /// Also consider pre-releases, when one is newer than the latest finished release
        #[arg(long = "pre-release")]
        pre_release: bool,
    },
    /// Update Rake to the latest version
    Update {
        /// Update from a local rake.exe instead of downloading a release
        #[arg(long, value_name = "PATH")]
        local: Option<PathBuf>,
        /// Also consider pre-releases, when one is newer than the latest finished release
        #[arg(long = "pre-release")]
        pre_release: bool,
    },
    /// Uninstall Rake
    Uninstall,
}

/// Turn the flag into a channel. One place, so the two subcommands cannot drift.
fn channel(pre_release: bool) -> ReleaseChannel {
    if pre_release {
        ReleaseChannel::PreRelease
    } else {
        ReleaseChannel::Stable
    }
}

pub async fn execute(args: Args, session: &Session) -> Result<()> {
    match args.action {
        SelfAction::Install { local, pre_release } => {
            install(session, local.as_deref(), channel(pre_release)).await
        }
        SelfAction::Update { local, pre_release } => {
            update(session, local.as_deref(), channel(pre_release)).await
        }
        SelfAction::Uninstall => uninstall(session),
    }
}

async fn install(session: &Session, local: Option<&Path>, channel: ReleaseChannel) -> Result<()> {
    let outcome = self_update::install(session, local, channel).await?;

    println!("{} Rake installed", style("✓").green());
    if let Some(version) = &outcome.version {
        println!("  Version: {version}");
    }
    println!("  Binary: {}", outcome.exe.display());
    if outcome.path_added {
        println!("  Added to PATH: {}", self_update::bin_dir()?.display());
    }
    report_stale(&outcome.stale);
    println!("  Run 'rake --help' to get started.");
    Ok(())
}

async fn update(session: &Session, local: Option<&Path>, channel: ReleaseChannel) -> Result<()> {
    // Both no-op outcomes are reported, and neither is dressed up as success. The
    // prerelease case in particular is not a failure — it is the answer to a question the
    // user can act on, so it says what exists and which flag takes it.
    let outcome = match self_update::update(session, local, channel).await? {
        UpdateOutcome::Updated(outcome) => outcome,
        UpdateOutcome::Unchanged(NoUpgrade::UpToDate) => {
            println!(
                "{} Rake {} is already up to date.",
                style("✓").green(),
                self_update::running_version()
            );
            return Ok(());
        }
        UpdateOutcome::Unchanged(NoUpgrade::PrereleaseHeldBack { newest }) => {
            println!(
                "{} Rake {} is up to date, but {newest} is available.",
                style("i").cyan(),
                self_update::running_version()
            );
            println!("  Run 'rake self update --pre-release' to install it.");
            return Ok(());
        }
    };

    println!("{} Rake updated", style("✓").green());
    if let Some(version) = &outcome.version {
        println!("  Version: {version}");
    }
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
