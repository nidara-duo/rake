//! Scoop's PowerShell library, vendored so manifest hooks can call its functions.
//!
//! Manifest `installer.script` bodies are not standalone PowerShell. They call into
//! scoop itself — `Add-Path`, `Invoke-ExternalCommand`, `Expand-InnoArchive` and
//! friends — which only resolve when scoop's `lib/` has been dot-sourced. Running such
//! a script with a bare `powershell -File` therefore dies on the first call.
//!
//! The files under `assets/scoop/lib` are scoop's own, copied verbatim. They are
//! written out to `<root>/runtime/scoop/lib` on first use and re-used afterwards, so a
//! normal install pays the dot-sourcing cost only for the hooks it actually runs.

use std::path::{Path, PathBuf};

use crate::Result;

/// scoop's `lib/` files, embedded at compile time.
///
/// Load order matters: `core.ps1` defines the primitives and the directory globals that
/// every other file relies on, and `install.ps1` calls into `core`, `manifest` and
/// `shortcuts`. The remainder are self-contained or pulled in by the ones above.
///
/// Deliberately excluded: `autoupdate.ps1` and `database.ps1` back `scoop checkup` and
/// the SQLite manifest cache, neither of which a manifest hook touches. `help.ps1` and
/// `getopt.ps1` serve scoop's own CLI argument parsing.
const LIBS: &[(&str, &str)] = &[
    ("core.ps1", include_str!("../../assets/scoop/lib/core.ps1")),
    (
        "system.ps1",
        include_str!("../../assets/scoop/lib/system.ps1"),
    ),
    ("json.ps1", include_str!("../../assets/scoop/lib/json.ps1")),
    (
        "manifest.ps1",
        include_str!("../../assets/scoop/lib/manifest.ps1"),
    ),
    (
        "install.ps1",
        include_str!("../../assets/scoop/lib/install.ps1"),
    ),
    (
        "decompress.ps1",
        include_str!("../../assets/scoop/lib/decompress.ps1"),
    ),
    (
        "download.ps1",
        include_str!("../../assets/scoop/lib/download.ps1"),
    ),
    (
        "versions.ps1",
        include_str!("../../assets/scoop/lib/versions.ps1"),
    ),
    (
        "depends.ps1",
        include_str!("../../assets/scoop/lib/depends.ps1"),
    ),
    (
        "shortcuts.ps1",
        include_str!("../../assets/scoop/lib/shortcuts.ps1"),
    ),
    (
        "psmodules.ps1",
        include_str!("../../assets/scoop/lib/psmodules.ps1"),
    ),
    (
        "buckets.ps1",
        include_str!("../../assets/scoop/lib/buckets.ps1"),
    ),
    (
        "commands.ps1",
        include_str!("../../assets/scoop/lib/commands.ps1"),
    ),
    (
        "description.ps1",
        include_str!("../../assets/scoop/lib/description.ps1"),
    ),
    (
        "diagnostic.ps1",
        include_str!("../../assets/scoop/lib/diagnostic.ps1"),
    ),
];

/// Version stamp for the extracted library.
///
/// Bump when the embedded files change so a stale extraction is rewritten instead of
/// being silently reused. Deriving it from the content would be self-maintaining.
const EXTRACT_VERSION: u32 = 1;

/// Write the vendored library under `<root>` and return its `lib` directory.
///
/// Existing files are left alone when their contents already match, so repeated
/// installs neither rewrite 222 KB nor invalidate anything. A missing or mismatched
/// file is (re)written.
pub fn ensure_library(root: &Path) -> Result<PathBuf> {
    let runtime = root.join("runtime").join("scoop");
    let lib = runtime.join("lib");
    let stamp = runtime.join("VERSION");

    let want = EXTRACT_VERSION.to_string();
    let have = std::fs::read_to_string(&stamp).unwrap_or_default();
    let fresh = have == want && LIBS.iter().all(|(name, _)| lib.join(name).is_file());

    if !fresh {
        crate::infra::fs::ensure_dir(&lib)?;
        for (name, contents) in LIBS {
            let path = lib.join(name);
            let current = std::fs::read_to_string(&path).ok();
            if current.as_deref() != Some(*contents) {
                std::fs::write(&path, contents)?;
            }
        }
        std::fs::write(&stamp, &want)?;
    }

    Ok(lib)
}

