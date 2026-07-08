use anyhow::Result;
use clap::Parser;
use rake_core::session::Session;

/// Export installed apps, buckets and configs in JSON format
#[derive(Debug, Parser)]
pub struct Args;

pub fn execute(_args: Args, _session: &Session) -> Result<()> {
    println!("'export' is not yet implemented.");
    Ok(())
}
