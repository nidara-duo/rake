use std::path::Path;
use std::process::Command;

use rake_domain::arch::Arch;
use rake_domain::manifest::Manifest;

use crate::Result;
use crate::infra::layout::InstallRoots;
use crate::infra::script;
use crate::operations::activation::is_in_dir;
use crate::operations::download;

/// Run a manifest hook with scoop's PowerShell library and context variables.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_hook(
    phase: script::HookPhase,
    lines: &rake_domain::one_or_many::OneOrMany<String>,
    version_dir: &Path,
    active_dir: &Path,
    persist_dir: &Path,
    version: &str,
    app: &str,
    arch: &str,
    fnames: &[String],
    roots: &InstallRoots,
) -> Result<()> {
    let ctx = script::HookContext::for_phase(
        phase,
        version_dir,
        active_dir,
        persist_dir,
        version,
        app,
        arch,
        fnames,
        (&roots.root, &roots.global),
    );
    let body: Vec<String> = lines.iter().cloned().collect();
    script::run_powershell_script(&body, &ctx)
}

/// Run `installer` / `uninstaller`, exactly as `Invoke-Installer` does
/// (assets/scoop/lib/install.ps1:84).
///
/// Order matters: the executable runs first and the hook script second. A
/// `.ps1` executable is invoked inside PowerShell and is never deleted, because
/// Scoop's `Remove-Item` sits in the `else` branch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_installer(
    manifest: &Manifest,
    arch: Arch,
    dir: &Path,
    persist_dir: &Path,
    version: &str,
    app: &str,
    fnames: &[String],
    roots: &InstallRoots,
    uninstall: bool,
) -> Result<()> {
    let phase = if uninstall {
        script::HookPhase::Uninstaller
    } else {
        script::HookPhase::Installer
    };
    let (title, file, args, script_lines, keep) = if uninstall {
        let Some(spec) = manifest.resolve_uninstaller(arch) else {
            return Ok(());
        };
        (
            "Uninstaller",
            spec.file.as_deref(),
            spec.args.as_ref(),
            spec.script.as_ref(),
            false,
        )
    } else {
        let Some(spec) = manifest.resolve_installer(arch) else {
            return Ok(());
        };
        (
            "Installer",
            spec.file.as_deref(),
            spec.args.as_ref(),
            spec.script.as_ref(),
            spec.keep.unwrap_or(false),
        )
    };

    if file.is_some() || args.is_some() {
        // The filename is either explicit (`installer.file`) or the name the
        // first URL downloads to (`url_filename @(url $manifest $architecture)`).
        let name = file.map(str::to_owned).or_else(|| {
            manifest
                .resolve_urls(arch)
                .first()
                .map(|url| download::target_filename(url))
        });

        if let Some(name) = name {
            let prog = dir.join(&name);
            if !is_in_dir(dir, &prog) {
                return Err(crate::Error::Custom(format!(
                    "Error in manifest: {title} {} is outside the app directory.",
                    prog.display()
                )));
            }
            if !prog.is_file() {
                return Err(crate::Error::Custom(format!(
                    "{title} {} is missing.",
                    prog.display()
                )));
            }

            let fn_args = substitute_args(args, dir, version);
            if name.to_lowercase().ends_with(".ps1") {
                // `& $progName @fnArgs` runs in-process, where scoop's library is
                // already loaded — so the installer can call its functions.
                run_ps1_installer(
                    phase,
                    &prog,
                    &fn_args,
                    dir,
                    persist_dir,
                    version,
                    app,
                    arch.as_str(),
                    fnames,
                    roots,
                )?;
            } else {
                let status = Command::new(&prog).args(&fn_args).status()?;
                if !status.success() {
                    return Err(crate::Error::Custom(if uninstall {
                        "Uninstallation aborted.".to_owned()
                    } else {
                        "Installation aborted. You might need to run 'rake uninstall' \
                         before trying again."
                            .to_owned()
                    }));
                }
                // Scoop keeps the installer only when `installer.keep` is true;
                // the uninstaller spec has no such flag, so it is always removed.
                if !keep {
                    std::fs::remove_file(&prog)?;
                }
            }
        }
    }

    if let Some(lines) = script_lines {
        let ctx = script::HookContext::for_phase(
            phase,
            dir,
            dir,
            persist_dir,
            version,
            app,
            arch.as_str(),
            fnames,
            (&roots.root, &roots.global),
        );
        let body: Vec<String> = lines.iter().cloned().collect();
        script::run_powershell_script(&body, &ctx)?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_ps1_installer(
    phase: script::HookPhase,
    prog: &Path,
    args: &[String],
    dir: &Path,
    persist_dir: &Path,
    version: &str,
    app: &str,
    arch: &str,
    fnames: &[String],
    roots: &InstallRoots,
) -> Result<()> {
    let mut line = format!(
        "& {}",
        script::quote_powershell_string(&prog.to_string_lossy())
    );
    for arg in args {
        line.push(' ');
        line.push_str(&script::quote_powershell_string(arg));
    }

    let ctx = script::HookContext::for_phase(
        phase,
        dir,
        dir,
        persist_dir,
        version,
        app,
        arch,
        fnames,
        (&roots.root, &roots.global),
    );
    script::run_powershell_script(&[line], &ctx)
}

/// `substitute` from lib/core.ps1:1286 for the three keys `Invoke-Installer`
/// supplies. `$global` is always `False`: rake has no global installs, and
/// PowerShell renders the boolean that way.
fn substitute_args(
    args: Option<&rake_domain::one_or_many::OneOrMany<String>>,
    dir: &Path,
    version: &str,
) -> Vec<String> {
    let Some(args) = args else {
        return Vec::new();
    };
    let dir = dir.to_string_lossy();

    args.as_slice()
        .iter()
        .map(|arg| {
            arg.replace("$dir", &dir)
                .replace("$global", "False")
                .replace("$version", version)
        })
        .collect()
}
