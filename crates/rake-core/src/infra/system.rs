#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use crate::Result;

fn volume_root(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let mut root = PathBuf::new();
        for comp in path.components() {
            match comp {
                std::path::Component::Prefix(_) => {
                    root.push(comp.as_os_str());
                }
                std::path::Component::RootDir => {
                    root.push(comp.as_os_str());
                    break;
                }
                _ => break,
            }
        }
        if root.as_os_str().is_empty() {
            if let Ok(cwd) = std::env::current_dir() {
                return volume_root(&cwd);
            }
            root.push("\\");
        }
        root
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        PathBuf::from("/")
    }
}

pub fn is_ntfs(path: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;

        let root = volume_root(path);
        let path_str: Vec<u16> = root
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut fs_name = [0u16; 32];
        let result = unsafe {
            GetVolumeInformationW(
                path_str.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };

        if result == 0 {
            return Ok(false);
        }

        let len = fs_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name.len());
        let name = String::from_utf16_lossy(&fs_name[..len]);
        Ok(name == "NTFS")
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(true)
    }
}

pub fn is_long_paths_enabled() -> Result<bool> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, RegCloseKey, RegOpenKeyExW,
            RegQueryValueExW,
        };

        let subkey: Vec<u16> = "SYSTEM\\CurrentControlSet\\Control\\FileSystem"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let value_name: Vec<u16> = "LongPathsEnabled"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut hkey: HKEY = std::ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, KEY_READ, &mut hkey) };

        if status != 0 {
            return Ok(false);
        }

        let mut value: u32 = 0;
        let mut value_size: u32 = std::mem::size_of::<u32>() as u32;
        let mut value_type: u32 = 0;

        let result = unsafe {
            RegQueryValueExW(
                hkey,
                value_name.as_ptr(),
                std::ptr::null_mut(),
                &mut value_type,
                &mut value as *mut u32 as *mut u8,
                &mut value_size,
            )
        };

        unsafe {
            RegCloseKey(hkey);
        }

        if result != 0 || value_type != REG_DWORD {
            return Ok(false);
        }

        Ok(value != 0)
    }

    #[cfg(not(windows))]
    {
        Ok(true)
    }
}

pub fn is_developer_mode_enabled() -> Result<bool> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, RegCloseKey, RegOpenKeyExW,
            RegQueryValueExW,
        };

        let subkey: Vec<u16> = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\AppModelUnlock"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let value_name: Vec<u16> = "AllowDevelopmentWithoutDevLicense"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut hkey: HKEY = std::ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, KEY_READ, &mut hkey) };

        if status != 0 {
            return Ok(false);
        }

        let mut value: u32 = 0;
        let mut value_size: u32 = std::mem::size_of::<u32>() as u32;
        let mut value_type: u32 = 0;

        let result = unsafe {
            RegQueryValueExW(
                hkey,
                value_name.as_ptr(),
                std::ptr::null_mut(),
                &mut value_type,
                &mut value as *mut u32 as *mut u8,
                &mut value_size,
            )
        };

        unsafe {
            RegCloseKey(hkey);
        }

        if result != 0 || value_type != REG_DWORD {
            return Ok(false);
        }

        Ok(value != 0)
    }

    #[cfg(not(windows))]
    {
        Ok(true)
    }
}

/// Is `path` covered by a Windows Defender exclusion?
///
/// Returns `None` when the question could not be answered — PowerShell unavailable, the
/// cmdlet refusing to run, Defender disabled mid-check. The previous version returned
/// `true` in every one of those cases, including from a `catch` inside the script, so
/// `checkup` printed "OK" for a check that had not happened. A check that cannot be
/// performed is not a passing check.
pub fn check_defender_exclusion(path: &Path) -> Result<Option<bool>> {
    #[cfg(windows)]
    {
        let path_str = path.to_str().unwrap_or(".");
        // Single quotes make the value literal, so the doubling below is the whole
        // escaping rule — see infra::script for why this differs from the double-quoted
        // form used elsewhere.
        let escaped = path_str.replace('\'', "''");

        // Three distinct answers, because conflating "not excluded" with "could not tell"
        // is what produced the false OK. No `catch`: a failure has to surface as a
        // failure, not as a verdict.
        let script = vec![format!(
            "$target = [System.IO.Path]::GetFullPath('{escaped}').TrimEnd('\\').TrimEnd('/')
$found = $false
foreach ($x in @((Get-MpPreference).ExclusionPath)) {{
    if ($null -eq $x) {{ continue }}
    $n = [System.IO.Path]::GetFullPath($x).TrimEnd('\\').TrimEnd('/')
    if ($target -eq $n -or $target.StartsWith($n + '\\', [StringComparison]::OrdinalIgnoreCase)) {{
        $found = $true
        break
    }}
}}
if ($found) {{ 'EXCLUDED' }} else {{ 'NOT_EXCLUDED' }}"
        )];

        let stdout = match crate::infra::script::run_powershell_capture(&script) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Defender exclusion check could not run: {e}");
                return Ok(None);
            }
        };

        Ok(exclusion_verdict(&stdout))
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(None)
    }
}

