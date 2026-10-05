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

    // Scoop substitutes $dir / $original_dir / $persist_dir into notes before
    // displaying them (libexec/scoop-info.ps1, via lib/core.ps1 `substitute`).
    // Without this, notes that point at those paths print the raw placeholder.
    let substitutions = note_substitutions(session, &query);

    for pkg in &packages {
        // Match on this package's own bucket: an app present in two buckets
        // must not report the other bucket's installed state.
        let state = installed.iter().find(|p| {
            p.name().eq_ignore_ascii_case(query.as_str())
                && p.bucket().eq_ignore_ascii_case(pkg.bucket())
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
        // Scoop joins all note lines into a single Notes field
        // (libexec/scoop-info.ps1: `-join "`n"`). Printing one labelled row per
        // entry duplicated the "Notes" label for every line.
        if let Some(ref notes) = pkg.manifest.notes {
            let joined = substitute(&notes.as_slice().join("\n"), &substitutions);
            if !joined.is_empty() {
                print_field("Notes", &joined);
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

/// Build the `$dir` / `$original_dir` / `$persist_dir` substitution table for
/// an installed app. Empty strings are used for apps that are not installed,
/// which matches Scoop: the placeholders simply disappear.
fn note_substitutions(session: &Session, query: &str) -> Vec<(&'static str, String)> {
    let Some(root) = session.config().root_path.as_ref() else {
        return Vec::new();
    };

    let app_dir = root.join("apps").join(query);
    let persist_dir = root.join("persist").join(query);

    // `$dir` is the version directory that `current` points at, which is what
    // notes are actually written against at install time.
    let version_dir = std::fs::read_link(app_dir.join("current"))
        .map(|target| target.to_string_lossy().to_string())
        .unwrap_or_else(|_| app_dir.join("current").to_string_lossy().to_string());

    vec![
        ("$original_dir", version_dir.clone()),
        ("$dir", version_dir),
        ("$persist_dir", persist_dir.to_string_lossy().to_string()),
    ]
}

/// Replace `$`-placeholders, longest first so `$original_dir` is not clobbered
/// by a `$dir` match. Mirrors `substitute` in ethalon lib/core.ps1.
fn substitute(text: &str, substitutions: &[(&str, String)]) -> String {
    let mut out = text.to_owned();
    for (key, value) in substitutions {
        if out.contains(key) {
            out = out.replace(key, value);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::substitute;

    fn subs() -> Vec<(&'static str, String)> {
        vec![
            ("$original_dir", r"C:\scoop\apps\demo\1.0".to_owned()),
            ("$dir", r"C:\scoop\apps\demo\1.0".to_owned()),
            ("$persist_dir", r"C:\scoop\persist\demo".to_owned()),
        ]
    }

    #[test]
    fn notes_render_as_one_block_with_single_label() {
        let joined = ["first line", "second line", "third line"].join("\n");
        let out = substitute(&joined, &subs());
        assert_eq!(out.lines().count(), 3, "all note lines are preserved");
    }

    #[test]
    fn substitutes_persist_dir() {
        let out = substitute("Data lives in $persist_dir/data", &subs());
        assert_eq!(out, r"Data lives in C:\scoop\persist\demo/data");
    }

    #[test]
    fn longest_placeholder_wins_for_original_dir() {
        let out = substitute("$original_dir/config", &subs());
        assert_eq!(out, r"C:\scoop\apps\demo\1.0/config");
    }

    #[test]
    fn leaves_text_without_placeholders_untouched() {
        assert_eq!(substitute("plain note", &subs()), "plain note");
    }
}
