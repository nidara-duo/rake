use std::io::Write;

use anyhow::Result;
use clap::Parser;
use crossterm::style::{Stylize, style};
use rake_core::operations::uninstall;
use rake_core::session::Session;

/// Uninstall an app
#[derive(Debug, Parser)]
pub struct Args {
    /// App(s) to uninstall
    pub names: Vec<String>,
    /// Remove all persistent data
    #[arg(short, long)]
    pub purge: bool,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let query: Vec<String> = if args.names.is_empty() {
        eprintln!("ERROR: <app> missing");
        return Ok(());
    } else {
        args.names
    };

    let result = uninstall::uninstall_packages(session, &query, args.purge)?;

    if result.is_empty() {
        writeln!(std::io::stdout(), "No apps were uninstalled.")?;
        return Ok(());
    }

    for pkg in &result {
        writeln!(
            std::io::stdout(),
            "Uninstalling '{}' ({}).",
            pkg.name,
            pkg.version,
        )?;
        // TODO: remove shortcuts
        // TODO: remove env_add_path / env_set
        writeln!(
            std::io::stdout(),
            " {} '{}' was uninstalled.",
            style("✓").green(),
            style(&pkg.name).green(),
        )?;
    }

    Ok(())
}
