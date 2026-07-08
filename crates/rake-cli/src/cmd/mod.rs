use anyhow::Result;
use clap::{Parser, Subcommand};
use rake_core::session::Session;

mod alias;
mod bucket;
mod cache;
mod cat;
mod checkup;
mod cleanup;
mod config;
mod download;
mod export_;
mod hold;
mod home;
mod import_;
mod info;
mod install;
mod list;
mod reset;
mod search;
mod self_;
mod shim;
mod status;
mod unhold;
mod uninstall;
mod update;
mod which;

#[derive(Parser)]
#[command(
    name = "rake",
    version = env!("CARGO_PKG_VERSION"),
    about = "Scoop-compatible package manager for Windows",
    subcommand_required = true,
    arg_required_else_help = true,
    max_term_width = 100
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage scoop aliases
    Alias(alias::Args),
    /// Manage buckets
    Bucket(bucket::Args),
    /// Manage download cache
    Cache(cache::Args),
    /// Show content of specified manifest
    Cat(cat::Args),
    /// Check for potential problems
    Checkup(checkup::Args),
    /// Remove old versions of packages
    Cleanup(cleanup::Args),
    /// Get or set configuration values
    Config(config::Args),
    /// Download packages to cache
    Download(download::Args),
    /// Export installed apps, buckets and configs in JSON format
    #[clap(name = "export")]
    Export(export_::Args),
    /// Hold package(s) to disable changes
    Hold(hold::Args),
    /// Browse the homepage of a package
    Home(home::Args),
    /// Import apps, buckets and configs from a Scoopfile in JSON format
    #[clap(name = "import")]
    Import(import_::Args),
    /// Show package(s) basic information
    Info(info::Args),
    /// Install an app
    Install(install::Args),
    /// List installed apps
    List(list::Args),
    /// Reset an app to resolve conflicts
    Reset(reset::Args),
    /// Search available packages
    Search(search::Args),
    /// Manage Rake itself (install, update, uninstall)
    #[clap(name = "self")]
    Self_(self_::Args),
    /// Manipulate Scoop shims
    Shim(shim::Args),
    /// Show status and check for new app versions
    Status(status::Args),
    /// Unhold package(s) to enable changes
    Unhold(unhold::Args),
    /// Uninstall an app
    #[clap(alias = "rm", alias = "remove")]
    Uninstall(uninstall::Args),
    /// Update installed packages to latest versions
    Update(update::Args),
    /// Locate a shim/executable
    Which(which::Args),
}

pub async fn start() -> Result<()> {
    let args = Cli::parse();
    let session = Session::new().await?;

    match args.command {
        Command::Alias(args) => alias::execute(args, &session)?,
        Command::Bucket(args) => bucket::execute(args, &session)?,
        Command::Cache(args) => cache::execute(args, &session)?,
        Command::Cat(args) => cat::execute(args, &session)?,
        Command::Checkup(args) => checkup::execute(args, &session)?,
        Command::Cleanup(args) => cleanup::execute(args, &session)?,
        Command::Config(args) => config::execute(args, &session)?,
        Command::Download(args) => download::execute(args, &session)?,
        Command::Export(args) => export_::execute(args, &session)?,
        Command::Hold(args) => hold::execute(args, &session)?,
        Command::Home(args) => home::execute(args, &session)?,
        Command::Import(args) => import_::execute(args, &session)?,
        Command::Info(args) => info::execute(args, &session)?,
        Command::Install(args) => install::execute(args, &session).await?,
        Command::List(args) => list::execute(args, &session)?,
        Command::Reset(args) => reset::execute(args, &session)?,
        Command::Search(args) => search::execute(args, &session)?,
        Command::Self_(args) => self_::execute(args, &session)?,
        Command::Shim(args) => shim::execute(args, &session)?,
        Command::Status(args) => status::execute(args, &session)?,
        Command::Unhold(args) => unhold::execute(args, &session)?,
        Command::Uninstall(args) => uninstall::execute(args, &session)?,
        Command::Update(args) => update::execute(args, &session).await?,
        Command::Which(args) => which::execute(args, &session)?,
    }

    Ok(())
}
