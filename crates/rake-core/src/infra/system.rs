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

#[cfg(windows)]
fn normalize_exclusion_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let no_verbatim = trimmed.strip_prefix(r"\\?\").unwrap_or(trimmed);
    no_verbatim
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

/// Pure check whether `target` matches or resides inside any exclusion path.
#[cfg(windows)]
pub(crate) fn path_is_excluded(target: &Path, exclusions: &[String]) -> bool {
    let target_norm = normalize_exclusion_path(&target.to_string_lossy());
    if target_norm.is_empty() {
        return false;
    }

    exclusions.iter().any(|entry| {
        let excl = normalize_exclusion_path(entry);
        if excl.is_empty() {
            return false;
        }
        target_norm == excl
            || target_norm
                .strip_prefix(&excl)
                .is_some_and(|rest| rest.starts_with('\\'))
    })
}

/// Read string value names (exclusion paths) from an HKLM registry key, returning
/// `None` if the key does not exist or cannot be opened (e.g. `ERROR_ACCESS_DENIED`).
#[cfg(windows)]
fn read_hklm_value_names(subkey: &str) -> Option<Vec<String>> {
    use windows_sys::Win32::Foundation::ERROR_NO_MORE_ITEMS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
    };

    const ERROR_MORE_DATA: u32 = 234;

    let wide_subkey: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            wide_subkey.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        )
    };
    if status != 0 {
        return None;
    }

    let mut names = Vec::new();
    let mut index: u32 = 0;
    let mut buf_len: usize = 512;

    let ok = loop {
        let mut buf = vec![0u16; buf_len];
        let mut len = buf.len() as u32;
        let rc = unsafe {
            RegEnumValueW(
                hkey,
                index,
                buf.as_mut_ptr(),
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc == 0 {
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
            index += 1;
        } else if rc == ERROR_MORE_DATA && buf_len < 32768 {
            buf_len *= 2;
        } else {
            break rc == ERROR_NO_MORE_ITEMS;
        }
    };

    unsafe {
        RegCloseKey(hkey);
    }

    ok.then_some(names)
}

/// Is `path` covered by a Windows Defender exclusion?
///
/// Queries the Defender registry exclusion keys first for sub-millisecond answers and
/// falls back to PowerShell `Get-MpPreference` when Tamper Protection restricts direct
/// registry read access to standard users. Returns `None` when the question could not be
/// answered.
pub fn check_defender_exclusion(path: &Path) -> Result<Option<bool>> {
    #[cfg(windows)]
    {
        let abs_path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());

        let policy_exclusions =
            read_hklm_value_names(r"SOFTWARE\Policies\Microsoft\Windows Defender\Exclusions\Paths")
                .unwrap_or_default();
        if path_is_excluded(&abs_path, &policy_exclusions) {
            return Ok(Some(true));
        }

        if let Some(mut main_exclusions) =
            read_hklm_value_names(r"SOFTWARE\Microsoft\Windows Defender\Exclusions\Paths")
        {
            main_exclusions.extend(policy_exclusions);
            return Ok(Some(path_is_excluded(&abs_path, &main_exclusions)));
        }

        let path_str = abs_path.to_str().unwrap_or(".");
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

/// Query `WinDefend` directly via Win32 SCM, returning `None` only when SCM could
/// not tell so the caller can fall back to PowerShell.
#[cfg(windows)]
fn query_windefend_scm() -> Option<bool> {
    use windows_sys::Win32::Foundation::{ERROR_SERVICE_DOES_NOT_EXIST, GetLastError};
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
    };

    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if scm.is_null() {
        return None;
    }

    let svc_name: Vec<u16> = "WinDefend\0".encode_utf16().collect();
    let svc = unsafe { OpenServiceW(scm, svc_name.as_ptr(), SERVICE_QUERY_STATUS) };
    if svc.is_null() {
        let err = unsafe { GetLastError() };
        unsafe {
            CloseServiceHandle(scm);
        }
        return if err == ERROR_SERVICE_DOES_NOT_EXIST {
            Some(false)
        } else {
            None
        };
    }

    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    let ok = unsafe { QueryServiceStatus(svc, &mut status) };
    unsafe {
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
    }

    if ok == 0 {
        None
    } else {
        Some(status.dwCurrentState == SERVICE_RUNNING)
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
        if let Some(running) = query_windefend_scm() {
            return Ok(Some(running));
        }

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

    #[cfg(windows)]
    #[test]
    fn path_is_excluded_matches_exact_subtree_and_ignores_sibling_prefix() {
        let exclusions = vec![r"C:\Users\Demo\scoop\".to_owned()];
        assert!(path_is_excluded(
            Path::new(r"C:\Users\Demo\scoop"),
            &exclusions
        ));
        assert!(path_is_excluded(
            Path::new(r"c:/users/demo/scoop/apps"),
            &exclusions
        ));
        assert!(
            !path_is_excluded(Path::new(r"C:\Users\Demo\scoop2"), &exclusions),
            "sibling directory sharing prefix must not match"
        );
        assert!(!path_is_excluded(Path::new(r"D:\scoop"), &exclusions));
    }
}
