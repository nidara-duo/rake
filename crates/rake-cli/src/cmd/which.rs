use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use rake_core::session::Session;

/// Locate a shim/executable (similar to 'which' on Linux)
#[derive(Debug, Parser)]
#[clap(arg_required_else_help = true)]
pub struct Args {
    /// Command to locate
    pub command: String,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let command = &args.command;

    // 1. Check shims directory
    if let Some(path) = find_in_shims(session, command) {
        println!("{}", path.display());
        return Ok(());
    }

    // 2. Check PATH via where.exe
    let output = std::process::Command::new("where.exe")
        .arg(command)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                println!("{}", line.trim());
            }
            Ok(())
        }
        _ => {
            eprintln!(
                "'{}' not found, not a scoop shim, or a broken shim.",
                command
            );
            std::process::exit(2);
        }
    }
}

fn find_in_shims(session: &Session, command: &str) -> Option<PathBuf> {
    let root = session.config().root_path.as_ref()?;
    let shims_dir = root.join("shims");

    if !shims_dir.exists() {
        return None;
    }

    // Look for <command>.exe (shim executable)
    let shim_exe = shims_dir.join(format!("{}.exe", command));
    let shim_file = shims_dir.join(format!("{}.shim", command));

    if shim_exe.exists() && shim_file.exists() {
        // Read .shim file to find real target
        let content = std::fs::read_to_string(&shim_file).ok()?;
        // Format: path = "C:\real\path\to\exe"
        let line = content.lines().next()?;
        let path = line.strip_prefix("path = ")?.trim_matches('"').trim();
        let target = PathBuf::from(path);
        if target.exists() {
            return Some(target);
        }
    }

    // Also check for .ps1 shims
    let shim_ps1 = shims_dir.join(format!("{}.ps1", command));
    if shim_ps1.exists() {
        // .ps1 shims have a comment with the real path: # C:\real\path
        if let Ok(content) = std::fs::read_to_string(&shim_ps1) {
            for line in content.lines() {
                if let Some(path) = line.strip_prefix("# ") {
                    let target = PathBuf::from(path.trim());
                    if target.exists() {
                        return Some(target);
                    }
                }
            }
        }
    }

    None
}
