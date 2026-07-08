use anyhow::Result;
use clap::Parser;
use rake_core::session::Session;

/// Manage scoop aliases
#[derive(Debug, Parser)]
pub struct Args;

pub fn execute(_args: Args, _session: &Session) -> Result<()> {
    println!("'alias' is not yet implemented.");
    Ok(())
}
