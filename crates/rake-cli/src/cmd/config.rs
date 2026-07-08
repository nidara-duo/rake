use anyhow::Result;
use clap::Parser;
use rake_core::session::Session;

/// Get or set configuration values
#[derive(Debug, Parser)]
pub struct Args;

pub fn execute(_args: Args, _session: &Session) -> Result<()> {
    println!("'config' is not yet implemented.");
    Ok(())
}
