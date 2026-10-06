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
    let names: Vec<&str> = buckets.iter().map(|b| b.name()).collect();
    Ok(main_bucket_item(&names))
}

/// The verdict from a list of bucket names, so the lookup can be tested without a
/// filesystem. Matching is case-insensitive, because a bucket directory on Windows can
/// arrive as `Main` from a clone done by a case-sensitive tool.
fn main_bucket_item(bucket_names: &[&str]) -> CheckupItem {
    let has_main = bucket_names.iter().any(|n| n.eq_ignore_ascii_case("main"));

    if has_main {
        CheckupItem::ok("Main bucket")
    } else {
        CheckupItem::warn(
            "Main bucket",
            "Main bucket is not added.",
            Some("rake bucket add main"),
        )
    }
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

    Ok(filesystem_item(system::is_ntfs(root)?))
}

/// The verdict for the filesystem probe, separated from making it.
///
/// Every check below is split this way: the probe touches the machine, the decision does
/// not. That is what lets the severity and wording be tested at all, which matters most
/// for the checks whose severity encodes advice.
fn filesystem_item(is_ntfs: bool) -> CheckupItem {
    if is_ntfs {
        CheckupItem::ok("Filesystem type")
    } else {
        // An error rather than a warning: without NTFS, junctions and case-sensitivity
        // behave differently and packages genuinely misbehave.
        CheckupItem::error(
            "Filesystem type",
            "Scoop requires an NTFS volume to work!",
            Some("Change SCOOP root path to an NTFS drive."),
        )
    }
}

fn check_long_paths() -> CheckupItem {
    // A failed probe reports the same as "not enabled", which is the cautious direction:
    // the advice is still valid and nothing is claimed to have been verified.
    long_paths_item(system::is_long_paths_enabled().unwrap_or(false))
}

fn long_paths_item(enabled: bool) -> CheckupItem {
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
    developer_mode_item(system::is_developer_mode_enabled().unwrap_or(false))
}

