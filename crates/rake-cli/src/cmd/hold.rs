use anyhow::Result;
use clap::Parser;
use crossterm::style::{Stylize, style};
use rake_core::operations::hold;
use rake_core::session::Session;

/// Hold package(s) to disable changes
#[derive(Debug, Parser)]
pub struct Args {
    /// Package name(s) to hold
    pub packages: Vec<String>,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    for name in &args.packages {
        match hold::set_held(session, name, true) {
            Ok(()) => {
                println!("{} '{}' was held.", style("✓").green(), name);
            }
            Err(e) => {
                eprintln!("ERROR: {}", e);
            }
        }
    }
    Ok(())
}
