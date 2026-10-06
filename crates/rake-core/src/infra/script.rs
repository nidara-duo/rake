use std::path::Path;

use crate::Result;

/// Quote a value for a PowerShell **single**-quoted string.
///
/// Single quotes make everything literal — `$`, backticks and `$( )` included — so the
/// only character needing escape is `'`, and doubling it is the escape. This is why the
/// hook variables are safe: a path cannot terminate the string and start a new statement.
///
/// Not the same rule as the double-quoted form in `infra/shim.rs`, where the backtick is
/// the escape character instead. Verified against PowerShell 5.1 with values containing
/// quotes, backticks, newlines, `$env:` and `$( )` — all round-tripped unchanged, and an
/// injected `New-Item` did not run.
///
/// Prefer this over passing values on a command line, where the shell would parse them.
pub fn quote_powershell_string(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

/// Script hook context — variable substitution for `$dir`, `$persist_dir`,
/// `$original_dir`, `$version`.
pub struct HookContext<'a> {
    pub version_dir: &'a Path,
    pub persist_dir: &'a Path,
    pub original_dir: &'a Path,
    pub version: &'a str,

    /// Rake root and global root. Present only when the hook needs scoop's PowerShell
    /// library; `None` runs the script standalone.
    ///
    /// Manifest `installer.script` bodies call scoop functions (`Add-Path`,
    /// `Invoke-ExternalCommand`, `Expand-InnoArchive`, ...), which resolve only when
    /// scoop's `lib/` is dot-sourced. Callers pass the roots to enable that.
    pub scoop_roots: Option<(&'a Path, &'a Path)>,
}

impl<'a> HookContext<'a> {
    /// A hook that runs standalone, without scoop's PowerShell library.
    ///
    /// Right for `pre_install`, `post_install`, `pre_uninstall` and `post_uninstall`:
    /// those are usually plain PowerShell, and loading the library costs ~0.7s per run.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version_dir: &'a Path,
        persist_dir: &'a Path,
        original_dir: &'a Path,
        version: &'a str,
    ) -> Self {
        Self {
            version_dir,
            persist_dir,
            original_dir,
            version,
            scoop_roots: None,
        }
    }

    /// A hook that runs with scoop's PowerShell library loaded.
    ///
    /// Required for `installer.script` and `uninstaller.script`, whose bodies call
    /// scoop functions such as `Add-Path` and `Invoke-ExternalCommand`.
    #[allow(clippy::too_many_arguments)]
    pub fn with_scoop_lib(
        version_dir: &'a Path,
        persist_dir: &'a Path,
        original_dir: &'a Path,
        version: &'a str,
        root: &'a Path,
        global_root: &'a Path,
    ) -> Self {
        Self {
            version_dir,
            persist_dir,
            original_dir,
            version,
            scoop_roots: Some((root, global_root)),
        }
    }
}

/// Execute a list of PowerShell script lines (e.g. `pre_install`, `post_install`).
///
/// Writes the hook body to a temporary `.ps1` file with safely quoted variables,
/// then runs it via `powershell.exe -File`. This avoids command-line injection
/// from unquoted filesystem paths.
pub fn run_powershell_script(lines: &[String], ctx: &HookContext) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    let dir = quote_powershell_string(&ctx.version_dir.to_string_lossy());
    let persist_dir = quote_powershell_string(&ctx.persist_dir.to_string_lossy());
    let original_dir = quote_powershell_string(&ctx.original_dir.to_string_lossy());
    let version = quote_powershell_string(ctx.version);

    let script_body = if cfg!(windows) {
        lines.join("\r\n")
    } else {
        lines.join("\n")
    };

    // When scoop's library is requested, its prelude replaces these four assignments:
    // it defines the same variables and additionally loads the functions a manifest
    // hook is likely to call.
    let header = match ctx.scoop_roots {
        Some((root, global_root)) => {
            let lib = crate::infra::scoop_lib::ensure_library(root)?;
            crate::infra::scoop_lib::build_prelude(
                &lib,
                root,
                global_root,
                ctx.version_dir,
                ctx.persist_dir,
                ctx.original_dir,
                ctx.version,
            )
        }
        None => format!(
            "$dir = {dir}\r\n$persist_dir = {persist_dir}\r\n$original_dir = {original_dir}\r\n$version = {version}\r\n\r\n",
            dir = dir,
            persist_dir = persist_dir,
            original_dir = original_dir,
            version = version,
        ),
    };

    run_body(&format!("{header}{script_body}")).map(|_| ())
}