fn developer_mode_item(enabled: bool) -> CheckupItem {
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
    match system::is_windows_defender_running()? {
        // Only a positive answer lets the question through. Treating "could not tell" as
        // "not running" reported a healthy checkup on a machine where PowerShell could
        // not even be started.
        None => {
            return Ok(CheckupItem::warn(
                "Windows Defender exclusion",
                "Could not read the Defender service state.",
                Some("Check that PowerShell is available"),
            ));
        }
        Some(false) => return Ok(CheckupItem::ok("Windows Defender exclusion")),
        Some(true) => {}
    }

    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let excluded = system::check_defender_exclusion(&root)?;

    // Three outcomes, not two. `Some(false)` is a real "not excluded"; `None` means the
    // question could not be answered, and reporting that as either verdict would be a lie
    // in one direction or the other.
    Ok(match excluded {
        Some(true) => CheckupItem::ok("Windows Defender exclusion"),
        Some(false) => CheckupItem::warn(
            "Windows Defender exclusion",
            "Windows Defender may slow down or disrupt installs with realtime scanning.",
            Some(format!(
                "Run: Add-MpPreference -ExclusionPath '{}'",
                root.display()
            )),
        ),
        None => CheckupItem::warn(
            "Windows Defender exclusion",
            "Could not read the Defender exclusion list.",
            Some("Check that PowerShell is available and run: Get-MpPreference"),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::manifest::Manifest;
    use rake_domain::package::{PackageIdent, PackageSource, PackageStatus};

    fn pkg(name: &str) -> Package {
        Package::new(
            PackageIdent::new("main", name),
            serde_json::from_str::<Manifest>(r#"{"version":"1.0.0"}"#).unwrap(),
            Some(PackageSource::Bucket("main".to_owned())),
            PackageStatus::Installed(rake_domain::package::InstallState {
                version: "1.0.0".to_owned(),
                bucket: Some("main".to_owned()),
                arch: "64bit".to_owned(),
                held: false,
                url: None,
            }),
        )
    }

    fn installed(names: &[&str]) -> Vec<Package> {
        names.iter().map(|n| pkg(n)).collect()
    }

    /// A helper counts as installed under any of its alternative names — `innounp` and
    /// `innounp-unicode` are the same thing for this purpose.
    #[test]
    fn helper_matches_any_of_its_names() {
        let items = installed(&["innounp-unicode"]);
        let item = check_helper_installed(
            &items,
            &["innounp", "innounp-unicode"],
            "Inno",
            "rake install innounp",
        );
        assert_eq!(item.severity, CheckupSeverity::Info);
    }

    /// Scoop treats app names case-insensitively, and a cloned bucket can deliver `7Zip`.
    #[test]
    fn helper_matching_is_case_insensitive() {
        for spelling in ["7zip", "7Zip", "7ZIP"] {
            let items = installed(&[spelling]);
            let item = check_helper_installed(&items, &["7zip"], "7-Zip", "rake install 7zip");
            assert_eq!(
                item.severity,
                CheckupSeverity::Info,
                "{spelling} should count as installed"
            );
        }
    }

    /// Missing helpers must carry the command that fixes them — that is the whole point of
    /// the check.
    #[test]
    fn missing_helper_names_the_fix() {
        let item = check_helper_installed(&[], &["7zip"], "7-Zip", "rake install 7zip");
        assert_eq!(item.severity, CheckupSeverity::Warning);
        assert_eq!(item.help.as_deref(), Some("rake install 7zip"));
        assert!(
            item.message.contains("not installed"),
            "got: {}",
            item.message
        );
    }

    /// A similar name must not satisfy the check.
    #[test]
    fn a_similar_name_does_not_count() {
        let items = installed(&["innounpx", "7zipper"]);
        assert_eq!(
            check_helper_installed(&items, &["innounp"], "Inno", "x").severity,
            CheckupSeverity::Warning
        );
        assert_eq!(
            check_helper_installed(&items, &["7zip"], "7-Zip", "x").severity,
            CheckupSeverity::Warning
        );
    }

    /// The filesystem check is an error, not a warning: without NTFS packages misbehave
    /// rather than merely run slowly.
    #[test]
    fn non_ntfs_is_an_error() {
        let bad = filesystem_item(false);
        assert_eq!(bad.severity, CheckupSeverity::Error);
        assert_eq!(filesystem_item(true).severity, CheckupSeverity::Info);
    }

    #[test]
    fn long_paths_and_developer_mode_are_warnings_when_off() {
        assert_eq!(long_paths_item(false).severity, CheckupSeverity::Warning);
        assert_eq!(long_paths_item(true).severity, CheckupSeverity::Info);
        assert_eq!(
            developer_mode_item(false).severity,
            CheckupSeverity::Warning
        );
        assert_eq!(developer_mode_item(true).severity, CheckupSeverity::Info);
    }

    /// Every check that can fail must say how to fix it, otherwise it is noise.
    #[test]
    fn failing_checks_always_carry_advice() {
        let items = [
            filesystem_item(false),
            long_paths_item(false),
            developer_mode_item(false),
            main_bucket_item(&[]),
            check_helper_installed(&[], &["7zip"], "7-Zip", "rake install 7zip"),
        ];
        for item in items {
            assert!(item.help.is_some(), "{} has no help text", item.name);
            assert_ne!(item.severity, CheckupSeverity::Info, "{}", item.name);
        }
    }

    /// Passing checks must not print advice — "OK" with a command to run is noise.
    #[test]
    fn passing_checks_carry_no_advice() {
        for item in [
            filesystem_item(true),
            long_paths_item(true),
            developer_mode_item(true),
            main_bucket_item(&["main"]),
            check_helper_installed(&installed(&["7zip"]), &["7zip"], "7-Zip", "x"),
        ] {
            assert_eq!(item.severity, CheckupSeverity::Info, "{}", item.name);
            assert!(item.help.is_none(), "{} has advice on success", item.name);
        }
    }

    #[test]
    fn main_bucket_is_matched_case_insensitively() {
        assert_eq!(main_bucket_item(&["Main"]).severity, CheckupSeverity::Info);
        assert_eq!(
            main_bucket_item(&["extras"]).severity,
            CheckupSeverity::Warning
        );
    }

    /// Other buckets present but not main is still a warning, and the advice is specific.
    #[test]
    fn a_bucket_tree_without_main_says_what_to_add() {
        let item = main_bucket_item(&["extras", "versions"]);
        assert_eq!(item.severity, CheckupSeverity::Warning);
        assert_eq!(item.help.as_deref(), Some("rake bucket add main"));
    }
}
