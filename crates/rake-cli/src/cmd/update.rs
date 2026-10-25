use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::style::{Stylize, style};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rake_core::event::{BucketState, Event};
use rake_core::operations::query;
use rake_core::operations::update::{UpdateSpec, update_packages};
use rake_core::operations::{download, update};
use rake_core::session::Session;
use rake_domain::arch::Arch;
use rake_domain::package::{Package, PackageStatus};
use rake_domain::version::compare_versions;

/// Update installed packages to latest versions
#[derive(Debug, Parser)]
pub struct Args {
    /// Package(s) to update (use `*` for all installed)
    pub names: Vec<String>,
    /// Update every installed package (same as passing `*`)
    #[arg(long)]
    pub all: bool,
    /// Force reinstall even if up-to-date
    #[arg(short, long)]
    pub force: bool,
    /// Skip cache and download fresh
    #[arg(short = 'k', long)]
    pub no_cache: bool,
    /// Suppress progress output
    #[arg(short, long)]
    pub quiet: bool,
    /// List every bucket and its outcome as text, with no progress display
    //
    // `Option<bool>` rather than `bool` for the same reason as `status --quiet`: "not
    // given" has to be distinguishable from "given as false", or a user who set
    // `update.verbose_by_default` to `false` could not ask for one verbose run. The
    // `num_args`/`default_missing_value` pair is what makes a bare `-v` mean true; with
    // `SetTrue` it would hand back `Some(false)` when absent and quietly invert the
    // setting.
    #[arg(
        short,
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        action = clap::ArgAction::Set
    )]
    pub verbose: Option<bool>,
    /// Skip confirmation prompt
    #[arg(short, long)]
    pub yes: bool,
    /// Run the work-in-progress installer instead of the download-only stub
    #[arg(long, hide = true)]
    pub unstable_install: bool,
}

use crate::util::human_size;

fn get_arch(arch_str: Option<&str>) -> Arch {
    match arch_str {
        Some("32bit") => Arch::Ia32,
        Some("64bit") => Arch::Amd64,
        Some("arm64") => Arch::Aarch64,
        _ => Arch::current(),
    }
}

struct DoneFlag(Arc<AtomicBool>);

impl DoneFlag {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }
}

impl Drop for DoneFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn finish_message(pb: &ProgressBar, msg: impl Into<String>) {
    pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
    pb.finish_with_message(msg.into());
}

fn short_name(name: &str) -> String {
    const MAX: usize = 40;
    if name.chars().count() <= MAX {
        return name.to_owned();
    }

    let tail: String = name
        .chars()
        .rev()
        .take(MAX.saturating_sub(3))
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    format!("...{tail}")
}

