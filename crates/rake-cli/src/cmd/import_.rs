use anyhow::Result;
use clap::Parser;
use rake_core::session::Session;

/// Import apps, buckets and configs from a Scoopfile in JSON format
#[derive(Debug, Parser)]
pub struct Args;

pub fn execute(_args: Args, _session: &Session) -> Result<()> {
    println!("'import' is not yet implemented.");
    Ok(())
}
