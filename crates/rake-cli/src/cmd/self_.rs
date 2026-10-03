use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use rake_core::infra::self_replace;
use rake_core::session::Session;

/// Manage Rake itself (install, update, uninstall)
#[derive(Debug, Parser)]
#[command(disable_help_subcommand = true)]
pub struct Args {
    #[command(subcommand)]
    pub action: SelfAction,
}

#[derive(Debug, Parser)]
pub enum SelfAction {
    /// Install Rake (delegates to bootstrap.ps1)
    Install {
        /// Install from a local rake.exe instead of downloading a release
        #[arg(long, value_name = "PATH")]
        local: Option<PathBuf>,
    },
    /// Update Rake to the latest version
    Update {
        /// Update from a local rake.exe instead of downloading a release
        #[arg(long, value_name = "PATH")]
        local: Option<PathBuf>,
    },
    /// Uninstall Rake
    Uninstall,
}

/// Locate `bootstrap.ps1`.
///
/// Installed builds ship the script next to `rake.exe`. Development builds live in
/// `target/<profile>/`, so walk up looking for the repository's `scripts/` directory.
fn bootstrap_script_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("Cannot determine executable path")?;
    let dir = exe.parent().context("Executable has no parent directory")?;

    let mut candidates = vec![dir.join("bootstrap.ps1")];
    for up in 2..=4 {
        if let Some(root) = dir.ancestors().nth(up) {
            candidates.push(root.join("scripts").join("bootstrap.ps1"));
        }
    }

    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }

    let searched = candidates
        .iter()
        .map(|p| format!("  - {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n");

    anyhow::bail!(
        "bootstrap.ps1 not found. Searched:\n{searched}\n\
         Download the latest release from https://github.com/nidara-duo/rake/releases"
    );
}

/// Marker the bootstrap script prints when it cannot finish removing a locked binary
/// itself. Fields are separated by `|`: target, optional prune directory, and an
/// executable the helper may run from (empty when Rust should pick one).
const DEFER_MARKER: &str = "RAKE_DEFER_DELETE";

/// Run bootstrap.ps1, then take responsibility for any deferred cleanup it reported.
///
/// The script deliberately does not spawn its own helper: it has no cheap way to do so
/// without paying for a second PowerShell process. Instead it prints what is left over,
/// and the cleanup runs here in a small Rake process that exits as soon as this one does.
fn invoke_bootstrap(action: &str, local: Option<&std::path::Path>) -> Result<()> {
    let script = bootstrap_script_path()?;
    let mut args: Vec<String> = vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "RemoteSigned".into(),
        "-File".into(),
        script.to_string_lossy().into_owned(),
        action.into(),
    ];
    if let Some(path) = local {
        args.push("-Source".into());
        args.push(path.to_string_lossy().into_owned());
    }

    let output = std::process::Command::new("powershell")
        .args(&args)
        .output()
        .context("Failed to execute bootstrap.ps1")?;

    // Forward the script's own output so progress and errors still reach the user.
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        anyhow::bail!(
            "bootstrap script failed (exit code: {:?})",
            output.status.code()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(line) = stdout.lines().find(|l| l.starts_with(DEFER_MARKER)) {
        schedule_cleanup(line)?;
    }

    Ok(())
}

/// A `RAKE_DEFER_DELETE` report, split into its parts.
#[derive(Debug, PartialEq, Eq)]
struct DeferReport {
    /// File that could not be removed and must be deleted once the owner exits.
    target: PathBuf,
    /// Directory to drop if removing the target empties it.
    prune_dir: Option<PathBuf>,
    /// An executable the helper may run from, when the script knows of a usable one.
    helper_exe: Option<PathBuf>,
}

/// Split a `RAKE_DEFER_DELETE` report into its fields.
///
/// Fields are separated by `|`, and any of the trailing ones may be left empty.
fn parse_defer_marker(line: &str) -> Result<DeferReport> {
    let payload = line[DEFER_MARKER.len()..].trim_start_matches('|');
    let mut parts = payload.split('|');

    let field = |s: &str| {
        let s = s.trim();
        (!s.is_empty()).then(|| PathBuf::from(s))
    };

    let target = field(parts.next().unwrap_or_default())
        .context("bootstrap reported deferred cleanup without a target")?;

    Ok(DeferReport {
        target,
        prune_dir: parts.next().and_then(field),
        helper_exe: parts.next().and_then(field),
    })
}

/// Schedule the removal described by a `RAKE_DEFER_DELETE` report.
fn schedule_cleanup(line: &str) -> Result<()> {
    let DeferReport {
        target,
        prune_dir,
        helper_exe,
    } = parse_defer_marker(line)?;

    let helper = match helper_exe {
        Some(p) => p,
        // No usable alternative was named, so fall back to our own binary. It may be
        // the locked file itself, in which case it gets copied aside before running.
        None => std::env::current_exe().context("Cannot determine executable path")?,
    };

    let request =
        self_replace::schedule_delete(&target, prune_dir.as_deref(), std::process::id(), &helper)?;

    if request.self_delete {
        println!(
            "  {} will be deleted as soon as this command exits.",
            request.target.display()
        );
    } else {
        println!(
            "  Scheduled cleanup of {} after this command exits.",
            request.target.display()
        );
    }
    Ok(())
}

pub fn execute(args: Args, _session: &Session) -> Result<()> {
    match args.action {
        SelfAction::Install { local } => invoke_bootstrap("install", local.as_deref()),
        SelfAction::Update { local } => invoke_bootstrap("update", local.as_deref()),
        SelfAction::Uninstall => invoke_bootstrap("uninstall", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> DeferReport {
        parse_defer_marker(line).expect("marker should parse")
    }

    #[test]
    fn parses_update_report_with_prune_and_helper() {
        let r = parse(r"RAKE_DEFER_DELETE|C:\rake\bin\rake.exe.old||C:\rake\bin\rake.exe");
        assert_eq!(r.target, PathBuf::from(r"C:\rake\bin\rake.exe.old"));
        assert_eq!(r.prune_dir, None);
        assert_eq!(r.helper_exe, Some(PathBuf::from(r"C:\rake\bin\rake.exe")));
    }

    #[test]
    fn parses_uninstall_report_with_prune_and_no_helper() {
        let r = parse(r"RAKE_DEFER_DELETE|C:\rake\bin\rake.exe.removing|C:\rake\bin|");
        assert_eq!(r.target, PathBuf::from(r"C:\rake\bin\rake.exe.removing"));
        assert_eq!(r.prune_dir, Some(PathBuf::from(r"C:\rake\bin")));
        assert_eq!(r.helper_exe, None);
    }

    /// The separator after the marker is not a field: treating it as one yields an
    /// empty target and silently skips cleanup.
    #[test]
    fn leading_separator_is_not_a_field() {
        let r = parse(r"RAKE_DEFER_DELETE|C:\rake\bin\rake.exe.old");
        assert_eq!(r.target, PathBuf::from(r"C:\rake\bin\rake.exe.old"));
    }

    #[test]
    fn trailing_fields_may_be_absent() {
        let r = parse(r"RAKE_DEFER_DELETE|C:\rake\bin\rake.exe.old");
        assert_eq!(r.prune_dir, None);
        assert_eq!(r.helper_exe, None);
    }

    #[test]
    fn rejects_report_without_a_target() {
        assert!(parse_defer_marker("RAKE_DEFER_DELETE|||").is_err());
        assert!(parse_defer_marker("RAKE_DEFER_DELETE").is_err());
    }
}
