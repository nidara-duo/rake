use clap::Parser;
use crossterm::style::Stylize;

/// Remove old versions of packages
#[derive(Debug, Parser)]
pub struct Args {
    /// App(s) to clean up (`*` to clean all)
    #[arg(required_unless_present = "all")]
    pub apps: Vec<String>,

    /// Clean all apps
    #[arg(short = 'a', long, conflicts_with = "apps")]
    pub all: bool,

    /// Also remove outdated download cache files
    #[arg(short = 'k', long)]
    pub cache: bool,
}

pub fn execute(args: Args, session: &rake_core::session::Session) -> anyhow::Result<()> {
    let names = if args.all {
        vec!["*".to_string()]
    } else {
        args.apps
    };

    let mut options = Vec::new();
    if args.cache {
        options.push(rake_core::operations::cleanup::CleanupOption::Cache);
    }

    let results = rake_core::operations::cleanup::cleanup_packages(session, &names, &options)?;

    if results.is_empty() {
        println!("{}", "All packages are already clean.".green());
        return Ok(());
    }

    for res in results {
        println!("Cleaning {} ...", res.name.yellow());
        for ver in res.removed_versions {
            println!("  {} Removed {}", "✔".green(), ver);
        }
    }

    if args.all || names.iter().any(|a| a == "*") || names.len() > 1 {
        println!("{}", "Everything is shiny now!".green());
    }

    Ok(())
}
