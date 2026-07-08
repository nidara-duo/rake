use anyhow::Result;
use clap::Parser;
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use rake_core::operations::query;
use rake_core::session::Session;
use rake_domain::package::PackageStatus;
use std::collections::HashSet;

/// List installed apps
#[derive(Debug, Parser)]
pub struct Args {
    /// Query string (regex supported)
    query: Option<String>,
    /// Turn regex off and use explicit matching
    #[arg(short = 'e', long)]
    explicit: bool,
    /// List upgradable package(s)
    #[arg(short = 'u', long)]
    upgradable: bool,
    /// List held package(s)
    #[arg(short = 'H', long)]
    held: bool,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let packages = query::query_installed(session)?;
    let latest_versions = query::latest_versions_for_installed(session, &packages)?;
    let held_buckets: HashSet<String> = rake_core::operations::bucket::bucket_held_names(session)?
        .into_iter()
        .collect();

    let mut filtered: Vec<_> = packages
        .into_iter()
        .filter(|p| matches!(p.status, PackageStatus::Installed(_)))
        .filter(|p| {
            if let Some(ref q) = args.query {
                if args.explicit {
                    p.name().eq_ignore_ascii_case(q)
                } else {
                    p.name()
                        .to_ascii_lowercase()
                        .contains(&q.to_ascii_lowercase())
                }
            } else {
                true
            }
        })
        .collect();

    filtered.sort_by(|a, b| a.name().cmp(b.name()));

    let mut table = Table::new();
    table.load_preset(NOTHING);

    let header = ["Name", "Version", "Available", "Bucket", "Status"]
        .into_iter()
        .map(|h| Cell::new(h).add_attribute(Attribute::Bold).fg(Color::Green));
    table.set_header(header);

    let mut has_rows = false;
    for pkg in &filtered {
        let state = match &pkg.status {
            PackageStatus::Installed(s) => s,
            _ => continue,
        };

        let is_bucket_held = held_buckets.contains(pkg.bucket());
        let is_pkg_held = state.held;

        // Apply filters
        if args.held && !is_pkg_held && !is_bucket_held {
            continue;
        }

        let name_lower = pkg.name().to_ascii_lowercase();
        let latest = latest_versions.get(&name_lower);

        let is_outdated = latest
            .map(|lv| {
                rake_domain::version::compare_versions(lv, &state.version)
                    == std::cmp::Ordering::Greater
            })
            .unwrap_or(false);

        if args.upgradable && !is_outdated {
            continue;
        }

        has_rows = true;

        let version_cell = if is_outdated {
            Cell::new(&state.version).add_attribute(Attribute::Dim)
        } else if is_pkg_held || is_bucket_held {
            Cell::new(&state.version).fg(Color::Blue)
        } else {
            Cell::new(&state.version)
        };

        let available_cell = match latest {
            Some(v) if is_outdated => Cell::new(v).fg(Color::Blue),
            _ => Cell::new(""),
        };

        let bucket_cell = if is_bucket_held {
            Cell::new(pkg.bucket())
                .fg(Color::Yellow)
                .add_attribute(Attribute::Dim)
        } else {
            Cell::new(pkg.bucket()).fg(Color::Green)
        };

        let status_cell = if is_pkg_held || is_bucket_held {
            Cell::new("held").fg(Color::Yellow)
        } else if is_outdated {
            Cell::new("outdated").fg(Color::Magenta)
        } else {
            Cell::new("✓").fg(Color::Green)
        };

        table.add_row(vec![
            Cell::new(pkg.name()),
            version_cell,
            available_cell,
            bucket_cell,
            status_cell,
        ]);
    }

    if has_rows {
        println!("{table}");
    } else {
        println!("No packages found.");
    }
    Ok(())
}
