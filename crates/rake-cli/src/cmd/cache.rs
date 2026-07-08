use anyhow::Result;
use clap::{Parser, Subcommand};
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use crossterm::style::{Stylize, style};
use rake_core::operations::cache;
use rake_core::session::Session;

/// Package cache management
#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List download caches
    #[clap(alias = "ls")]
    List {
        /// List caches matching the query
        query: Option<String>,
    },
    /// Show download caches (alias for list)
    Show {
        /// Show caches matching the query
        query: Option<String>,
    },
    /// Remove download caches
    #[clap(alias = "rm")]
    Remove {
        /// Remove caches matching the query
        query: Option<String>,
        /// Remove all caches
        #[arg(short, long, conflicts_with = "query")]
        all: bool,
    },
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let command = args.command.unwrap_or(Command::List { query: None });

    match command {
        Command::List { query } | Command::Show { query } => {
            let q = query.unwrap_or_else(|| "*".to_string());
            let files = cache::cache_list(session, &q)?;
            let mut total_size: u64 = 0;
            let total_count = files.len();

            let mut table = Table::new();
            table.load_preset(NOTHING);

            let header_cells = ["Name", "Version", "Filename", "Size"]
                .into_iter()
                .map(|title| {
                    Cell::new(title)
                        .add_attribute(Attribute::Bold)
                        .fg(Color::Green)
                });
            table.set_header(header_cells);

            for f in &files {
                let size = f.size();
                total_size += size;

                table.add_row(vec![
                    Cell::new(f.name()),
                    Cell::new(f.version()),
                    Cell::new(f.filename()).add_attribute(Attribute::Dim),
                    Cell::new(human_size(size)).fg(Color::Green),
                ]);
            }

            if total_count > 0 {
                table.add_row(vec!["", "", "", ""]);
                table.add_row(vec![
                    Cell::new("Total:")
                        .add_attribute(Attribute::Bold)
                        .set_alignment(comfy_table::CellAlignment::Right),
                    Cell::new(format!("{} files", total_count)),
                    Cell::new(""),
                    Cell::new(human_size(total_size)).add_attribute(Attribute::Bold),
                ]);
                println!("{table}");
            } else {
                println!("No files found in cache.");
            }

            Ok(())
        }
        Command::Remove { query, all } => {
            let q = if all {
                "*".to_string()
            } else if let Some(q) = query {
                q
            } else {
                eprintln!("ERROR: <app> missing");
                eprintln!("Usage: rake cache rm <app>");
                std::process::exit(1);
            };

            let count = cache::cache_remove(session, &q)?;

            if q == "*" || all {
                println!("{} All download caches were removed.", style("✓").green());
            } else {
                println!(
                    "{} All caches matching '{}' were removed.",
                    style("✓").green(),
                    q
                );
            }

            if count > 0 {
                println!("  Removed {} file(s).", count);
            }

            Ok(())
        }
    }
}

use crate::util::human_size;