/// Build the PowerShell prelude that loads scoop's library and defines the hook
/// variables a manifest script may reference.
///
/// `$dir`, `$persist_dir`, `$original_dir` and `$version` are the same four scoop
/// exposes to `Invoke-HookScript`. `$global` matters just as much: manifests routinely
/// write `Add-Path -Path "$persist_dir\bin" -Global:$global -Force`, and leaving the
/// variable undefined makes `$global` resolve to `$false` only by accident — it needs
/// to be a real variable so that `-Global:$global` parses and passes `false` the way
/// scoop passes it for a non-global install.
///
/// `$env:SCOOP` is set before dot-sourcing because `core.ps1` derives `$scoopdir`,
/// `$cachedir` and `$scoopPathEnvVar` from it while loading (lib/core.ps1:1375-1392).
/// Without it the library would fall back to its own install path and resolve helpers
/// such as `innounp.exe` and `7z.exe` against the wrong tree.
pub fn build_prelude(
    lib: &Path,
    root: &Path,
    global_path: &Path,
    ctx_dir: &Path,
    persist_dir: &Path,
    original_dir: &Path,
    version: &str,
) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let lib_q = q(&lib.to_string_lossy());
    let root_q = q(&root.to_string_lossy());
    let global_q = q(&global_path.to_string_lossy());

    let mut out = String::new();
    out.push_str("# Rake hook prelude: loads scoop's PowerShell library.\n");
    out.push_str("# Generated by crates/rake-core/src/infra/scoop_lib.rs - do not edit.\n");
    out.push_str("$ErrorActionPreference = 'Stop'\n");
    out.push_str(&format!("$env:SCOOP = {}\n", root_q));
    out.push_str(&format!("$env:SCOOP_GLOBAL = {}\n", global_q));
    out.push_str(&format!("$rakeLib = {}\n", lib_q));
    out.push_str("foreach ($f in @(");
    let names: Vec<String> = LIBS.iter().map(|(n, _)| format!("'{}'", n)).collect();
    out.push_str(&names.join(", "));
    out.push_str(")) { . (Join-Path $rakeLib $f) }\n");
    out.push_str("$global = $false\n");
    out.push_str(&format!("$dir = {}\n", q(&ctx_dir.to_string_lossy())));
    out.push_str(&format!(
        "$original_dir = {}\n",
        q(&original_dir.to_string_lossy())
    ));
    out.push_str(&format!(
        "$persist_dir = {}\n",
        q(&persist_dir.to_string_lossy())
    ));
    out.push_str(&format!("$version = {}\n", q(version)));
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prelude_loads_every_library_file() {
        let prelude = build_prelude(
            Path::new(r"C:\rake\runtime\scoop\lib"),
            Path::new(r"C:\rake"),
            Path::new(r"C:\ProgramData\rake"),
            Path::new(r"C:\rake\apps\demo\1.0.0"),
            Path::new(r"C:\rake\persist\demo"),
            Path::new(r"C:\rake\apps\demo\1.0.0"),
            "1.0.0",
        );
        for (name, _) in LIBS {
            assert!(prelude.contains(&format!("'{name}'")), "missing {name}");
        }
    }

    /// A manifest hook writing `-Global:$global` needs the variable to exist. Without
    /// this line PowerShell expands `$global` to nothing and the parameter binding
    /// turns `-Global:` into a syntax error.
    #[test]
    fn prelude_defines_global_as_a_real_variable() {
        let prelude = build_prelude(
            Path::new(r"C:\rake\runtime\scoop\lib"),
            Path::new(r"C:\rake"),
            Path::new(r"C:\ProgramData\rake"),
            Path::new(r"C:\rake\apps\demo\1.0.0"),
            Path::new(r"C:\rake\persist\demo"),
            Path::new(r"C:\rake\apps\demo\1.0.0"),
            "1.0.0",
        );
        assert!(prelude.contains("\n$global = $false\n"));
    }

    /// core.ps1 resolves $scoopdir from $env:SCOOP while loading, so it has to be set
    /// before the dot-sourcing loop, not after.
    #[test]
    fn prelude_sets_scoop_env_before_dot_sourcing() {
        let prelude = build_prelude(
            Path::new(r"C:\rake\runtime\scoop\lib"),
            Path::new(r"C:\rake"),
            Path::new(r"C:\ProgramData\rake"),
            Path::new(r"C:\rake\apps\demo\1.0.0"),
            Path::new(r"C:\rake\persist\demo"),
            Path::new(r"C:\rake\apps\demo\1.0.0"),
            "1.0.0",
        );
        let env_at = prelude.find("$env:SCOOP = ").expect("SCOOP must be set");
        let dot_at = prelude
            .find(". (Join-Path $rakeLib")
            .expect("must dot-source");
        assert!(
            env_at < dot_at,
            "SCOOP must be set before the library loads"
        );
    }

    #[test]
    fn single_quotes_in_paths_are_escaped() {
        let prelude = build_prelude(
            Path::new(r"C:\rake\lib"),
            Path::new(r"C:\o'brien\rake"),
            Path::new(r"C:\ProgramData\rake"),
            Path::new(r"C:\o'brien\rake\apps\demo\1.0.0"),
            Path::new(r"C:\rake\persist\demo"),
            Path::new(r"C:\o'brien\rake\apps\demo\1.0.0"),
            "1.0.0",
        );
        assert!(prelude.contains("o''brien"), "apostrophe must be doubled");
        assert!(
            !prelude.contains("o'brien"),
            "raw apostrophe would break quoting"
        );
    }

    #[test]
    fn library_is_written_and_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = ensure_library(tmp.path()).unwrap();
        assert!(lib.join("core.ps1").is_file());
        assert!(lib.join("install.ps1").is_file());
        assert_eq!(
            std::fs::read_to_string(lib.join("core.ps1")).unwrap(),
            LIBS[0].1
        );

        // Second call must be a no-op that still returns the same path.
        let again = ensure_library(tmp.path()).unwrap();
        assert_eq!(again, lib);
    }
}
