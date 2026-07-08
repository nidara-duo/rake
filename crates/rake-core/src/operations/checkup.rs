use serde::Serialize;

use crate::Result;
use crate::infra::system;
use crate::operations::query;
use crate::session::Session;
use rake_domain::package::Package;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CheckupSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckupItem {
    pub name: String,
    pub severity: CheckupSeverity,
    pub message: String,
    pub help: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckupReport {
    pub items: Vec<CheckupItem>,
}

impl CheckupItem {
    fn ok(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            severity: CheckupSeverity::Info,
            message: "OK".to_owned(),
            help: None,
        }
    }

    fn warn(
        name: impl Into<String>,
        message: impl Into<String>,
        help: Option<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            severity: CheckupSeverity::Warning,
            message: message.into(),
            help: help.map(|h| h.into()),
        }
    }

    fn error(
        name: impl Into<String>,
        message: impl Into<String>,
        help: Option<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            severity: CheckupSeverity::Error,
            message: message.into(),
            help: help.map(|h| h.into()),
        }
    }
}

pub fn run_checkup(session: &Session, verbose: bool) -> Result<CheckupReport> {
    let installed = query::query_installed_inner(session).unwrap_or_default();

    let mut items = vec![
        check_main_bucket(session)?,
        check_helper_installed(&installed, &["7zip"], "7-Zip", "rake install 7zip"),
        check_helper_installed(
            &installed,
            &["innounp", "innounp-unicode"],
            "Inno Setup Unpacker",
            "rake install innounp",
        ),
        check_helper_installed(
            &installed,
            &["dark", "wixtoolset"],
            "Dark (WiX Toolset)",
            "rake install dark",
        ),
        check_filesystem(session)?,
        check_long_paths(),
        check_defender(session)?,
    ];

    if verbose {
        items.push(check_developer_mode());
    }

    Ok(CheckupReport { items })
}

fn check_main_bucket(session: &Session) -> Result<CheckupItem> {
    let buckets = crate::bucket::added_buckets(session)?;
    let has_main = buckets.iter().any(|b| b.name() == "main");

    Ok(if has_main {
        CheckupItem::ok("Main bucket")
    } else {
        CheckupItem::warn(
            "Main bucket",
            "Main bucket is not added.",
            Some("rake bucket add main"),
        )
    })
}

fn check_helper_installed(
    installed: &[Package],
    app_names: &[&str],
    display_name: &str,
    install_cmd: &str,
) -> CheckupItem {
    let found = installed.iter().any(|p| {
        let name = p.name().to_ascii_lowercase();
        app_names.iter().any(|n| name == *n)
    });

    if found {
        CheckupItem::ok(format!("{display_name} installation"))
    } else {
        CheckupItem::warn(
            format!("{display_name} installation"),
            format!(
                "'{display_name}' is not installed! It's required for unpacking certain installers."
            ),
            Some(install_cmd.to_string()),
        )
    }
}

fn check_filesystem(session: &Session) -> Result<CheckupItem> {
    let root = session
        .config()
        .root_path
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("."));

    let is_ntfs = system::is_ntfs(root)?;

    Ok(if is_ntfs {
        CheckupItem::ok("Filesystem type")
    } else {
        CheckupItem::error(
            "Filesystem type",
            "Scoop requires an NTFS volume to work!",
            Some("Change SCOOP root path to an NTFS drive."),
        )
    })
}

fn check_long_paths() -> CheckupItem {
    let enabled = system::is_long_paths_enabled().unwrap_or(false);

    if enabled {
        CheckupItem::ok("Long path support")
    } else {
        CheckupItem::warn(
            "Long path support",
            "LongPaths support is not enabled.",
            Some(
                "Run: Set-ItemProperty 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\FileSystem' -Name 'LongPathsEnabled' -Value 1",
            ),
        )
    }
}

fn check_developer_mode() -> CheckupItem {
    let enabled = system::is_developer_mode_enabled().unwrap_or(false);

    if enabled {
        CheckupItem::ok("Windows Developer Mode")
    } else {
        CheckupItem::warn(
            "Windows Developer Mode",
            "Windows Developer Mode is not enabled. Operations relevant to symlinks may fail without proper rights.",
            Some("Enable Developer Mode in Settings > Update & Security > For developers."),
        )
    }
}

fn check_defender(session: &Session) -> Result<CheckupItem> {
    let running = system::is_windows_defender_running()?;
    if !running {
        return Ok(CheckupItem::ok("Windows Defender exclusion"));
    }

    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let excluded = system::check_defender_exclusion(&root)?;

    Ok(if excluded {
        CheckupItem::ok("Windows Defender exclusion")
    } else {
        CheckupItem::warn(
            "Windows Defender exclusion",
            "Windows Defender may slow down or disrupt installs with realtime scanning.",
            Some(format!(
                "Run: Add-MpPreference -ExclusionPath '{}'",
                root.display()
            )),
        )
    })
}
