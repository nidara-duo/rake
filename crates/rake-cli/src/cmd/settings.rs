//! `rake settings` — get, set and reset user preferences.
//!
//! Modelled on Scoop's `scoop config [rm] name [value]` (libexec/scoop-config.ps1), which
//! is flat key/value with no interface beyond a dump, a get and a set. Explicit
//! subcommands are used instead of Scoop's positional form because `settings rm shim`
//! would otherwise be indistinguishable from `settings shim` with an empty value — Scoop
//! disambiguates by comparing the first argument to the string `rm`, which works and is
//! fragile.
//!
//! No TUI: the file holds a dozen booleans, a full-screen editor is disproportionate, and
//! a CLI has to keep working when its output is redirected. `settings edit` hands the file
//! to `$EDITOR` for anything the CLI cannot express.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, Color, Table};
use rake_core::session::Session;
use rake_core::settings;
use rake_domain::settings::Settings;

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    pub action: Option<Action>,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Show every setting, its value, and the default
    List,

    /// Print one setting
    Get {
        /// Setting name, e.g. status.offline_by_default
        name: String,
    },

    /// Change one setting
    Set {
        /// Setting name, e.g. status.offline_by_default
        name: String,
        /// New value: true or false
        value: String,
    },

    /// Restore one setting to its default
    Reset {
        /// Setting name, e.g. status.offline_by_default
        name: String,
    },

    /// Open the settings file in your editor
    Edit,
}

pub fn execute(args: Args, _session: &Session) -> Result<()> {
    match args.action {
        None | Some(Action::List) => list(),
        Some(Action::Get { name }) => get(&name),
        Some(Action::Set { name, value }) => set(&name, &value),
        Some(Action::Reset { name }) => reset(&name),
        Some(Action::Edit) => edit(),
    }
}

fn list() -> Result<()> {
    let current = settings::load()?;

    let mut table = Table::new();
    table.load_preset(NOTHING);
    table.set_header(vec![
        Cell::new("Setting")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Value")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("Default")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
        Cell::new("")
            .add_attribute(Attribute::Bold)
            .fg(Color::Green),
    ]);

    for (name, value, default, changed) in current.entries() {
        let value_cell = if changed {
            Cell::new(value).fg(Color::Yellow)
        } else {
            Cell::new(value).add_attribute(Attribute::Dim)
        };
        table.add_row(vec![
            Cell::new(name),
            value_cell,
            Cell::new(default).add_attribute(Attribute::Dim),
            Cell::new(if changed { "changed" } else { "" }),
        ]);
    }

    println!("{table}");

    let Some(path) = settings::settings_path() else {
        println!("No config home found, so settings cannot be saved.");
        return Ok(());
    };
    if !path.is_file() {
        println!("\nNo settings file yet; defaults are in use. It would be created at:");
        println!("  {}", path.display());
    } else {
        println!("\nFile: {}", path.display());
    }

    Ok(())
}

fn get(name: &str) -> Result<()> {
    let current = settings::load()?;
    match current.get(name) {
        Some(value) => {
            println!("{name} = {value}");
            Ok(())
        }
        None => {
            println!("'{name}' is not a known setting.");
            println!("Known settings: {}", Settings::keys().join(", "));
            Ok(())
        }
    }
}

fn set(name: &str, value: &str) -> Result<()> {
    let mut current = settings::load()?;
    // A bad value must not create the file, so parse before saving.
    current
        .set(name, value)
        .with_context(|| format!("setting '{name}'"))?;

    settings::save(&current)?;
    println!(
        "'{name}' has been set to '{}'",
        current.get(name).unwrap_or_default()
    );
    Ok(())
}

fn reset(name: &str) -> Result<()> {
    let mut current = settings::load()?;
    let previous = current.get(name);
    current
        .reset(name)
        .with_context(|| format!("resetting '{name}'"))?;

    settings::save(&current)?;
    match previous {
        Some(old) => println!(
            "'{name}' has been reset ({old} → {})",
            current.get(name).unwrap_or_default()
        ),
        None => println!("'{name}' has been reset"),
    }
    Ok(())
}

fn edit() -> Result<()> {
    let path = settings::settings_path()
        .context("cannot locate a config home, so there is no settings file to edit")?;

    // Create the file if it is missing, so the editor does not open an empty buffer that
    // would look like a broken file rather than an empty one.
    if !path.is_file() {
        settings::save(&Settings::default())?;
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "notepad".to_owned());

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("starting editor '{editor}'"))?;

    if !status.success() {
        anyhow::bail!("'{editor}' exited with {status}");
    }

    // Validate what they wrote: a typo saved into the file should be reported here rather
    // than at some later command that silently used a different value.
    match settings::load() {
        Ok(_) => {
            println!("Settings saved to {}", path.display());
            Ok(())
        }
        Err(e) => {
            println!("{} is not valid: {e}", path.display());
            std::process::exit(1);
        }
    }
}