fn spawn_update_ui(
    session: &Session,
    quiet: bool,
    done: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let rx = session.event_bus().core_receiver();

    thread::spawn(move || {
        let mp = MultiProgress::new();
        let bar_style = ProgressStyle::with_template(
            "{msg:<30} [{bar:>20}] {bytes:>10}/{total_bytes:<10} {percent:>3}%",
        )
        .unwrap()
        .progress_chars("=> ");

        let mut download_bars: HashMap<String, ProgressBar> = HashMap::new();

        let mut handle_event = |event: Event| match event {
            Event::DownloadStart(ident) => {
                if quiet {
                    return;
                }

                let key = ident.to_string();
                let pb = mp.add(ProgressBar::new(0));
                pb.set_style(bar_style.clone());
                pb.set_message(format!("{} {}", ident, style("downloading").dim()));
                download_bars.insert(key, pb);
            }
            Event::DownloadProgress(ev) => {
                if quiet {
                    return;
                }

                let key = ev.ident.to_string();
                if let Some(pb) = download_bars.get(&key) {
                    if ev.total_bytes > 0 {
                        pb.set_length(ev.total_bytes);
                        pb.set_position(ev.downloaded_bytes);
                    }
                    let fname = short_name(&ev.filename);
                    pb.set_message(format!("{}  {}", ev.ident, style(fname).dim()));
                }
            }
            Event::DownloadCached(ident) => {
                if quiet {
                    return;
                }

                if let Some(pb) = download_bars.remove(&ident.to_string()) {
                    let _ = mp.println(format!(
                        " {} {} {}",
                        style("✓").green(),
                        ident,
                        style("(cached)").dim(),
                    ));
                    pb.finish_and_clear();
                }
            }
            Event::DownloadError(ident, msg) => {
                if quiet {
                    return;
                }

                if let Some(pb) = download_bars.remove(&ident.to_string()) {
                    finish_message(&pb, format!("{} {} ({})", style("✗").red(), ident, msg));
                }
            }
            Event::DownloadDone => {
                if quiet {
                    return;
                }

                for (_, pb) in download_bars.drain() {
                    pb.finish_and_clear();
                }
            }

            Event::UpdateStart(ident, old_ver, new_ver) => {
                if quiet {
                    return;
                }

                let _ = mp.println(format!(
                    " {} {} ({} → {})",
                    style("→").cyan(),
                    style(ident).bold(),
                    old_ver,
                    new_ver,
                ));
            }
            Event::UpdateProgress(msg) => {
                if quiet || msg.is_empty() {
                    return;
                }

                let _ = mp.println(format!("   {}", msg));
            }
            Event::UpdateDone(ident) => {
                if quiet {
                    return;
                }

                let _ = mp.println(format!(
                    " {} {} {}",
                    style("✓").green(),
                    style(ident).green(),
                    style("updated successfully").green(),
                ));
            }

            Event::CommitStart(ident, version) => {
                if quiet {
                    return;
                }

                let _ = mp.println(format!(
                    "   Linking {}/{} ({})",
                    style(&ident.bucket).bold(),
                    style(&ident.name).bold(),
                    version,
                ));
            }
            Event::CommitProgress(msg) => {
                if quiet {
                    return;
                }

                let _ = mp.println(format!("    {}", msg));
            }
            Event::CommitDone(ident) => {
                if quiet {
                    return;
                }

                let _ = mp.println(format!(
                    "   {} {} linked",
                    style("✓").green(),
                    style(ident).green(),
                ));
            }

            _ => {}
        };

        while !done.load(Ordering::Acquire) {
            if let Ok(event) = rx.recv_timeout(Duration::from_millis(50)) {
                handle_event(event);

                while let Ok(event) = rx.try_recv() {
                    handle_event(event);
                }
            }
        }

        for (_, pb) in download_bars.drain() {
            pb.finish_and_clear();
        }
    })
}

/// Whether a progress display can draw at all.
///
/// Decided from stderr before any thread starts, because that is where the display writes.
/// `MultiProgress::is_hidden()` is not a usable substitute: it reported "visible" for a
/// run whose output was redirected to a file and where the display then drew nothing at
/// all, which cost the user every bucket name in the summary.
///
/// Tying it to stderr rather than stdout is deliberate. Piping stdout while watching the
/// console — `rake update | tee log` — still shows the spinner, and the summary stays
/// quiet to match. Redirecting both hides the display, so the summary prints everything.
fn display_can_draw() -> bool {
    std::io::stderr().is_terminal()
}

/// Whether a progress display should run at all.
///
/// Split from [`display_can_draw`] so the decision is testable. There is no terminal to
/// borrow here, and this is the half that decides whether the user loses information.
fn should_draw(mode: BucketReport, stderr_is_terminal: bool) -> bool {
    mode.wants_progress() && stderr_is_terminal
}

