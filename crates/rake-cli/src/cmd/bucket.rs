use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::style::{Stylize, style};
use rake_core::operations::bucket as bucket_ops;
use rake_core::session::Session;

/// Manage buckets
#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    pub command: BucketCommand,
}

#[derive(Debug, Subcommand)]
pub enum BucketCommand {
    /// Add a bucket
    Add(AddArgs),
    /// List all added buckets
    List,
    /// Remove a bucket
    Remove(RemoveArgs),
    /// Hold a bucket (prevent updating)
    Hold(HoldArgs),
    /// Unhold a bucket
    Unhold(HoldArgs),
}

#[derive(Debug, Parser)]
pub struct AddArgs {
    /// Bucket name
    pub name: String,
    /// Remote URL (optional — uses known URL if omitted)
    pub url: Option<String>,
}

#[derive(Debug, Parser)]
pub struct RemoveArgs {
    /// Bucket name
    pub name: String,
}

#[derive(Debug, Parser)]
pub struct HoldArgs {
    /// Bucket name
    pub name: String,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    match args.command {
        BucketCommand::Add(a) => add(a, session),
        BucketCommand::List => list(session),
        BucketCommand::Remove(a) => remove(a, session),
        BucketCommand::Hold(a) => hold(a, session),
        BucketCommand::Unhold(a) => unhold(a, session),
    }
}

fn add(args: AddArgs, session: &Session) -> Result<()> {
    let url = args.url.unwrap_or_default();
    let name_lower = args.name.to_ascii_lowercase();
    print!("Adding bucket '{name_lower}'... ");
    use std::io::Write;
    std::io::stdout().flush()?;
    bucket_ops::bucket_add(session, &name_lower, &url)?;
    println!("done.");
    Ok(())
}

fn list(session: &Session) -> Result<()> {
    let buckets = bucket_ops::bucket_list(session)?;

    if buckets.is_empty() {
        println!("No buckets added.");
        return Ok(());
    }

    for bucket in &buckets {
        if bucket.is_held() {
            print!("{}", style(bucket.name()).green());
            println!(" {}", style("[held]").yellow());
        } else {
            println!("{}", style(bucket.name()).green());
        }

        println!(
            " {} {} {}",
            style("├─").dim(),
            style("manifests:").dim(),
            bucket.manifest_count(),
        );

        let source = bucket.source();
        println!(
            " {} {} {}",
            style("└─").dim(),
            style("source:").dim(),
            source,
        );
    }

    Ok(())
}

fn remove(args: RemoveArgs, session: &Session) -> Result<()> {
    let name_lower = args.name.to_ascii_lowercase();
    bucket_ops::bucket_remove(session, &name_lower)?;
    println!("Removed bucket '{name_lower}'.");
    Ok(())
}

fn hold(args: HoldArgs, session: &Session) -> Result<()> {
    let name_lower = args.name.to_ascii_lowercase();
    bucket_ops::bucket_hold(session, &name_lower)?;
    println!("Held bucket '{name_lower}'.");
    Ok(())
}

fn unhold(args: HoldArgs, session: &Session) -> Result<()> {
    let name_lower = args.name.to_ascii_lowercase();
    bucket_ops::bucket_unhold(session, &name_lower)?;
    println!("Unheld bucket '{name_lower}'.");
    Ok(())
}
