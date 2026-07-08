use std::io::Write;

use anyhow::Result;
use clap::Parser;
use crossterm::style::{Stylize, style};
use rake_core::operations::reset;
use rake_core::session::Session;

/// Reset an app to resolve conflicts
#[derive(Debug, Parser)]
pub struct Args {
    /// App(s) to reset
    pub names: Vec<String>,
    /// Reset all installed apps
    #[arg(short, long)]
    pub all: bool,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let query: Vec<String> = if args.all {
        vec!["*".to_string()]
    } else if args.names.is_empty() {
        eprintln!("ERROR: <app> missing");
        return Ok(());
    } else {
        args.names
    };

    let result = reset::reset_packages(session, &query)?;

    if result.is_empty() {
        writeln!(std::io::stdout(), "No apps were reset.")?;
        return Ok(());
    }

    for (name, version, version_dir) in &result {
        let app_dir = version_dir.parent().unwrap();
        let current_link = app_dir.join("current");

        writeln!(std::io::stdout(), "Resetting {} ({}).", name, version)?;
        writeln!(
            std::io::stdout(),
            "  Linking {} => {}",
            style(current_link.display()).dim(),
            style(version_dir.display()).dim(),
        )?;

        // TODO: Creating shim for ...
        // TODO: Removing ... from your path
        // TODO: Adding ... to your path
        // TODO: Persisting ...
    }

    Ok(())
}