/// Draw the bucket-sync progress.
///
/// There is no mode parameter on purpose: the caller decides whether a display runs at
/// all, and a parameter that could only ever be `Default` would invite a later caller to
/// switch on a spinner in verbose mode without meaning to.
///
/// Two events can arrive together: one taken from the queue with a timeout, then
/// whatever else is already queued. Both go through one handler. The two copies this
/// replaced had already drifted — one wrote `✗ name` and the other `✗  name` — which is
/// the usual way duplicated code announces that it should not have been duplicated.
fn spawn_bucket_ui(session: &Session, done: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    let rx = session.event_bus().core_receiver();

    thread::spawn(move || {
        let mp = MultiProgress::new();

        let spinner_style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);

        let mut spinners: HashMap<String, ProgressBar> = HashMap::new();

        // Returns true when the stream is over, so the caller can stop early instead of
        // waiting for the done flag.
        let mut handle = |event: &Event| -> bool {
            match event {
                Event::BucketSyncDone => true,
                Event::BucketSyncProgress { name, state } => {
                    match state {
                        BucketState::Started => {
                            let pb = mp.add(ProgressBar::new_spinner());
                            pb.set_style(spinner_style.clone());
                            pb.set_message(name.to_string());
                            pb.enable_steady_tick(Duration::from_millis(80));
                            spinners.insert(name.clone(), pb);
                        }
                        BucketState::Succeeded => {
                            if let Some(pb) = spinners.remove(name) {
                                pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
                                pb.finish_with_message(format!("{} {}", "✓".green(), name));
                            }
                        }
                        BucketState::Failed(msg) => {
                            if let Some(pb) = spinners.remove(name) {
                                pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
                                pb.finish_with_message(format!("{} {} ({})", "✗".red(), name, msg));
                            }
                        }
                    }
                    false
                }
                _ => false,
            }
        };

        let mut stop = false;
        while !stop && !done.load(Ordering::Acquire) {
            if let Ok(event) = rx.recv_timeout(Duration::from_millis(50)) {
                stop = handle(&event);
                while !stop {
                    match rx.try_recv() {
                        Ok(event) => stop = handle(&event),
                        Err(_) => break,
                    }
                }
            }
        }

        for (_, pb) in spinners.drain() {
            pb.finish_and_clear();
        }
    })
}

/// How much of a bucket sync to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BucketReport {
    /// Progress display while syncing, then a line for anything it could not show.
    Default,
    /// A line per bucket, always, and no progress display.
    Verbose,
    /// Nothing informational.
    Quiet,
}

impl BucketReport {
    fn from_flags(verbose: bool, quiet: bool) -> Self {
        match (verbose, quiet) {
            (true, true) => Self::Quiet,
            (true, false) => Self::Verbose,
            (false, true) => Self::Quiet,
            (false, false) => Self::Default,
        }
    }

    fn wants_progress(self) -> bool {
        self == Self::Default
    }
}

/// Resolve `--verbose`: an explicit flag wins over `update.verbose_by_default`.
///
/// Same shape as `resolve_quiet` in `status`, and for the same reason: the flag has to be
/// able to say both "yes, be verbose" and "no, not this time", which `bool` cannot do.
fn resolve_verbose(flag: Option<bool>, verbose_by_default: bool) -> bool {
    flag.unwrap_or(verbose_by_default)
}

/// Whether an outcome still needs a line of its own after the progress display has run.
///
/// The invariant that was broken: the display leaves `✓ extras` behind as a permanent
/// line, and the summary then printed the same thing again. So an outcome the display
/// already drew must not be repeated, while one it never drew must not be dropped —
/// held buckets and non-repositories are not progress events at all, and piped output has
/// no display to draw anything.
fn needs_report(outcome: &update::BucketSyncOutcome, display_drew: bool) -> bool {
    match outcome {
        update::BucketSyncOutcome::Updated | update::BucketSyncOutcome::Failed(_) => !display_drew,
        update::BucketSyncOutcome::Held | update::BucketSyncOutcome::NotARepository => true,
    }
}

