use anyhow::Result;
use clap::Parser;
use crossterm::style::Stylize;
use rake_core::operations::checkup::CheckupSeverity;
use rake_core::session::Session;

/// Check for potential problems
#[derive(Debug, Parser)]
pub struct Args {
    /// Show additional diagnostics (Windows Developer Mode, etc.)
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let report = rake_core::operations::checkup::run_checkup(session, args.verbose)?;

    let mut issue_count = 0usize;
    let mut max_name_len = 0usize;

    for item in &report.items {
        let name_len = item.name.len();
        if name_len > max_name_len {
            max_name_len = name_len;
        }
    }

    for item in &report.items {
        print!("Checking {} ...", item.name);
        let pad = max_name_len.saturating_sub(item.name.len()) + 4;
        for _ in 0..pad {
            print!(" ");
        }

        let status = match item.severity {
            CheckupSeverity::Info => "OK".green(),
            CheckupSeverity::Warning => "WARNING".yellow(),
            CheckupSeverity::Error => "ERROR".red(),
        };
        println!("{status}");

        if item.severity != CheckupSeverity::Info {
            issue_count += 1;
            println!("  {}", item.message);
            if let Some(ref help) = item.help {
                println!("  {}", help.as_str().yellow());
            }
        }
    }

    println!();
    if issue_count == 0 {
        println!("{}", "No problems identified!".green());
    } else {
        let word = if issue_count == 1 {
            "problem"
        } else {
            "problems"
        };
        println!(
            "{}",
            format!("Found {issue_count} potential {word}.").yellow()
        );
    }

    Ok(())
}