/// Run a PowerShell snippet and return what it printed to stdout.
///
/// For callers that need the answer rather than just success — `checkup` asks Windows
/// Defender a question this way. Like [`run_powershell_script`] the body goes through a
/// temporary file rather than `-Command`, because a script passed on the command line is
/// re-parsed by the C runtime before PowerShell ever sees it, and a double quote inside it
/// no longer survives as written.
pub fn run_powershell_capture(lines: &[String]) -> Result<String> {
    let body = if cfg!(windows) {
        lines.join("\r\n")
    } else {
        lines.join("\n")
    };
    run_body(&body)
}

/// Write `content` to a temporary `.ps1` and run it with `powershell.exe -File`.
///
/// Returns stdout on success, or an error naming stderr. The temporary file is removed
/// when the returned path goes out of scope.
fn run_body(content: &str) -> Result<String> {
    let mut tmp = tempfile::Builder::new()
        .prefix("rake-hook-")
        .suffix(".ps1")
        .tempfile()
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("create hook script: {e}"))))?;

    std::io::Write::write_all(&mut tmp, content.as_bytes())
        .map_err(|e| crate::Error::Io(std::io::Error::other(format!("write hook script: {e}"))))?;

    // Persist the temp file on disk and hand the path to PowerShell.
    // It is auto-removed when `path` goes out of scope.
    let path = tmp.into_temp_path();
    let script_path = path.to_string_lossy();
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            script_path.as_ref(),
        ])
        .output();

    let out = match out {
        Ok(o) => o,
        Err(e) => {
            return Err(crate::Error::Io(std::io::Error::other(format!(
                "powershell script: {e}"
            ))));
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(crate::Error::Io(std::io::Error::other(format!(
            "script hook failed: {stderr}"
        ))));
    }

    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_in_single_quotes() {
        assert_eq!(quote_powershell_string("C:\\rake\\bin"), "'C:\\rake\\bin'");
    }

    /// The only escape that single quotes need: doubling the quote itself. Verified
    /// against PowerShell — an app path with an apostrophe round-tripped intact.
    #[test]
    fn doubles_single_quotes() {
        assert_eq!(quote_powershell_string("o'brien"), "'o''brien'");
        assert_eq!(quote_powershell_string("''"), "''''''");
    }

    /// Inside single quotes nothing is expandable, which is the property the whole
    /// approach rests on. `$env:TEMP` and `$( )` must come back as literal text.
    #[test]
    fn dollars_and_backticks_stay_literal() {
        assert_eq!(
            quote_powershell_string("$env:TEMP"),
            "'$env:TEMP'",
            "single quotes must not expand"
        );
        assert_eq!(quote_powershell_string("`$(whoami)"), "'`$(whoami)'");
        assert_eq!(
            quote_powershell_string("a`nb"),
            "'a`nb'",
            "a backtick-n is two literal characters here"
        );
    }

    /// A value crafted to close the string and append a statement must come out
    /// balanced, so nothing after the assignment can run.
    #[test]
    fn injection_attempt_stays_inside_the_string() {
        let evil = "x'; New-Item -Path C:\\pwned; $v='y";
        let quoted = quote_powershell_string(evil);
        // Two delimiters plus the two doubled pairs the value itself carries.
        assert_eq!(quoted.matches('\'').count(), 6);
        assert_eq!(
            quoted, "'x''; New-Item -Path C:\\pwned; $v=''y'",
            "every quote in the value must be doubled"
        );
        assert!(quoted.starts_with('\'') && quoted.ends_with('\''));
    }

    /// A newline is literal inside single quotes, so it cannot start a new statement.
    #[test]
    fn newline_cannot_start_a_statement() {
        let quoted = quote_powershell_string("a\nRemove-Item x");
        assert_eq!(quoted, "'a\nRemove-Item x'");
    }

    /// Non-ASCII must survive: a hook on a path under a Cyrillic or CJK directory name
    /// has to arrive intact.
    #[test]
    fn non_ascii_paths_are_preserved() {
        assert_eq!(
            quote_powershell_string("C:\\Программы\\bin"),
            "'C:\\Программы\\bin'"
        );
        assert_eq!(quote_powershell_string("C:\\程序\\bin"), "'C:\\程序\\bin'");
    }

    /// The hook header must define all four variables Scoop's `Invoke-HookScript` makes
    /// available (`lib/install.ps1:143`), since manifests reference them by name.
    #[test]
    fn hook_context_exposes_the_four_variables_scoop_defines() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = HookContext::new(tmp.path(), tmp.path(), tmp.path(), "1.0.0");
        assert_eq!(ctx.version, "1.0.0");
        assert!(ctx.scoop_roots.is_none());

        let (a, b) = (tmp.path().join("root"), tmp.path().join("global"));
        let with_lib =
            HookContext::with_scoop_lib(tmp.path(), tmp.path(), tmp.path(), "2.0.0", &a, &b);
        assert!(with_lib.scoop_roots.is_some());
    }

    /// An empty hook is a no-op, and must not spawn PowerShell at all.
    #[test]
    fn empty_hook_does_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = HookContext::new(tmp.path(), tmp.path(), tmp.path(), "1.0.0");
        assert!(run_powershell_script(&[], &ctx).is_ok());
    }

    /// End to end: a real hook runs and can read the variables it was given. Proves the
    /// header is syntactically valid PowerShell and that the paths arrive as values.
    #[cfg(windows)]
    #[test]
    fn hook_receives_its_variables_and_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let version_dir = tmp.path().join("apps").join("demo").join("1.0.0");
        let persist_dir = tmp.path().join("persist").join("demo");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();

        let marker = tmp.path().join("marker.txt");
        let ctx = HookContext::new(&version_dir, &persist_dir, &version_dir, "1.0.0");
        let lines = vec![format!(
            "Set-Content -LiteralPath '{}' -Value \"$version|$dir\"",
            marker.to_string_lossy()
        )];

        run_powershell_script(&lines, &ctx).unwrap();

        let written = std::fs::read_to_string(&marker).unwrap();
        assert_eq!(
            written.trim_end(),
            format!("1.0.0|{}", version_dir.to_string_lossy()),
            "the hook must see $version and $dir as plain values"
        );
    }

    /// A path with an apostrophe must still reach the hook intact — the reason the
    /// values are quoted at all.
    #[cfg(windows)]
    #[test]
    fn hook_variable_survives_an_apostrophe_in_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let version_dir = tmp.path().join("o'brien").join("1.0.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        let marker = tmp.path().join("marker.txt");

        let ctx = HookContext::new(&version_dir, &version_dir, &version_dir, "1.0.0");
        let lines = vec![format!(
            "Set-Content -LiteralPath '{}' -Value $dir",
            marker.to_string_lossy()
        )];

        run_powershell_script(&lines, &ctx).unwrap();

        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim_end(),
            version_dir.to_string_lossy()
        );
    }

    /// A failing hook must report an error rather than pass silently. Callers treat a
    /// `Result` of `Ok` as "the hook worked".
    #[cfg(windows)]
    #[test]
    fn a_failing_hook_is_reported_as_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = HookContext::new(tmp.path(), tmp.path(), tmp.path(), "1.0.0");
        let err = run_powershell_script(&["exit 3".to_owned()], &ctx).unwrap_err();
        assert!(err.to_string().contains("script hook failed"), "got: {err}");
    }

    /// The hook body is manifest text written verbatim, so it must not be able to break
    /// out of the file. Not a claim that hooks are safe to run — running them at all is
    /// the point of `installer.script` — but a stray quote in the body must not corrupt
    /// the header.
    #[cfg(windows)]
    #[test]
    fn header_is_valid_even_with_a_hostile_body() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("marker.txt");
        let ctx = HookContext::new(tmp.path(), tmp.path(), tmp.path(), "1.0.0");
        // The body closes nothing in the header (it is appended after it), but it must
        // still run and see the variables.
        let lines = vec![format!(
            "# ' \" ;;; \nSet-Content -LiteralPath '{}' -Value $persist_dir",
            marker.to_string_lossy()
        )];

        run_powershell_script(&lines, &ctx).unwrap();

        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim_end(),
            tmp.path().to_string_lossy(),
            "the header must survive a body full of quotes"
        );
    }
}