/// Update installed packages to latest versions
pub async fn execute(args: Args, session: &Session) -> Result<()> {
    let installed = query::query_installed(session)?;
    let has_wildcard = args.all || args.names.iter().any(|n| n == "*");
    let effective_names: Vec<String> = if has_wildcard {
        installed.iter().map(|p| p.name().to_owned()).collect()
    } else {
        args.names.clone()
    };

    if effective_names.is_empty() && !has_wildcard {
        let loaded = rake_core::settings::load()?;
        // Reported on stderr, as in `status`: a silently substituted default looks
        // exactly like a preference that did nothing, and `update` prints to stdout.
        for warning in loaded.warnings() {
            eprintln!("warning: {warning}");
        }

        let verbose = resolve_verbose(args.verbose, loaded.settings.update.verbose_by_default);
        let mode = BucketReport::from_flags(verbose, args.quiet);
        return bucket_only(session, mode).await;
    }
    if effective_names.is_empty() {
        println!("No installed packages to update.");
        return Ok(());
    }

    let synced = rake_core::operations::query::find_synced_by_names(
        session,
        &effective_names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )?;

    let installed_by_name: HashMap<&str, &Package> =
        installed.iter().map(|p| (p.name(), p)).collect();

    let mut specs: Vec<UpdateSpec> = Vec::new();

    for target_name in &effective_names {
        let installed_pkg = match installed_by_name.get(target_name.as_str()) {
            Some(p) => *p,
            None => {
                eprintln!(" '{}' is not installed", style(target_name).red());
                continue;
            }
        };

        let arch = match &installed_pkg.status {
            PackageStatus::Installed(state) => get_arch(Some(&state.arch)),
            _ => Arch::current(),
        };

        let candidate = match synced
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case(target_name))
        {
            Some(p) => p,
            None => {
                eprintln!(" '{}' not found in any bucket", style(target_name).red());
                continue;
            }
        };

        let held = match &installed_pkg.status {
            PackageStatus::Installed(state) => state.held,
            _ => false,
        };

        if held && !args.force {
            eprintln!(
                " '{}' is held — use --force to override",
                style(target_name).yellow()
            );
            continue;
        }

        let old_ver = installed_pkg.version();
        let new_ver = candidate.version();
        let outdated = compare_versions(old_ver, new_ver) == std::cmp::Ordering::Less;

        if !outdated && !args.force {
            eprintln!(
                " {} '{}' ({}) is already at latest version",
                style("✓").green(),
                style(target_name).green(),
                old_ver,
            );
            continue;
        }

        specs.push(UpdateSpec {
            installed: installed_pkg.clone(),
            candidate: candidate.clone(),
            arch,
            downloaded: Vec::new(),
        });
    }

    if specs.is_empty() {
        return Ok(());
    }

    // --- Confirmation ---
    if !args.yes {
        println!("Calculating download size...");

        let mut total_size: u64 = 0;
        let mut estimated = false;
        for spec in &specs {
            let s = download::calculate_download_size(
                session,
                std::slice::from_ref(&spec.candidate),
                spec.arch,
            )
            .await?;
            total_size += s.total;
            estimated |= s.estimated;
        }

        let size_str = human_size(total_size);
        let est = if estimated { " (estimated)" } else { "" };
        print!("Total download size: {size_str}{est}. Continue? [y/N]: ");
        use std::io::{Write, stdout};
        stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let done = Arc::new(AtomicBool::new(false));
    let _guard = DoneFlag::new(done.clone());
    let ui_handle = spawn_update_ui(session, args.quiet, done);

    if !args.quiet {
        println!("Resolving packages...");
    }

    for spec in &mut specs {
        if !args.quiet {
            println!(
                "Updating '{}' ({} → {})",
                style(spec.candidate.name()).bold(),
                spec.installed.version(),
                spec.candidate.version(),
            );
        }

        let reuse_cache = !args.no_cache;
        let files = download::download_packages(
            session,
            std::slice::from_ref(&spec.candidate),
            reuse_cache,
            spec.arch,
        )
        .await?;

        if files.is_empty() {
            eprintln!(
                " {} '{}' failed to download (all mirrors failed verification) — skipping update",
                style("✗").red(),
                spec.candidate.name(),
            );
        }

        spec.downloaded = files;
    }

    specs.retain(|s| !s.downloaded.is_empty());

    if specs.is_empty() {
        drop(_guard);
        let _ = ui_handle.join();
        println!("No packages were updated.");
        return Ok(());
    }

    // Safe stub: the artifacts above are downloaded and verified, but nothing
    // has been written into `apps/<app>` yet. `--unstable-install` is the only
    // way to hand control to the work-in-progress installer.
    if !args.unstable_install {
        drop(_guard);
        let _ = ui_handle.join();

        for spec in &specs {
            crate::util::report_install_stub("update", spec.candidate.name(), &spec.downloaded);
        }

        return Ok(());
    }

    let result = update_packages(session, &specs).await;

    drop(_guard);
    let _ = ui_handle.join();

    match result {
        Ok(updated) => {
            for pkg in &updated {
                println!(
                    " {} '{}' ({}) updated!",
                    style("✓").green(),
                    style(pkg.name()).green(),
                    pkg.version(),
                );
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Update failed: {e}");
            Err(e.into())
        }
    }
}

/// Bucket-sync only mode: update all buckets.
async fn bucket_only(session: &Session, mode: BucketReport) -> Result<()> {
    let all_buckets = rake_core::operations::bucket::bucket_list(session)?;

    if all_buckets.is_empty() {
        println!("No buckets to update.");
        return Ok(());
    }

    let quiet = mode == BucketReport::Quiet;

    // No thread at all when nobody can see it: verbose output is text from start to
    // finish, and a display with no terminal is a thread waiting on a channel for nothing.
    let show_display = should_draw(mode, display_can_draw());
    let ui = show_display.then(|| {
        let done = Arc::new(AtomicBool::new(false));
        let guard = DoneFlag::new(done.clone());
        (spawn_bucket_ui(session, done), guard)
    });

    if !quiet {
        let count = all_buckets.len();
        if count == 1 {
            println!("Updating bucket");
        } else {
            println!("Updating buckets");
        }
    }

    let report = update::bucket_update(session).await?;

    let display_drew = match ui {
        Some((handle, guard)) => {
            drop(guard);
            // The outcome of the join does not matter: the display was started because a
            // terminal was there to receive it, so it drew. Only the `None` arm can mean
            // "nothing was shown", and that is decided before any thread runs.
            let _ = handle.join();
            true
        }
        None => false,
    };

    // Each outcome is printed exactly once, by whichever channel is actually visible.
    for (name, outcome) in &report.outcomes {
        if !needs_report(outcome, display_drew) {
            continue;
        }

        match outcome {
            update::BucketSyncOutcome::Updated => {
                if !quiet {
                    println!("{} {}", style("✓").green(), name);
                }
            }
            update::BucketSyncOutcome::Held => {
                println!(
                    "{} {} is held — not updated (use 'rake bucket unhold {}')",
                    style("!").yellow(),
                    name,
                    name
                );
            }
            update::BucketSyncOutcome::NotARepository => {
                println!(
                    "{} {} is not a git repository — not updated",
                    style("!").yellow(),
                    name
                );
            }
            update::BucketSyncOutcome::Failed(reason) => {
                println!("{} {} ({})", style("✗").red(), name, reason);
            }
        }
    }

    if report.failed() > 0 {
        // Non-zero, so a script running `rake update` notices. The per-bucket reasons are
        // already printed above rather than folded into this message.
        return Err(anyhow::anyhow!(
            "{} of {} buckets failed to update",
            report.failed(),
            report.outcomes.len()
        ));
    }

    if !quiet {
        if report.updated() == 0 && report.skipped() > 0 {
            println!("Nothing was updated: every bucket is held or not a repository.");
        } else {
            println!("Everything is up to date!");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_core::operations::update::BucketSyncOutcome as Outcome;

    /// The chain, flag → setting → built-in default, stated as the three cases that
    /// matter to a user. The built-in default is `true` because Scoop lists the buckets.
    #[test]
    fn verbose_follows_flag_then_setting_then_default() {
        // No flag, default setting: verbose, like scoop.
        assert!(resolve_verbose(None, true));
        // No flag, user turned it off.
        assert!(!resolve_verbose(None, false));
        // Flag on, user turned it off — one verbose run without changing the setting.
        assert!(resolve_verbose(Some(true), false));
        // Flag off, user turned it on — one short run without changing the setting.
        assert!(!resolve_verbose(Some(false), true));
    }

    /// `-v` with no value means "yes", and `--verbose false` means "no". Without the
    /// `num_args`/`default_missing_value` pair clap would demand an argument for the
    /// flag, or hand back `Some(false)` and quietly override the setting the wrong way.
    #[test]
    fn the_flag_can_be_written_both_ways() {
        let args = Args::try_parse_from(["rake", "update", "-v"]).unwrap();
        assert_eq!(args.verbose, Some(true), "-v alone must mean true");

        let args = Args::try_parse_from(["rake", "update", "--verbose"]).unwrap();
        assert_eq!(args.verbose, Some(true), "--verbose alone must mean true");

        let args = Args::try_parse_from(["rake", "update", "-v", "false"]).unwrap();
        assert_eq!(
            args.verbose,
            Some(false),
            "an explicit value must be honoured"
        );

        let args = Args::try_parse_from(["rake", "update"]).unwrap();
        assert_eq!(args.verbose, None, "absent must stay absent");
    }

    /// Turning the setting off gives the short form; the flag still reaches it.
    #[test]
    fn the_setting_selects_short_output_and_the_flag_still_overrides() {
        let setting_off = rake_domain::settings::Settings::default();
        let mut setting_off = setting_off;
        setting_off
            .set("update.verbose_by_default", "false")
            .unwrap();

        let mode = BucketReport::from_flags(
            resolve_verbose(None, setting_off.update.verbose_by_default),
            false,
        );
        assert_eq!(
            mode,
            BucketReport::Default,
            "the short form has a progress display"
        );

        let forced = BucketReport::from_flags(
            resolve_verbose(Some(true), setting_off.update.verbose_by_default),
            false,
        );
        assert_eq!(forced, BucketReport::Verbose);
    }

    /// Quiet still wins over verbose, whichever asked for it — the user asked for less
    /// output twice, and honouring the quieter cannot add noise.
    #[test]
    fn quiet_wins_over_verbose_in_either_order() {
        assert_eq!(BucketReport::from_flags(true, true), BucketReport::Quiet);
        assert_eq!(BucketReport::from_flags(false, true), BucketReport::Quiet);
    }

    /// The bug: the display leaves `✓ extras` on screen and the summary printed it again.
    #[test]
    fn an_outcome_the_display_drew_is_not_reported_again() {
        assert!(!needs_report(&Outcome::Updated, true));
        assert!(!needs_report(&Outcome::Failed("boom".into()), true));
    }

    /// The other half of the same invariant: if the display drew nothing — piped output,
    /// where indicatif suppresses itself — nothing may be dropped.
    #[test]
    fn an_undrawn_display_reports_everything() {
        assert!(needs_report(&Outcome::Updated, false));
        assert!(needs_report(&Outcome::Failed("boom".into()), false));
    }

    /// Held buckets and non-repositories are not progress events, so the display never
    /// shows them and they must be reported whether it drew or not.
    #[test]
    fn outcomes_the_display_never_draws_are_always_reported() {
        for drew in [true, false] {
            assert!(needs_report(&Outcome::Held, drew), "held, drew={drew}");
            assert!(
                needs_report(&Outcome::NotARepository, drew),
                "not-a-repo, drew={drew}"
            );
        }
    }

    #[test]
    fn flags_select_the_mode() {
        assert_eq!(
            BucketReport::from_flags(false, false),
            BucketReport::Default
        );
        assert_eq!(BucketReport::from_flags(true, false), BucketReport::Verbose);
        // Asking for both is contradictory, but quiet wins: the user asked for less
        // output twice, and honouring the quieter of the two cannot add noise.
        assert_eq!(BucketReport::from_flags(true, true), BucketReport::Quiet);
        assert_eq!(BucketReport::from_flags(false, true), BucketReport::Quiet);
    }

    /// Only the default mode runs a progress display. Verbose is text throughout, so a
    /// spinner would only flicker over it.
    #[test]
    fn only_the_default_mode_draws_progress() {
        assert!(BucketReport::Default.wants_progress());
        assert!(!BucketReport::Verbose.wants_progress());
        assert!(!BucketReport::Quiet.wants_progress());
    }

    /// All four combinations, because this is the decision that decides whether the user
    /// sees the bucket names at all.
    #[test]
    fn a_display_needs_both_the_default_mode_and_a_terminal() {
        assert!(should_draw(BucketReport::Default, true));
        // No terminal: nothing can be drawn, so the summary has to print every bucket.
        // Assuming otherwise is what lost them in the first attempt.
        assert!(!should_draw(BucketReport::Default, false));
        assert!(!should_draw(BucketReport::Verbose, true));
        assert!(!should_draw(BucketReport::Quiet, true));
        assert!(!should_draw(BucketReport::Quiet, false));
    }

    /// The two properties that must hold together, whichever way the display is decided:
    /// every outcome is named exactly once.
    #[test]
    fn each_outcome_is_named_exactly_once_in_every_configuration() {
        let outcomes = [
            Outcome::Updated,
            Outcome::Failed("boom".into()),
            Outcome::Held,
            Outcome::NotARepository,
        ];

        for mode in [
            BucketReport::Default,
            BucketReport::Verbose,
            BucketReport::Quiet,
        ] {
            for terminal in [true, false] {
                let drew = should_draw(mode, terminal);
                for outcome in &outcomes {
                    let shown_by_display =
                        drew && matches!(outcome, Outcome::Updated | Outcome::Failed(_));
                    let shown_by_summary = needs_report(outcome, drew);
                    assert!(
                        shown_by_display ^ shown_by_summary,
                        "{mode:?} terminal={terminal} {outcome:?}: printed twice or not at all"
                    );
                }
            }
        }
    }
}
