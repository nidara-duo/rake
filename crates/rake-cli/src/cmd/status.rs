use anyhow::Result;
use clap::Parser;
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use crossterm::style::{Stylize, style};
use rake_core::operations::status::{CheckBuckets, StatusInfoFlag, StatusReport};
use rake_core::session::Session;

#[derive(Debug, Parser)]
pub struct Args {
    /// Stay offline: compare against each bucket's last fetched state (default)
    #[arg(short = 'l', long)]
    pub local: bool,

    /// Go online: fetch every bucket first, so freshness is checked against
    /// upstream and the fetch is reused by `rake update`
    #[arg(short = 'C', long = "check-buckets")]
    pub check_buckets: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn execute(args: Args, session: &Session) -> Result<()> {
    // Offline is the default: the status of already-installed packages is
    // fully answerable from disk, and asking every bucket's remote about it
    // turns a 40 ms command into a multi-second one. Scoop reaches for the
    // network unconditionally (it fetches), which is where its 6 s comes from.
    let mode = if args.check_buckets && !args.local {
        CheckBuckets::Fetch
    } else {
        CheckBuckets::Local
    };

    let report = rake_core::operations::status::collect_status(session, mode).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if report.buckets_outdated {
        // Wording follows scoop status (libexec/scoop-status.ps1:49), with the
        // command name swapped for rake's own.
        println!(
            "{} Bucket(s) out of date. Run `rake update` to get the latest changes.",
            style("!").yellow()
        );
    }

    if !report.buckets_unknown.is_empty() {
        println!(
            "{} Could not check bucket freshness: {}",
            style("!").yellow(),
            report.buckets_unknown.join(", ")
        );
    }

    // Say plainly that the bucket verdict was not re-verified online, otherwise
    // a stale-but-quiet bucket looks identical to a genuinely current one.
    if mode == CheckBuckets::Local && crate::util::has_buckets(session) {
        println!(
            "{} Bucket state is from the last `rake update` — add --check-buckets to fetch and verify online.",
            style("i").dark_grey()
        );
    }

    // With no buckets there is nothing Rake can install or check, so reporting "ok" here
    // would be misleading on a freshly installed machine.
    if !crate::util::has_buckets(session) {
        println!(
            "{} No buckets added yet — nothing can be installed until there is one.",
            style("!").yellow()
        );
        println!("  Add the default set with: rake bucket add main");
        return Ok(());
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
        Cell::new("Installed Version")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Latest Version")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Missing Dependencies")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Info")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
    ]);

    for entry in &report.entries {
        // Absent values render as empty cells, not as "-": Scoop leaves them
        // blank (libexec/scoop-status.ps1:66-68).
        let version_cell = match entry.installed_version.as_deref() {
            Some(v) => Cell::new(v).add_attribute(Attribute::Dim),
            None => Cell::new(""),
        };

        let latest_cell = match entry.latest_version.as_deref() {
            Some(v) => Cell::new(v).fg(Color::Blue),
            None => Cell::new(""),
        };

        // Scoop joins deps with " | ", not ", ".
        let missing_cell = if entry.missing_dependencies.is_empty() {
            Cell::new("")
        } else {
            Cell::new(entry.missing_dependencies.join(" | ")).fg(Color::Yellow)
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
    // Order matches scoop status (libexec/scoop-status.ps1:70-73), which emits
    // the labels in a fixed sequence rather than in flag-declaration order.
    const ORDER: [StatusInfoFlag; 4] = [
        StatusInfoFlag::InstallFailed,
        StatusInfoFlag::Held,
        StatusInfoFlag::Deprecated,
        StatusInfoFlag::ManifestRemoved,
    ];

    let labels: Vec<&str> = ORDER
        .iter()
        .filter(|f| flags.contains(f))
        .map(|f| f.as_str())
        .collect();

    if labels.is_empty() {
        // Outdated and missing deps are conveyed by their own columns, so an
        // entry can be listed with an empty Info column — exactly like Scoop.
        return Cell::new("");
    }

    let text = labels.join(", ");

    let color = if flags.contains(&StatusInfoFlag::InstallFailed)
        || flags.contains(&StatusInfoFlag::ManifestRemoved)
    {
        Color::Red
    } else if flags.contains(&StatusInfoFlag::Deprecated)
        || flags.contains(&StatusInfoFlag::Outdated)
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
