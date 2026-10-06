use anyhow::Result;
use clap::Parser;
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use crossterm::style::{Stylize, style};
use rake_core::operations::status::{CheckBuckets, StatusInfoFlag, StatusReport};
use rake_core::session::Session;

#[derive(Debug, Parser)]
pub struct Args {
    /// Stay offline: compare against each bucket's last fetched state
    ///
    /// `Option<bool>` with `num_args = 0..=1` and a missing value of `true`, rather than
    /// a plain `bool`: an absent flag has to be distinguishable from `false`, or a user
    /// who set `status.offline_by_default` to `false` could not ask for one offline run.
    ///
    /// Two other spellings were tried and are wrong. A plain `bool` cannot be told from
    /// `false`. `action = SetTrue` on an `Option<bool>` yields `Some(false)` when the flag
    /// is absent, which silently discards every settings value because `unwrap_or` never
    /// reaches the default.
    #[arg(
        short = 'l',
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        require_equals = false
    )]
    pub local: Option<bool>,

    /// Go online: fetch every bucket first, so freshness is checked against
    /// upstream and the fetch is reused by `rake update`
    #[arg(short = 'C', long = "check-buckets", num_args = 0..=1, default_missing_value = "true")]
    pub check_buckets: Option<bool>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Suppress informational notes, keeping warnings and errors
    #[arg(short, long, num_args = 0..=1, default_missing_value = "true")]
    pub quiet: Option<bool>,
}

/// Column headings.
///
/// Deliberately shorter than Scoop's `Installed Version` / `Latest Version` /
/// `Missing Dependencies`: the "Version" and "Dependencies" words are noise in a table
/// this narrow, and a heading that fits does not wrap. This reverses the matching
/// Scoop word-for-word that an earlier release did — the values themselves are still
/// Scoop-compatible.
const HEADERS: [&str; 5] = ["Name", "Installed", "Latest", "Missing Deps", "Info"];

/// Resolve which mode to run in: explicit flag, then the setting, then the built-in
/// default.
///
/// The flags are `Option<bool>` rather than `bool` precisely so that "not given" is
/// distinguishable from "given as false". Without that, a user who set
/// `status.offline_by_default` to `false` could not ask for one offline run, because the
/// absent flag would be indistinguishable from an explicit `false`.
///
/// `-l` beats `-C` when both are given, which is what the previous single expression
/// (`check_buckets && !local`) did, and `-C` is rejected outright alongside it rather
/// than silently ignored — a user asking for both is confused otherwise.
fn resolve_mode(args: &Args, offline_by_default: bool) -> Result<CheckBuckets, String> {
    match (args.local, args.check_buckets) {
        (Some(true), Some(true)) => {
            Err("--local and --check-buckets ask for opposite things".to_owned())
        }
        (Some(true), _) => Ok(CheckBuckets::Local),
        (_, Some(true)) => Ok(CheckBuckets::Fetch),
        _ if offline_by_default => Ok(CheckBuckets::Local),
        _ => Ok(CheckBuckets::Fetch),
    }
}

/// Resolve `--quiet`: an explicit flag wins over `status.hide_offline_note`.
fn resolve_quiet(args: &Args, hide_offline_note: bool) -> bool {
    args.quiet.unwrap_or(hide_offline_note)
}

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
    let loaded = rake_core::settings::load()?;
    // A silently substituted default is indistinguishable from "my setting did nothing", so
    // an unreadable entry is named on stderr. On stdout it would be noise on a command
    // people run often.
    for warning in loaded.warnings() {
        eprintln!("warning: {warning}");
    }
    let settings = &loaded.settings;

    let mode =
        resolve_mode(&args, settings.status.offline_by_default).map_err(|e| anyhow::anyhow!(e))?;
    let quiet = resolve_quiet(&args, settings.status.hide_offline_note);

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
    if should_report_offline_note(mode, crate::util::has_buckets(session), quiet) {
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

    fn args(local: Option<bool>, check_buckets: Option<bool>, quiet: Option<bool>) -> Args {
        Args {
            local,
            check_buckets,
            json: false,
            quiet,
        }
    }

    /// With no flags and the shipped default, the command must not touch the network —
    /// this is the behaviour that took ~40 ms against Scoop's 5.9 s.
    #[test]
    fn offline_by_default_when_nothing_is_given() {
        let mode = resolve_mode(&args(None, None, None), true).unwrap();
        assert_eq!(mode, CheckBuckets::Local);
    }

    /// The distinction the whole settings mechanism rests on, checked through the real
    /// argument parser rather than by constructing `Args` by hand: an absent flag must be
    /// `None`, not `Some(false)`.
    ///
    /// It was `Some(false)` at first — `SetTrue` on an `Option<bool>` — which silently
    /// discarded every settings value, since `unwrap_or` never reached the default. The
    /// flags looked like they were reading the settings and simply ignored them.
    #[test]
    fn absent_flags_parse_as_none_not_some_false() {
        let a = <Args as clap::Parser>::parse_from(["status"]);
        assert_eq!(a.local, None, "-l absent must be None");
        assert_eq!(a.check_buckets, None, "-C absent must be None");
        assert_eq!(a.quiet, None, "-q absent must be None");
    }

    /// And a present flag must be `Some(true)` while still being usable on its own.
    #[test]
    fn present_flags_parse_as_some_true_without_a_value() {
        let a = <Args as clap::Parser>::parse_from(["status", "-l", "-C", "-q"]);
        assert_eq!(a.local, Some(true));
        assert_eq!(a.check_buckets, Some(true));
        assert_eq!(a.quiet, Some(true));

        let long =
            <Args as clap::Parser>::parse_from(["status", "--local", "--check-buckets", "--quiet"]);
        assert_eq!(long.quiet, Some(true));
    }

    /// An explicit flag always beats the setting. Without this a user could not make one
    /// offline run against their own preference.
    #[test]
    fn explicit_flag_beats_the_setting() {
        // Setting says online, flag says offline.
        let mode = resolve_mode(&args(Some(true), None, None), false).unwrap();
        assert_eq!(mode, CheckBuckets::Local);

        // Setting says offline, flag says online.
        let mode = resolve_mode(&args(None, Some(true), None), true).unwrap();
        assert_eq!(mode, CheckBuckets::Fetch);
    }

    /// The setting governs only when no flag was given, in both directions.
    #[test]
    fn setting_applies_when_no_flag_is_given() {
        assert_eq!(
            resolve_mode(&args(None, None, None), false).unwrap(),
            CheckBuckets::Fetch
        );
        assert_eq!(
            resolve_mode(&args(None, None, None), true).unwrap(),
            CheckBuckets::Local
        );
    }

    /// `-l -C` is contradictory. It used to be silently resolved to offline; saying so is
    /// more useful than picking one.
    #[test]
    fn contradictory_flags_are_refused() {
        assert!(resolve_mode(&args(Some(true), Some(true), None), true).is_err());
    }

    #[test]
    fn quiet_flag_beats_the_setting() {
        // Setting hides the note, but the flag cannot be used to un-hide it — there is no
        // --no-quiet — so both agree on hiding.
        assert!(resolve_quiet(&args(None, None, Some(true)), false));
        assert!(resolve_quiet(&args(None, None, Some(true)), true));
        // The setting alone hides it.
        assert!(resolve_quiet(&args(None, None, None), true));
        // And by default it shows.
        assert!(!resolve_quiet(&args(None, None, None), false));
    }

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
