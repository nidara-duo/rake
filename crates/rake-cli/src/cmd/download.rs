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
use rake_core::operations::download;
use rake_core::session::Session;
use rake_domain::arch::Arch;

/// Download packages to cache
#[derive(Debug, Parser)]
pub struct Args {
    /// Package(s) to download (name or name@version)
    pub names: Vec<String>,
    /// Force download (overwrite cache)
    #[arg(short, long)]
    pub force: bool,
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

struct PkgState {
    ident: String,
    total: u64,
    downloaded: u64,
    start: Instant,
    prev_bytes: u64,
    prev_time: Instant,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
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

    // 2. Calculate download size
    write!(stdout, "Calculating download size...\r")?;
    stdout.flush()?;

    let reuse_cache = !args.force;
    let size = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(download::calculate_total_download_size(
            session, &packages, arch,
        ))
    })?;

    writeln!(stdout, "✓ Calculated download size")?;

    // 3. Confirmation
    if !args.yes {
        let size_str = human_size(size.total);
        let est = if size.estimated { " (estimated)" } else { "" };
        write!(
            stdout,
            "Total download size: {size_str}{est}. Continue? [y/N]: "
        )?;
        stdout.flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            writeln!(stdout, "Aborted.")?;
            return Ok(());
        }
    }

    // 4. Download with inline progress per package
    let _ = stdout.execute(cursor::Hide);

    // Print scoop-style header for each package
    for pkg in &packages {
        let bucket_name = pkg.bucket();
        let version = pkg.version();
        writeln!(
            stdout,
            "Downloading '{}' ({}) [{}] from {} bucket",
            style(pkg.name()).bold(),
            version,
            arch,
            bucket_name,
        )?;
    }

    let rx = session.event_bus().core_receiver();
    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();
    let ui_handle = std::thread::spawn(move || {
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
                                write_done_line(&mut out, s);
                            }
                            state = Some(PkgState {
                                ident: ident.clone(),
                                total: ev.total_bytes,
                                downloaded: ev.downloaded_bytes,
                                start: Instant::now(),
                                prev_bytes: ev.downloaded_bytes,
                                prev_time: Instant::now(),
                            });
                        } else if let Some(ref mut s) = state {
                            s.total = ev.total_bytes.max(s.total);
                            s.downloaded = ev.downloaded_bytes.max(s.downloaded);
                        }

                        draw_progress(
                            &mut out,
                            &ident,
                            ev.total_bytes,
                            ev.downloaded_bytes,
                            state.as_ref(),
                        );
                        if let Some(ref mut s) = state {
                            s.prev_bytes = s.downloaded;
                            s.prev_time = Instant::now();
                        }
                    }
                    Event::DownloadCached(ident) => {
                        let _ = writeln!(
                            out,
                            " {} {} (cached)",
                            style("✓").green(),
                            style(ident).green()
                        );
                        state = None;
                    }
                    Event::DownloadStart(_ident) => {
                        // new package starting, print previous done line if any
                        if let Some(ref s) = state
                            && s.downloaded > 0
                        {
                            write_done_line(&mut out, s);
                        }
                        state = None;
                    }
                    Event::DownloadError(ident, msg) => {
                        if let Some(ref s) = state
                            && s.downloaded > 0
                        {
                            write_done_line(&mut out, s);
                        }
                        let _ = writeln!(
                            out,
                            " {} {} ({})",
                            style("✗").red(),
                            style(ident).red(),
                            msg
                        );
                        state = None;
                    }
                    Event::DownloadDone => break,
                    _ => {}
                }
                let _ = out.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
            let _ = out.flush();
        }

        // Drain remaining events
        while let Ok(event) = rx.try_recv() {
            if let Event::DownloadProgress(ev) = event {
                let ident = ev.ident.to_string();
                if state.as_ref().is_none_or(|s| s.ident != ident) {
                    if let Some(ref s) = state
                        && s.downloaded >= s.total
                        && s.total > 0
                    {
                        write_done_line(&mut out, s);
                    }
                    state = Some(PkgState {
                        ident: ident.clone(),
                        total: ev.total_bytes,
                        downloaded: ev.downloaded_bytes,
                        start: Instant::now(),
                        prev_bytes: ev.downloaded_bytes,
                        prev_time: Instant::now(),
                    });
                } else if let Some(ref mut s) = state {
                    s.total = ev.total_bytes.max(s.total);
                    s.downloaded = ev.downloaded_bytes.max(s.downloaded);
                }
                draw_progress(
                    &mut out,
                    &ident,
                    ev.total_bytes,
                    ev.downloaded_bytes,
                    state.as_ref(),
                );
                if let Some(ref mut s) = state {
                    s.prev_bytes = s.downloaded;
                    s.prev_time = Instant::now();
                }
            } else if let Event::DownloadError(ident, msg) = &event {
                if let Some(ref s) = state
                    && s.downloaded > 0
                {
                    write_done_line(&mut out, s);
                }
                let _ = writeln!(
                    out,
                    " {} {} ({})",
                    style("✗").red(),
                    style(ident).red(),
                    msg
                );
                state = None;
            }
        }

        if let Some(ref s) = state
            && s.downloaded > 0
        {
            write_done_line(&mut out, s);
        }
    });

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(download::download_packages(
            session,
            &packages,
            reuse_cache,
            arch,
        ))
    });

    done.store(true, Ordering::Relaxed);
    let _ = ui_handle.join();
    let _ = std::io::stdout().execute(cursor::Show);

    if let Err(e) = result {
        writeln!(std::io::stdout(), "Download failed: {e}")?;
        return Err(e.into());
    }

    // Scoop-style success for each package
    for pkg in &packages {
        writeln!(
            std::io::stdout(),
            " {} '{}' ({}) was downloaded successfully!",
            style("✓").green(),
            style(pkg.name()).green(),
            pkg.version(),
        )?;
    }

    Ok(())
}

