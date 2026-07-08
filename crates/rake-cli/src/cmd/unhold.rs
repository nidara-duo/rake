use anyhow::Result;
use clap::Parser;
use crossterm::style::{Stylize, style};
use rake_core::operations::hold;
use rake_core::session::Session;

/// Unhold package(s) to enable changes
#[derive(Debug, Parser)]
pub struct Args {
    /// Package name(s) to unhold
    pub packages: Vec<String>,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    for name in &args.packages {
        match hold::set_held(session, name, false) {
            Ok(()) => {
                println!("{} '{}' was unheld.", style("✓").green(), name);
            }
            Err(e) => {
                eprintln!("ERROR: {}", e);
            }
        }
    }
    Ok(())
}
