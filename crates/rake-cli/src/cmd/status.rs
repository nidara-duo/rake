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

    /// Suppress informational notes, keeping warnings and errors
    #[arg(short, long)]
    pub quiet: bool,
}

/// Column headings.
///
/// Deliberately shorter than Scoop's `Installed Version` / `Latest Version` /
/// `Missing Dependencies`: the "Version" and "Dependencies" words are noise in a table
/// this narrow, and a heading that fits does not wrap. This reverses the matching
/// Scoop word-for-word that an earlier release did — the values themselves are still
/// Scoop-compatible.
const HEADERS: [&str; 5] = ["Name", "Installed", "Latest", "Missing Deps", "Info"];

/// Whether to print the note explaining that the bucket verdict came from the last
/// fetch rather than from upstream.
///
/// Suppressed by `--quiet`, which is why this is a function rather than a bare `if`:
/// the note is informational, while the "out of date" and "could not check" lines are
/// warnings and stay.
fn should_report_offline_note(mode: CheckBuckets, has_buckets: bool, quiet: bool) -> bool {
    mode == CheckBuckets::Local && has_buckets && !quiet
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
        // command name swapped for rake's own and set in bold rather than wrapped in
        // backticks, which a terminal prints verbatim.
        println!(
            "{} Bucket(s) out of date. Run {} to get the latest changes.",
            style("!").yellow(),
            style("rake update").bold(),
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
    if should_report_offline_note(mode, crate::util::has_buckets(session), args.quiet) {
        println!(
            "{} Bucket state is from the last {} — add --check-buckets to fetch and verify online.",
            style("i").dark_grey(),
            style("rake update").bold(),
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

    table.set_header(
        HEADERS
            .iter()
            .map(|h| {
                Cell::new(*h)
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Green)
            })
            .collect::<Vec<_>>(),
    );

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

#[cfg(test)]
mod tests {
    use super::*;
    use rake_core::operations::status::StatusEntry;

    /// The headings were shortened from Scoop's wording on purpose, which is exactly the
    /// kind of change that gets "fixed" back by someone who assumes it was an accident.
    /// Pinned here so that assumption has to be disproved rather than acted on.
    #[test]
    fn headers_are_the_short_forms() {
        assert_eq!(
            HEADERS,
            ["Name", "Installed", "Latest", "Missing Deps", "Info"]
        );
    }

    /// A row must supply exactly as many cells as there are headers, and the rendered
    /// table must carry every heading. Rendering is what proves the widths line up —
    /// comfy-table has no accessor for the header row.
    #[test]
    fn rendered_table_carries_every_heading() {
        let report = StatusReport {
            entries: vec![StatusEntry {
                name: "git".to_owned(),
                installed_version: Some("2.50.0".to_owned()),
                latest_version: Some("2.51.0".to_owned()),
                missing_dependencies: vec!["7zip".to_owned(), "innounp".to_owned()],
                flags: vec![],
            }],
            buckets_outdated: false,
            buckets_unknown: vec![],
        };

        let rendered = build_entries_table(&report).to_string();
        for header in HEADERS {
            assert!(
                rendered.contains(header),
                "heading {header:?} missing from:\n{rendered}"
            );
        }
        assert!(rendered.contains("git"));
        // Scoop joins dependencies with " | ", and this must survive the shorter heading.
        assert!(rendered.contains("7zip | innounp"), "got:\n{rendered}");
    }

    #[test]
    fn offline_note_is_shown_when_buckets_exist_and_no_fetch_happened() {
        assert!(should_report_offline_note(CheckBuckets::Local, true, false));
    }

    /// Suppressing the note is what `--quiet` is for. Warnings are unaffected, so a
    /// quiet run still reports an out-of-date bucket.
    #[test]
    fn quiet_suppresses_the_offline_note() {
        assert!(!should_report_offline_note(CheckBuckets::Local, true, true));
    }

    /// After a real fetch the note would be false information, so it never appears.
    #[test]
    fn no_note_after_checking_buckets_online() {
        assert!(!should_report_offline_note(
            CheckBuckets::Fetch,
            true,
            false
        ));
    }

    /// With no buckets there is nothing to be stale about.
    #[test]
    fn no_note_without_buckets() {
        assert!(!should_report_offline_note(
            CheckBuckets::Local,
            false,
            false
        ));
    }

    /// Flags keep Scoop's fixed emission order regardless of the order they were set in,
    /// so the column does not shuffle between runs.
    #[test]
    fn flags_render_in_scoop_order() {
        let cell = build_flags_cell(&[
            StatusInfoFlag::ManifestRemoved,
            StatusInfoFlag::Held,
            StatusInfoFlag::InstallFailed,
        ]);
        assert_eq!(
            cell.content(),
            "Install failed, Held package, Manifest removed"
        );

        let reversed = build_flags_cell(&[
            StatusInfoFlag::InstallFailed,
            StatusInfoFlag::Held,
            StatusInfoFlag::ManifestRemoved,
        ]);
        assert_eq!(
            cell.content(),
            reversed.content(),
            "declaration order must not affect the output"
        );
    }

    /// Outdated and missing deps have their own columns, so they contribute no label.
    #[test]
    fn column_carried_flags_render_no_info_label() {
        let cell = build_flags_cell(&[
            StatusInfoFlag::Outdated,
            StatusInfoFlag::MissingDependencies,
        ]);
        assert_eq!(cell.content(), "");
    }
}
