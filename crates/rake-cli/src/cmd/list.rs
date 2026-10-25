use anyhow::Result;
use clap::Parser;
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use rake_core::operations::query;
use rake_core::session::Session;
use rake_domain::package::{InstallState, PackageStatus};
use rake_domain::version::compare_versions;
use std::collections::HashSet;

/// What the `Status` column says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowStatus {
    Held,
    Outdated,
    Ok,
    InstallFailed,
}

/// The text of one row, before any colouring.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowText {
    version: String,
    available: String,
    bucket: String,
    status: RowStatus,
    outdated: bool,
}

impl RowText {
    /// Build the row for one installed package.
    ///
    /// An interrupted install has no trustworthy version and no bucket, so
    /// `scoop list` prints neither and says `Install failed` instead of a
    /// tick. That verdict comes from [`rake_core::operations::query`], not from
    /// comparing against a bucket — an unknown bucket is not an up-to-date one.
    fn new(
        state: &InstallState,
        install_failed: bool,
        latest: Option<&str>,
        bucket_held: bool,
    ) -> Self {
        if install_failed {
            return Self {
                version: String::new(),
                available: String::new(),
                bucket: String::new(),
                status: RowStatus::InstallFailed,
                outdated: false,
            };
        }

        let outdated = latest
            .is_some_and(|v| compare_versions(v, &state.version) == std::cmp::Ordering::Greater);

        let status = if state.held || bucket_held {
            RowStatus::Held
        } else if outdated {
            RowStatus::Outdated
        } else {
            RowStatus::Ok
        };

        Self {
            version: state.version.clone(),
            available: if outdated {
                latest.unwrap_or_default().to_owned()
            } else {
                String::new()
            },
            // A URL-sourced or local install has no bucket; the recorded
            // manifest URL is the only source there is.
            bucket: state
                .bucket
                .clone()
                .or_else(|| state.url.clone())
                .unwrap_or_default(),
            status,
            outdated,
        }
    }
}

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
        let latest = if pkg.install_failed {
            None
        } else {
            latest_versions.get(&name_lower).map(String::as_str)
        };
        let row = RowText::new(state, pkg.install_failed, latest, is_bucket_held);

        if args.upgradable && !row.outdated {
            continue;
        }

        has_rows = true;

        let version_cell = match row.status {
            RowStatus::InstallFailed => Cell::new(""),
            RowStatus::Outdated => Cell::new(&row.version).add_attribute(Attribute::Dim),
            RowStatus::Held => Cell::new(&row.version).fg(Color::Blue),
            RowStatus::Ok => Cell::new(&row.version),
        };

        let available_cell = if row.available.is_empty() {
            Cell::new("")
        } else {
            Cell::new(&row.available).fg(Color::Blue)
        };

        let bucket_cell = if is_bucket_held {
            Cell::new(&row.bucket)
                .fg(Color::Yellow)
                .add_attribute(Attribute::Dim)
        } else {
            Cell::new(&row.bucket).fg(Color::Green)
        };

        let status_cell = match row.status {
            RowStatus::InstallFailed => Cell::new("Install failed").fg(Color::Red),
            RowStatus::Held => Cell::new("held").fg(Color::Yellow),
            RowStatus::Outdated => Cell::new("outdated").fg(Color::Magenta),
            RowStatus::Ok => Cell::new("✓").fg(Color::Green),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state(version: &str, bucket: Option<&str>, url: Option<&str>) -> InstallState {
        InstallState {
            version: version.to_owned(),
            bucket: bucket.map(str::to_owned),
            arch: "64bit".to_owned(),
            held: false,
            url: url.map(str::to_owned),
        }
    }

    #[test]
    fn failed_install_shows_no_version_bucket_or_tick() {
        let row = RowText::new(&state("0.4.0", None, None), true, None, false);

        assert_eq!(row.version, "", "a broken install has no usable version");
        assert_eq!(row.bucket, "", "and no bucket to name");
        assert_eq!(row.available, "");
        assert_eq!(row.status, RowStatus::InstallFailed);
        assert!(!row.outdated, "a failed install is never up to date");
    }

    #[test]
    fn healthy_install_shows_version_bucket_and_tick() {
        let row = RowText::new(&state("1.2.3", Some("main"), None), false, None, false);

        assert_eq!(row.version, "1.2.3");
        assert_eq!(row.bucket, "main");
        assert_eq!(row.status, RowStatus::Ok);
    }

    #[test]
    fn outdated_install_shows_the_available_version() {
        let row = RowText::new(
            &state("1.2.3", Some("main"), None),
            false,
            Some("1.3.0"),
            false,
        );

        assert_eq!(row.available, "1.3.0");
        assert_eq!(row.status, RowStatus::Outdated);
        assert!(row.outdated);
    }

    /// A URL-sourced install has no bucket, so the recorded manifest URL is
    /// what the Bucket column shows.
    #[test]
    fn url_install_shows_the_manifest_url() {
        let row = RowText::new(
            &state("1.0", None, Some("https://example.com/app.json")),
            false,
            None,
            false,
        );

        assert_eq!(row.bucket, "https://example.com/app.json");
        assert_eq!(row.status, RowStatus::Ok);
    }

    #[test]
    fn held_install_reports_held_even_when_outdated() {
        let mut install = state("1.2.3", Some("main"), None);
        install.held = true;
        let row = RowText::new(&install, false, Some("1.3.0"), false);

        assert_eq!(row.status, RowStatus::Held);
        assert!(
            row.outdated,
            "held is a label, not a reason to hide the update"
        );
    }
}
