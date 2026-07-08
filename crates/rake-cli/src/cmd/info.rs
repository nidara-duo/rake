use anyhow::Result;
use clap::Parser;
use crossterm::style::Stylize;
use rake_core::operations::query;
use rake_core::session::Session;
use rake_domain::package::PackageStatus;

/// Show package(s) basic information
#[derive(Debug, Parser)]
#[clap(arg_required_else_help = true)]
pub struct Args {
    /// Package name
    query: String,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let query = args.query.to_ascii_lowercase();
    let packages = rake_core::operations::query::find_all_synced_by_name(session, &query)?;
    let installed = query::query_installed(session)?;

    if packages.is_empty() {
        eprintln!("Could not find package for query '{}'.", args.query);
        return Ok(());
    }

    for pkg in &packages {
        let state = installed.iter().find(|p| {
            p.name().to_ascii_lowercase() == query
                && matches!(p.status, PackageStatus::Installed(_))
        });

        let version_display = match state {
            Some(state) if let PackageStatus::Installed(s) = &state.status => {
                if s.version == pkg.version() {
                    s.version.clone()
                } else {
                    format!("{} (Update to {} available)", s.version, pkg.version())
                }
            }
            _ => pkg.version().to_owned(),
        };

        print_field("Name", pkg.name());
        if let Some(ref desc) = pkg.manifest.description {
            print_field("Description", desc);
        }
        print_field("Version", &version_display);
        print_field("Source", pkg.bucket());
        if let Some(ref hp) = pkg.manifest.homepage {
            print_field("Website", hp);
        }
        if let Some(ref lic) = pkg.manifest.license {
            print_field("License", lic.identifier());
        }
        if let Some(ref deps) = pkg.manifest.depends {
            print_field("Dependencies", &deps.as_slice().join(", "));
        }
        if let Some(ref bins) = pkg.manifest.bin {
            let shims: Vec<&str> = bins
                .as_slice()
                .iter()
                .map(|b| match b.as_slice() {
                    [first, ..] => first.as_str(),
                    [] => "",
                })
                .collect();
            if !shims.is_empty() {
                print_field("Shims", &shims.join(", "));
            }
        }
        if let Some(ref notes) = pkg.manifest.notes {
            for note in notes.iter() {
                print_field("Notes", note);
            }
        }
    }

    Ok(())
}

fn print_field(key: &str, value: &str) {
    let styled = key.bold();
    let pad = 15usize.saturating_sub(key.len());
    let padding = " ".repeat(pad);
    println!("{}{} {}", styled, padding, value);
}
