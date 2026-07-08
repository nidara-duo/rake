use std::collections::HashMap;
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
    /// Force reinstall even if up-to-date
    #[arg(short, long)]
    pub force: bool,
    /// Skip cache and download fresh
    #[arg(short = 'k', long)]
    pub no_cache: bool,
    /// Suppress progress output
    #[arg(short, long)]
    pub quiet: bool,
    /// Skip confirmation prompt
    #[arg(short, long)]
    pub yes: bool,
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

fn spawn_bucket_ui(
    session: &Session,
    quiet: bool,
    done: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let rx = session.event_bus().core_receiver();

    thread::spawn(move || {
        let mp = MultiProgress::new();

        let spinner_style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);

        let mut spinners: HashMap<String, ProgressBar> = HashMap::new();

        while !done.load(Ordering::Acquire) {
            if let Ok(event) = rx.recv_timeout(Duration::from_millis(50)) {
                match event {
                    Event::BucketSyncProgress { name, state } => {
                        if quiet {
                            continue;
                        }

                        match state {
                            BucketState::Started => {
                                let pb = mp.add(ProgressBar::new_spinner());
                                pb.set_style(spinner_style.clone());
                                pb.set_message(name.to_string());
                                pb.enable_steady_tick(Duration::from_millis(80));
                                spinners.insert(name, pb);
                            }
                            BucketState::Succeeded => {
                                if let Some(pb) = spinners.remove(&name) {
                                    pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
                                    pb.finish_with_message(format!("{} {}", "✓".green(), name));
                                }
                            }
                            BucketState::Failed(msg) => {
                                if let Some(pb) = spinners.remove(&name) {
                                    pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
                                    pb.finish_with_message(format!(
                                        "{} {} ({})",
                                        "✗".red(),
                                        name,
                                        msg
                                    ));
                                }
                            }
                        }
                    }
                    Event::BucketSyncDone => break,
                    _ => {}
                }

                while let Ok(event) = rx.try_recv() {
                    if let Event::BucketSyncProgress { name, state } = event {
                        if quiet {
                            continue;
                        }

                        match state {
                            BucketState::Started => {
                                let pb = mp.add(ProgressBar::new_spinner());
                                pb.set_style(spinner_style.clone());
                                pb.set_message(name.to_string());
                                pb.enable_steady_tick(Duration::from_millis(80));
                                spinners.insert(name, pb);
                            }
                            BucketState::Succeeded => {
                                if let Some(pb) = spinners.remove(&name) {
                                    pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
                                    pb.finish_with_message(format!("{} {}", "✓".green(), name));
                                }
                            }
                            BucketState::Failed(msg) => {
                                if let Some(pb) = spinners.remove(&name) {
                                    pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
                                    pb.finish_with_message(format!(
                                        "{}  {} ({})",
                                        "✗".red(),
                                        name,
                                        msg
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        for (_, pb) in spinners.drain() {
            pb.finish_and_clear();
        }
    })
}

/// Update installed packages to latest versions
pub async fn execute(args: Args, session: &Session) -> Result<()> {
    let installed = query::query_installed(session)?;
    let has_wildcard = args.names.iter().any(|n| n == "*");
    let effective_names: Vec<String> = if has_wildcard {
        installed.iter().map(|p| p.name().to_owned()).collect()
    } else {
        args.names.clone()
    };

    if effective_names.is_empty() && !has_wildcard {
        return bucket_only(session, args.quiet).await;
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
            let s = download::calculate_total_download_size(
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
        println!("No packages were updated.");
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

/// Bucket-sync only mode: update all buckets with spinner UI.
async fn bucket_only(session: &Session, quiet: bool) -> Result<()> {
    let all_buckets = rake_core::operations::bucket::bucket_list(session)?;
    let buckets_to_update: Vec<_> = all_buckets.into_iter().filter(|b| !b.is_held()).collect();
    let count = buckets_to_update.len();

    if count == 0 {
        println!("No buckets to update.");
        return Ok(());
    }

    let done = Arc::new(AtomicBool::new(false));
    let _guard = DoneFlag::new(done.clone());
    let ui_handle = spawn_bucket_ui(session, quiet, done);

    if !quiet {
        if count == 1 {
            println!("Updating bucket");
        } else {
            println!("Updating buckets");
        }
    }

    update::bucket_update(session).await?;

    drop(_guard);
    let _ = ui_handle.join();

    if !quiet {
        println!("Everything is up to date!");
    }

    Ok(())
}
