use crossterm::style::{Stylize, style};
use rake_core::operations::download::DownloadedFile;
use rake_core::session::Session;

/// Whether at least one bucket has been added.
///
/// Commands that search manifests must tell "you have no buckets yet" apart from "the
/// package you asked for does not exist". On a fresh install the first is the real
/// blocker, while the second sends people hunting for a typo that isn't there.
pub fn has_buckets(session: &Session) -> bool {
    matches!(rake_core::bucket::added_buckets(session), Ok(b) if !b.is_empty())
}

/// Report the result of the download-only stub used by `install` and `update`.
///
/// Both commands share this so their wording cannot drift: the artifacts are
/// really downloaded and hash-verified, and the mutating half is deliberately
/// skipped until the installer rewrite lands. The verified cache is usable by
/// Scoop itself in the meantime.
pub fn report_install_stub(action: &str, app: &str, files: &[DownloadedFile]) {
    println!(
        " {} '{}' downloaded to the cache:",
        style("✓").green(),
        style(app).green(),
    );

    for file in files {
        let size = file
            .cache_path
            .metadata()
            .map(|m| human_size(m.len()))
            .unwrap_or_else(|_| "?".to_owned());
        let hash = if file.verified {
            style("[hash verified]").green().to_string()
        } else {
            style("[no hash in manifest]").yellow().to_string()
        };
        println!(
            "   {} ({size}) → {} {hash}",
            style(file.cache_path.display()).dim(),
            style(&file.target_filename).bold(),
        );
    }

    println!(
        " {} {action} is temporarily disabled while the installer is being rewritten;",
        style("!").yellow(),
    );
    println!("   the verified cache is ready — run `scoop {action} {app}` in the meantime.");
    println!(
        "   (maintainers: `rake {action} --unstable-install` runs the work-in-progress installer)"
    );
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}
