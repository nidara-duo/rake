use anyhow::Result;
use clap::Parser;
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use rake_core::operations::query;
use rake_core::session::Session;
use rake_domain::package::PackageStatus;

/// Search available packages from synced buckets
#[derive(Debug, Parser)]
#[clap(arg_required_else_help = true)]
pub struct Args {
    /// Query string
    #[arg(required = true)]
    query: Vec<String>,

    /// Use exact matching instead of substring
    #[arg(short = 'e', long)]
    explicit: bool,

    /// Search through package descriptions as well
    #[arg(short = 'D', long)]
    with_description: bool,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let snap = query::collect_snapshot(session)?;
    let synced = snap.synced;
    let installed = snap.installed;

    let installed_map: std::collections::HashMap<String, (String, bool)> = installed
        .into_iter()
        .filter(|p| matches!(p.status, PackageStatus::Installed(_)))
        .filter_map(|p| {
            let state = match &p.status {
                PackageStatus::Installed(s) => s,
                _ => return None,
            };
            Some((
                p.name().to_ascii_lowercase(),
                (state.version.clone(), state.held),
            ))
        })
        .collect();

    let filtered: Vec<_> = synced
        .into_iter()
        .filter(|pkg| {
            let name_lower = pkg.name().to_ascii_lowercase();
            let desc_lower = pkg
                .manifest
                .description
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            args.query.iter().any(|q| {
                let q_lower = q.to_ascii_lowercase();
                let name_match = if args.explicit {
                    name_lower == q_lower
                } else {
                    name_lower.contains(&q_lower)
                };
                if name_match {
                    return true;
                }
                if args.with_description {
                    if args.explicit {
                        desc_lower == q_lower
                    } else {
                        desc_lower.contains(&q_lower)
                    }
                } else {
                    false
                }
            })
        })
        .collect();

    if filtered.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(NOTHING);

    let header = ["Name", "Version", "Bucket", "Info"]
        .into_iter()
        .map(|h| Cell::new(h).add_attribute(Attribute::Bold).fg(Color::Green));
    table.set_header(header);

    for pkg in &filtered {
        let name_lower = pkg.name().to_ascii_lowercase();
        let installed_info = installed_map.get(&name_lower);

        let name_cell = match installed_info {
            Some(_) => Cell::new(pkg.name()).fg(Color::Blue),
            None => Cell::new(pkg.name()),
        };

        let version_cell = match installed_info {
            Some((installed_ver, _held)) if installed_ver == pkg.version() => {
                Cell::new(pkg.version()).add_attribute(Attribute::Dim)
            }
            Some((installed_ver, _held)) => {
                Cell::new(format!("{} [installed: {}]", pkg.version(), installed_ver))
            }
            None => Cell::new(pkg.version()),
        };

        let info_cell = match installed_info {
            Some((_ver, held)) if *held => Cell::new("held").fg(Color::Yellow),
            Some(_) => Cell::new("installed").fg(Color::Blue),
            None => Cell::new(""),
        };

        table.add_row(vec![
            name_cell,
            version_cell,
            Cell::new(pkg.bucket()),
            info_cell,
        ]);
    }

    println!("{table}");
    Ok(())
}
