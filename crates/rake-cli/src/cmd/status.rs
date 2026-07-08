use anyhow::Result;
use clap::Parser;
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use crossterm::style::{Stylize, style};
use rake_core::operations::status::{StatusInfoFlag, StatusReport};
use rake_core::session::Session;

#[derive(Debug, Parser)]
pub struct Args {
    /// Check only local manifests, skip git-based outdated detection
    #[arg(short = 'l', long)]
    pub local: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let report = rake_core::operations::status::collect_status(session, args.local)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if report.buckets_outdated {
        println!(
            "{} One or more buckets are outdated. Run `rake update` to sync.",
            style("!").yellow()
        );
    }

    if report.entries.is_empty() {
        println!("Everything is ok!");
        return Ok(());
    }

    let table = build_entries_table(&report);
    println!("{table}");

    Ok(())
}

fn build_entries_table(report: &StatusReport) -> Table {
    let mut table = Table::new();
    table.load_preset(NOTHING);

    table.set_header(vec![
        Cell::new("Name")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Version")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Available")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Missing")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Info")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
    ]);

    for entry in &report.entries {
        let version_cell = match entry.installed_version.as_deref() {
            Some(v) => Cell::new(v).add_attribute(Attribute::Dim),
            None => Cell::new("-"),
        };

        let latest_cell = match entry.latest_version.as_deref() {
            Some(v) => Cell::new(v).fg(Color::Blue),
            None => Cell::new("-"),
        };

        let missing = if entry.missing_dependencies.is_empty() {
            String::from("-")
        } else {
            entry.missing_dependencies.join(", ")
        };

        let missing_cell = if missing == "-" {
            Cell::new("-")
        } else {
            Cell::new(missing).fg(Color::Yellow)
        };

        let info_cell = build_flags_cell(&entry.flags);

        table.add_row(vec![
            Cell::new(&entry.name),
            version_cell,
            latest_cell,
            missing_cell,
            info_cell,
        ]);
    }

    table
}

fn build_flags_cell(flags: &[StatusInfoFlag]) -> Cell {
    if flags.is_empty() {
        return Cell::new("-");
    }

    let text = flags
        .iter()
        .map(|f| f.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let color = if flags.contains(&StatusInfoFlag::InstallFailed)
        || flags.contains(&StatusInfoFlag::ManifestRemoved)
    {
        Color::Red
    } else if flags.contains(&StatusInfoFlag::Outdated)
        || flags.contains(&StatusInfoFlag::MissingDependencies)
    {
        Color::Yellow
    } else if flags.contains(&StatusInfoFlag::Held) {
        Color::Magenta
    } else {
        Color::White
    };

    Cell::new(text).fg(color)
}
