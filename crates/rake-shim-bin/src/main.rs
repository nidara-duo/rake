use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let self_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("shim: failed to get own path: {e}");
            return ExitCode::FAILURE;
        }
    };

    let shim_path = self_path.with_extension("shim");
    let content = match std::fs::read_to_string(&shim_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("shim: failed to read '{}': {e}", shim_path.display());
            return ExitCode::FAILURE;
        }
    };

    let (target, extra_args) = match parse_shim(&content) {
        Some(t) => t,
        None => {
            eprintln!("shim: missing 'path' in '{}'", shim_path.display());
            return ExitCode::FAILURE;
        }
    };

    let user_args: Vec<OsString> = std::env::args_os().skip(1).collect();

    let target_path = PathBuf::from(&target);
    let is_gui = is_gui_subsystem(&target_path);

    let mut cmd = Command::new(&target);
    cmd.args(&extra_args);
    cmd.args(&user_args);

    if is_gui {
        match cmd.spawn() {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("shim: failed to spawn '{}': {e}", target_path.display());
                ExitCode::FAILURE
            }
        }
    } else {
        match cmd.status() {
            Ok(status) => {
                if let Some(code) = status.code() {
                    ExitCode::from(code as u8)
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("shim: failed to run '{}': {e}", target_path.display());
                ExitCode::FAILURE
            }
        }
    }
}

/// Parse a .shim file. Returns (target_path, extra_args).
///
/// Format:
///   path = "C:\...\target.exe"
///   args = --foo --bar
fn parse_shim(content: &str) -> Option<(String, Vec<String>)> {
    let mut target: Option<String> = None;
    let mut args: Vec<String> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("path = ") {
            let cleaned = rest.trim_matches('"').trim().to_owned();
            target = Some(cleaned);
        } else if let Some(rest) = line.strip_prefix("args = ") {
            let cleaned = rest.trim().to_owned();
            if !cleaned.is_empty() {
                args = cleaned
                    .split(' ')
                    .map(|s| s.trim_matches('"').to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }

    target.map(|t| (t, args))
}

/// Check if the target executable has a GUI subsystem (IMAGE_SUBSYSTEM_WINDOWS_GUI == 2).
fn is_gui_subsystem(path: &std::path::Path) -> bool {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    if data.len() < 0x3c + 4 {
        return false;
    }
    let pe_offset = u32::from_le_bytes([data[0x3c], data[0x3d], data[0x3e], data[0x3f]]) as usize;
    if data.len() < pe_offset + 24 + 2 {
        return false;
    }
    let subsystem = u16::from_le_bytes([data[pe_offset + 24 + 68], data[pe_offset + 24 + 69]]);
    subsystem == 2
}
