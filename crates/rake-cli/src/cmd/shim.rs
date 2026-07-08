use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use crossterm::style::{Stylize, style};
use rake_core::infra::shim::{self, ShimType};
use rake_core::session::Session;

/// Manipulate Scoop shims
#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List all shims
    #[clap(alias = "ls")]
    List {
        /// Filter shims by regex pattern
        pattern: Option<String>,
    },
    /// Show detailed info about a specific shim
    Info {
        /// Shim name
        name: String,
    },
    /// Remove shim(s)
    #[clap(alias = "rm")]
    Remove {
        /// Shim name(s) to remove
        names: Vec<String>,
    },
}

#[derive(Debug)]
struct ShimInfo {
    name: String,
    source: Option<String>,
    shim_type: ShimType,
    alternatives: Vec<String>,
}

pub fn execute(args: Args, session: &Session) -> Result<()> {
    let command = args.command.unwrap_or(Command::List { pattern: None });
    let root = session
        .config()
        .root_path
        .as_ref()
        .ok_or_else(|| anyhow!("root_path not configured"))?;
    let shims_dir = root.join("shims");

    match command {
        Command::List { pattern } => cmd_list(&shims_dir, pattern.as_deref()),
        Command::Info { name } => cmd_info(&shims_dir, &name),
        Command::Remove { names } => cmd_remove(&shims_dir, &names),
    }
}

fn cmd_list(shims_dir: &Path, pattern: Option<&str>) -> Result<()> {
    let all = scan_shims(shims_dir)?;

    let filtered: Vec<_> = match pattern {
        Some(pat) => all.into_iter().filter(|s| s.name.contains(pat)).collect(),
        None => all,
    };

    if filtered.is_empty() {
        println!("No shims found.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(NOTHING);

    let header = ["Name", "Source", "Type", "Alternatives"]
        .into_iter()
        .map(|title| {
            Cell::new(title)
                .add_attribute(Attribute::Bold)
                .fg(Color::Green)
        });
    table.set_header(header);

    for s in &filtered {
        let src = s.source.as_deref().unwrap_or("-");
        let alt = if s.alternatives.is_empty() {
            "-".to_string()
        } else {
            s.alternatives.join(", ")
        };
        table.add_row(vec![
            Cell::new(&s.name),
            Cell::new(src).add_attribute(Attribute::Dim),
            Cell::new(type_name(s.shim_type)),
            Cell::new(alt).add_attribute(Attribute::Dim),
        ]);
    }

    println!("{table}");
    Ok(())
}

fn cmd_info(shims_dir: &Path, name: &str) -> Result<()> {
    let shim_path = shims_dir.join(format!("{}.shim", name));
    if !shim_path.exists() {
        eprintln!("Shim '{}' not found.", name);
        return Ok(());
    }

    let target = read_shim_target(&shim_path)?;
    let source = extract_source(&target);
    let alternatives = find_alternatives(shims_dir, name);
    let shim_type = determine_shim_type(&target);

    println!("Name:         {}", name);
    println!("Target:       {}", target.display());
    if let Some(src) = source {
        println!("Source:       {}", src);
    }
    println!("Type:         {}", type_name(shim_type));
    println!(
        "Alternatives: {}",
        if alternatives.is_empty() {
            "(none)".to_string()
        } else {
            alternatives.join(", ")
        }
    );

    Ok(())
}

fn cmd_remove(shims_dir: &Path, names: &[String]) -> Result<()> {
    for name in names {
        if !shims_dir.join(format!("{}.shim", name)).exists() {
            eprintln!("Shim '{}' not found.", name);
            continue;
        }

        let alternatives = find_alternatives(shims_dir, name);

        shim::remove_shim(name, shims_dir)?;

        if !alternatives.is_empty() {
            let alt = &alternatives[0];
            let alt_base = format!("{}.{}", name, alt);
            for ext in &["exe", "shim", "cmd", "ps1"] {
                let src = shims_dir.join(format!("{}.{}", alt_base, ext));
                let dst = shims_dir.join(format!("{}.{}", name, ext));
                if src.exists() {
                    std::fs::rename(&src, &dst)?;
                }
            }
            // Unix compat no-extension shim
            let src = shims_dir.join(&alt_base);
            let dst = shims_dir.join(name);
            if src.exists() {
                std::fs::rename(&src, &dst)?;
            }

            println!(
                "{} Shim '{}' removed. Promoted alternative from '{}'.",
                style("✓").green(),
                name,
                alt
            );
        } else {
            println!("{} Shim '{}' removed.", style("✓").green(), name);
        }
    }

    Ok(())
}

fn scan_shims(shims_dir: &Path) -> Result<Vec<ShimInfo>> {
    if !shims_dir.exists() {
        return Ok(vec![]);
    }

    let mut shims = Vec::new();

    let entries: Vec<_> = std::fs::read_dir(shims_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|s| s == "shim").unwrap_or(false))
        .collect();

    for entry in &entries {
        let stem = entry
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        // Skip alternatives: name.app.shim → base before last dot has a primary
        if let Some(dot_pos) = stem.rfind('.') {
            let base = &stem[..dot_pos];
            let primary_path = shims_dir.join(format!("{}.shim", base));
            if primary_path.exists() {
                continue;
            }
        }

        let name = stem;
        let target = read_shim_target(&entry.path())?;
        let source = extract_source(&target);
        let alternatives = find_alternatives(shims_dir, &name);
        let shim_type = determine_shim_type(&target);

        shims.push(ShimInfo {
            name,
            source,
            shim_type,
            alternatives,
        });
    }

    shims.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(shims)
}

fn read_shim_target(path: &Path) -> Result<PathBuf> {
    let content = std::fs::read_to_string(path)?;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("path = ") {
            let trimmed = val.trim().trim_matches('"');
            return Ok(PathBuf::from(trimmed));
        }
    }
    Err(anyhow!(
        "No 'path = ' line found in shim file: {}",
        path.display()
    ))
}

fn find_alternatives(shims_dir: &Path, name: &str) -> Vec<String> {
    let prefix = format!("{}.", name);
    let entries = match std::fs::read_dir(shims_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let mut alts = Vec::new();
    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.starts_with(&prefix)
            && fname.ends_with(".shim")
            && fname != format!("{}.shim", name)
        {
            let inner = &fname[name.len() + 1..fname.len() - 5];
            alts.push(inner.to_string());
        }
    }
    alts.sort();
    alts
}

fn extract_source(target: &Path) -> Option<String> {
    for ancestor in target.ancestors() {
        if ancestor.file_name().and_then(|s| s.to_str()) == Some("current")
            && let Some(parent) = ancestor.parent()
        {
            return parent.file_name().map(|s| s.to_string_lossy().into_owned());
        }
    }
    None
}

fn determine_shim_type(target: &Path) -> ShimType {
    let ext = target
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());
    match ext.as_deref() {
        Some("exe" | "com") => ShimType::Exe,
        Some("bat" | "cmd") => ShimType::Batch,
        Some("ps1") => ShimType::PowerShell,
        Some("jar") => ShimType::Java,
        Some("py") => ShimType::Python,
        _ => ShimType::Bash,
    }
}

fn type_name(t: ShimType) -> &'static str {
    match t {
        ShimType::Exe => "Application",
        _ => "ExternalScript",
    }
}
