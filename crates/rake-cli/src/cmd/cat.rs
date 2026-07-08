use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crossterm::style::Stylize;
use rake_core::session::Session;

/// Show content of specified manifest
#[derive(Debug, Parser)]
#[clap(arg_required_else_help = true)]
pub struct Args {
    /// Name of the package
    pub name: String,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let path = find_manifest(session, &args.name);

    let path = match path {
        Some(p) => p,
        None => {
            eprintln!("Could not find manifest for '{}'.", args.name);
            return Ok(());
        }
    };

    let content = std::fs::read_to_string(&path)?;

    // Pretty-print with serde_json
    let parsed: serde_json::Value = serde_json::from_str(&content)?;
    let pretty = serde_json::to_string_pretty(&parsed)?;

    println!("{}:", path.display().to_string().green());
    println!("{}", pretty);

    Ok(())
}

fn find_manifest(session: &Session, name: &str) -> Option<PathBuf> {
    let root = session.config().root_path.as_ref()?;
    let buckets_dir = root.join("buckets");

    let bucket_entries = std::fs::read_dir(&buckets_dir).ok()?;
    for entry in bucket_entries {
        let bucket = entry.ok()?.path();
        let manifest = bucket.join("bucket").join(format!("{}.json", name));
        if manifest.exists() {
            return Some(manifest);
        }
    }

    None
}