/// Read the exclusion answer, treating anything unexpected as "unknown".
///
/// Pure so the distinction that matters can be tested without touching Defender: an
/// unrecognised answer must never be read as "excluded".
pub(crate) fn exclusion_verdict(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "EXCLUDED" => Some(true),
        "NOT_EXCLUDED" => Some(false),
        other => {
            tracing::debug!("Unexpected Defender exclusion output: {other:?}");
            None
        }
    }
}

/// Is the Defender service running?
///
/// `None` means the question could not be answered. It used to return `false` on any
/// failure, and `checkup` treats "not running" as "nothing to worry about" — so a machine
/// where PowerShell could not be spawned reported a clean bill of health for a check that
/// never ran. Both directions of that lie are now unavailable.
pub fn is_windows_defender_running() -> Result<Option<bool>> {
    #[cfg(windows)]
    {
        let script =
            vec!["(Get-Service -Name WinDefend -ErrorAction SilentlyContinue).Status".to_owned()];

        let stdout = match crate::infra::script::run_powershell_capture(&script) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Defender service query could not run: {e}");
                return Ok(None);
            }
        };

        Ok(service_running_verdict(&stdout))
    }

    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

/// Read the service-status answer, treating anything unexpected as "unknown".
pub(crate) fn service_running_verdict(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "Running" => Some(true),
        "Stopped" | "Disabled" | "StartPending" | "StopPending" | "Paused" => Some(false),
        "" => {
            tracing::debug!("Defender service query returned nothing");
            None
        }
        other => {
            tracing::debug!("Unexpected Defender service status: {other:?}");
            None
        }
    }
}

#[cfg(test)]
mod defender_tests {
    use super::*;

    /// The bug this pins: an unrecognised answer used to be read as "excluded", so
    /// `checkup` printed OK for a check that never ran. Anything unrecognised must be
    /// "unknown", never a verdict.
    #[test]
    fn exclusion_verdict_reads_the_two_real_answers() {
        assert_eq!(exclusion_verdict("EXCLUDED"), Some(true));
        assert_eq!(exclusion_verdict("NOT_EXCLUDED"), Some(false));
        assert_eq!(exclusion_verdict("  EXCLUDED  \r\n"), Some(true));
    }

    #[test]
    fn exclusion_verdict_treats_anything_else_as_unknown() {
        for junk in ["", "  ", "ERROR", "True", "excluded", "EXCLUDED extra"] {
            assert_eq!(
                exclusion_verdict(junk),
                None,
                "{junk:?} must not be read as excluded"
            );
        }
    }

    /// Same rule for the service query. The old code returned `false` when PowerShell
    /// could not be spawned, which `checkup` read as "Defender not running, nothing to
    /// report" — a green result for a check that did not happen.
    #[test]
    fn service_verdict_reads_the_two_real_answers() {
        assert_eq!(service_running_verdict("Running"), Some(true));
        assert_eq!(service_running_verdict("Stopped"), Some(false));
        assert_eq!(service_running_verdict("Disabled"), Some(false));
    }

    #[test]
    fn service_verdict_treats_anything_else_as_unknown() {
        // Empty is what a failed cmdlet prints, so it must not become "not running".
        for junk in ["", "   ", "NoService", "true", "Running extra"] {
            assert_eq!(
                service_running_verdict(junk),
                None,
                "{junk:?} must not be read as a verdict"
            );
        }
    }

    /// The two are separate questions and must not be inferred from one another: a running
    /// Defender says nothing about whether the path is excluded.
    #[test]
    fn the_two_answers_are_independent() {
        assert_eq!(
            (
                service_running_verdict("Running"),
                exclusion_verdict("NOT_EXCLUDED")
            ),
            (Some(true), Some(false))
        );
        assert_eq!(
            (
                service_running_verdict("Stopped"),
                exclusion_verdict("EXCLUDED")
            ),
            (Some(false), Some(true))
        );
    }
}
