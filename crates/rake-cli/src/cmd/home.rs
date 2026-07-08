use std::io::Write;

use anyhow::Result;
use clap::Parser;
use crossterm::style::{Stylize, style};
use rake_core::session::Session;

/// Browse the homepage of a package
#[derive(Debug, Parser)]
pub struct Args {
    /// The package name
    pub package: String,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let packages = rake_core::operations::query::find_all_synced_by_name(session, &args.package)?;
    let matches: Vec<_> = packages
        .into_iter()
        .filter(|p| p.name().eq_ignore_ascii_case(&args.package))
        .collect();

    match matches.len() {
        0 => {
            eprintln!("Could not find package named '{}'.", args.package);
        }
        1 => {
            open_homepage(&matches[0])?;
        }
        _ => {
            println!("Found multiple packages named '{}':\n", args.package);
            for (idx, pkg) in matches.iter().enumerate() {
                let hp = pkg.homepage().unwrap_or("(no homepage)");
                println!("  {}. {}/{} ({})", idx, pkg.bucket(), pkg.name(), hp);
            }
            print!("\nPlease select one, enter the number to continue: ");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if let Ok(num) = input.trim().parse::<usize>()
                && let Some(pkg) = matches.get(num)
            {
                open_homepage(pkg)?;
                return Ok(());
            }
            eprintln!("Invalid input.");
        }
    }

    Ok(())
}

fn open_homepage(pkg: &rake_domain::package::Package) -> Result<()> {
    let url = match pkg.homepage() {
        Some(u) => u,
        None => {
            eprintln!("Package '{}' has no homepage.", pkg.name());
            return Ok(());
        }
    };

    let _ = std::process::Command::new("rundll32.exe")
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to open browser: {e}"))?;

    println!(
        " {} '{}' homepage opened in your browser.",
        style("✓").green(),
        pkg.name(),
    );

    Ok(())
}