fn write_done_line(out: &mut impl Write, s: &PkgState) {
    let total_s = human_size(s.total);
    let elapsed = s.start.elapsed();
    let avg_speed = if elapsed.as_secs_f64() > 0.0 {
        s.total as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    let speed_s = if avg_speed > 0.0 && elapsed.as_secs_f64() >= 0.2 {
        format!(" @ {}/s", human_size(avg_speed as u64))
    } else {
        String::new()
    };
    let time_s = if elapsed.as_secs_f64() >= 0.5 {
        format!(" in {:.1}s", elapsed.as_secs_f64())
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "\r{} {} ({}{speed_s}{time_s})",
        style("✓").green(),
        style(&s.ident).green(),
        total_s,
    );
}

fn draw_progress(
    out: &mut impl Write,
    ident: &str,
    total_bytes: u64,
    downloaded_bytes: u64,
    state: Option<&PkgState>,
) {
    let pct = if total_bytes > 0 {
        downloaded_bytes as f64 / total_bytes as f64 * 100.0
    } else {
        0.0
    };
    let pct_int = pct as u64;

    let bar_width = 20usize;
    let filled = ((pct / 100.0) * bar_width as f64) as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar = if pct_int >= 100 {
        "=".repeat(bar_width)
    } else if filled > 0 {
        format!("{}{}", "=".repeat(filled - 1), ">")
    } else {
        String::new()
    };

    let total_s = human_size(total_bytes);

    let speed_eta = state
        .and_then(|s| {
            let elapsed = s.prev_time.elapsed();
            if elapsed.as_secs_f64() < 0.2 {
                return None;
            }
            let delta = s.downloaded.saturating_sub(s.prev_bytes);
            let speed = delta as f64 / elapsed.as_secs_f64();
            if speed <= 0.0 {
                return None;
            }
            let remaining = total_bytes.saturating_sub(downloaded_bytes);
            let eta_secs = remaining as f64 / speed;
            let speed_h = human_size(speed as u64);
            if eta_secs.is_finite() && eta_secs >= 1.0 {
                if eta_secs >= 3600.0 {
                    Some(format!(
                        " @ {}/s ETA {:.0}h{:.0}m",
                        speed_h,
                        eta_secs / 3600.0,
                        (eta_secs % 3600.0) / 60.0
                    ))
                } else if eta_secs >= 60.0 {
                    Some(format!(
                        " @ {}/s ETA {:.0}m{:.0}s",
                        speed_h,
                        eta_secs / 60.0,
                        eta_secs % 60.0
                    ))
                } else {
                    Some(format!(" @ {}/s ETA {:.0}s", speed_h, eta_secs))
                }
            } else {
                Some(format!(" @ {}/s", speed_h))
            }
        })
        .unwrap_or_default();

    let line = format!(
        " {} {} [{}{}] {:>3}%{}",
        ident,
        total_s,
        bar,
        " ".repeat(empty),
        pct_int,
        speed_eta,
    );

    let _ = write!(out, "\r{:<width$}\r{}", "", line, width = line.len() + 4);
}

use crate::util::human_size;
