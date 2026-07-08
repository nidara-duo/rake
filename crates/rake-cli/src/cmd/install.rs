use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use crossterm::ExecutableCommand;
use crossterm::cursor;
use crossterm::style::{Stylize, style};
use rake_core::event::Event;
use rake_core::operations::{download, install};
use rake_core::session::Session;
use rake_domain::arch::Arch;

/// Install an app
#[derive(Debug, Parser)]
pub struct Args {
    /// Package(s) to install (name or name@version)
    pub names: Vec<String>,
    /// Architecture override (32bit, 64bit, arm64)
    #[arg(short, long)]
    pub arch: Option<String>,
    /// Skip confirmation prompt
    #[arg(short, long)]
    pub yes: bool,
}

fn parse_app_name(raw: &str) -> &str {
    raw.split('@').next().unwrap_or(raw)
}

fn get_arch(arch_str: Option<&str>) -> Arch {
    match arch_str {
        Some("32bit") => Arch::Ia32,
        Some("64bit") => Arch::Amd64,
        Some("arm64") => Arch::Aarch64,
        _ => Arch::current(),
    }
}

use crate::util::human_size;

pub async fn execute(args: Args, session: &Session) -> Result<()> {
    let mut stdout = std::io::stdout();
    let arch = get_arch(args.arch.as_deref());

    // 1. Resolve packages
    write!(stdout, "\rResolving packages...\r")?;
    stdout.flush()?;

    let names: Vec<&str> = args.names.iter().map(|n| parse_app_name(n)).collect();
    let packages = rake_core::operations::query::find_synced_by_names(session, &names)?;

    if packages.is_empty() {
        writeln!(stdout, "✗ No matching packages found")?;
        return Ok(());
    }

    writeln!(stdout, "✓ Resolved {} package(s)", packages.len())?;

    // 2. Calculate download size + cache info
    write!(stdout, "Calculating download size...\r")?;
    stdout.flush()?;

    let size = download::calculate_total_download_size(session, &packages, arch).await?;

    writeln!(stdout, "✓ Calculated download size")?;

    // 3. Confirmation with cache info
    if !args.yes {
        let size_str = human_size(size.total);
        let est = if size.estimated { " (estimated)" } else { "" };

        // Check what's cached
        let cache_root = session
            .config()
            .cache_path
            .as_ref()
            .cloned()
            .unwrap_or_else(|| std::path::PathBuf::from("cache"));

        let mut cached: u64 = 0;
        for pkg in &packages {
            let urls = download::resolve_urls(pkg, arch);
            for url in &urls {
                if let Some(cache_path) = download::find_cached_file(&cache_root, pkg, url)
                    && let Ok(meta) = cache_path.metadata()
                {
                    let fname = cache_path.file_name().unwrap_or_default();
                    let _ = writeln!(
                        stdout,
                        "  {} {} ({} cached)",
                        style("✓").green(),
                        style(fname.to_string_lossy()).dim(),
                        human_size(meta.len()),
                    );
                    cached += meta.len();
                }
            }
        }

        let from_cache = if cached > 0 {
            format!(" ({} cached)", human_size(cached))
        } else {
            String::new()
        };
        write!(
            stdout,
            "Total download size: {size_str}{est}{from_cache}. Continue? [y/N]: "
        )?;
        stdout.flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            writeln!(stdout, "Aborted.")?;
            return Ok(());
        }
    }

    // 4. Scoop-style header for each package
    let _ = stdout.execute(cursor::Hide);
    for pkg in &packages {
        writeln!(
            stdout,
            "Installing '{}' ({}) [{}]",
            style(pkg.name()).bold(),
            pkg.version(),
            arch,
        )?;
    }

    // 5. Download with inline progress
    let files = {
        let rx = session.event_bus().core_receiver();
        let done = Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        struct PkgState {
            ident: String,
            total: u64,
            downloaded: u64,
            prev_bytes: u64,
            prev_time: Instant,
        }

        let ui_handle = tokio::spawn(async move {
            let mut out = std::io::stdout();
            let mut state: Option<PkgState> = None;

            while !done_clone.load(Ordering::Relaxed) {
                while let Ok(event) = rx.try_recv() {
                    match event {
                        Event::DownloadProgress(ev) => {
                            let ident = ev.ident.to_string();

                            if state.as_ref().is_none_or(|s| s.ident != ident) {
                                if let Some(ref s) = state
                                    && s.downloaded >= s.total
                                    && s.total > 0
                                {
                                    let total_s = human_size(s.total);
                                    let _ = writeln!(
                                        out,
                                        "\r {} {} ({})        ",
                                        style("✓").green(),
                                        style(&s.ident).green(),
                                        total_s,
                                    );
                                }
                                state = Some(PkgState {
                                    ident: ident.clone(),
                                    total: ev.total_bytes,
                                    downloaded: ev.downloaded_bytes,
                                    prev_bytes: ev.downloaded_bytes,
                                    prev_time: Instant::now(),
                                });
                            } else if let Some(ref mut s) = state {
                                s.total = ev.total_bytes.max(s.total);
                                s.downloaded = ev.downloaded_bytes.max(s.downloaded);
                            }

                            let pct = if ev.total_bytes > 0 {
                                ev.downloaded_bytes as f64 / ev.total_bytes as f64 * 100.0
                            } else {
                                0.0
                            };
                            let bar_width = 20usize;
                            let filled = ((pct / 100.0) * bar_width as f64) as usize;
                            let bar = if pct >= 100.0 {
                                "=".repeat(bar_width)
                            } else if filled > 0 {
                                format!("{}{}", "=".repeat(filled - 1), ">")
                            } else {
                                String::new()
                            };
                            let empty = bar_width.saturating_sub(filled);
                            let _ = write!(
                                out,
                                "\r {} [{}{}] {:>3}%",
                                ev.filename,
                                bar,
                                " ".repeat(empty),
                                pct as u64,
                            );
                            let _ = out.flush();

                            if let Some(ref mut s) = state {
                                s.prev_bytes = s.downloaded;
                                s.prev_time = Instant::now();
                            }
                        }
                        Event::DownloadCached(_ident) => {}
                        Event::DownloadStart(_ident) => {}
                        Event::DownloadError(ident, msg) => {
                            if let Some(ref s) = state
                                && s.downloaded > 0
                            {
                                let total_s = human_size(s.total);
                                let _ = writeln!(
                                    out,
                                    "\r {} {} ({})        ",
                                    style("✓").green(),
                                    style(&s.ident).green(),
                                    total_s,
                                );
                            }
                            let _ = writeln!(
                                out,
                                " {} {} ({})        ",
                                style("✗").red(),
                                style(ident).red(),
                                msg
                            );
                            state = None;
                        }
                        Event::DownloadDone => break,
                        Event::CommitProgress(msg) => {
                            let _ = writeln!(out, " {}", msg);
                        }
                        _ => {}
                    }
                    let _ = out.flush();
                }
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }

            if let Some(ref s) = state
                && s.downloaded > 0
            {
                let total_s = human_size(s.total);
                let _ = writeln!(
                    out,
                    "\r {} {} ({})        ",
                    style("✓").green(),
                    style(&s.ident).green(),
                    total_s,
                );
            }
            let _ = out.flush();

            while let Ok(event) = rx.try_recv() {
                if let Event::CommitProgress(msg) = event {
                    let _ = writeln!(out, " {}", msg);
                }
            }
            let _ = out.flush();
        });

        let files = download::download_packages(session, &packages, true, arch).await;

        done.store(true, Ordering::Relaxed);
        let _ = ui_handle.await;
        let _ = stdout.execute(cursor::Show);

        match files {
            Ok(f) => f,
            Err(e) => {
                writeln!(stdout, "Download failed: {e}")?;
                return Err(e.into());
            }
        }
    };

    // 6. Commit (extract, link, shim, persist)
    let commit_result = {
        let _ = stdout.execute(cursor::Hide);
        let rx = session.event_bus().core_receiver();
        let done = Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        let commit_handle = tokio::spawn(async move {
            let mut out = std::io::stdout();
            while !done_clone.load(Ordering::Relaxed) {
                while let Ok(event) = rx.try_recv() {
                    match event {
                        Event::CommitStart(ident, version) => {
                            let _ = writeln!(
                                out,
                                "Linking {}/{} ({})",
                                style(&ident.bucket).bold(),
                                style(&ident.name).bold(),
                                version,
                            );
                        }
                        Event::CommitProgress(msg) => {
                            let _ = writeln!(out, " {}", msg);
                        }
                        Event::CommitDone(ident) => {
                            let _ = writeln!(
                                out,
                                " {} {} linked",
                                style("✓").green(),
                                style(ident).green(),
                            );
                        }
                        _ => {}
                    }
                    let _ = out.flush();
                }
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
            while let Ok(event) = rx.try_recv() {
                if let Event::CommitProgress(msg) = event {
                    let _ = writeln!(out, " {}", msg);
                }
                let _ = out.flush();
            }
        });

        let result = install::install_packages(session, &packages, &files, arch).await;
        done.store(true, Ordering::Relaxed);
        let _ = commit_handle.await;
        let _ = stdout.execute(cursor::Show);
        result
    };

    match commit_result {
        Ok(installed) => {
            for pkg in &installed {
                writeln!(
                    stdout,
                    " {} '{}' ({}) was installed successfully!",
                    style("✓").green(),
                    style(pkg.name()).green(),
                    pkg.version(),
                )?;
            }
        }
        Err(e) => {
            writeln!(stdout, "Installation failed: {e}")?;
            return Err(e.into());
        }
    }

    Ok(())
}
