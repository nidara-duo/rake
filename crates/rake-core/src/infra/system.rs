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

pub fn check_defender_exclusion(path: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        let path_str = path.to_str().unwrap_or(".");
        let escaped_path = path_str.replace('\'', "''");
        let script = format!(
            "try {{$c=[System.IO.Path]::GetFullPath('{p}').TrimEnd('\\').TrimEnd('/');$e=@((Get-MpPreference).ExclusionPath);if($e.Count -eq 0 -or ($e.Count -eq 1 -and $null -eq $e[0])){{'TRUE';return}}foreach($x in $e){{if($null -eq $x){{continue}}$n=[System.IO.Path]::GetFullPath($x).TrimEnd('\\').TrimEnd('/');if($c -eq $n){{'TRUE';return}}if($c.StartsWith($n+'\\',[StringComparison]::OrdinalIgnoreCase)){{'TRUE';return}}}}'FALSE'}}catch{{'TRUE'}}",
            p = escaped_path
        );

        let output = match std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
        {
            Ok(o) => o,
            Err(_) => return Ok(true),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(stdout == "TRUE")
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(true)
    }
}

pub fn is_windows_defender_running() -> Result<bool> {
    #[cfg(windows)]
    {
        let output = match std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "try { (Get-Service -Name WinDefend -ErrorAction SilentlyContinue).Status -eq 'Running' } catch { $false }",
            ])
            .output()
        {
            Ok(o) => o,
            Err(_) => return Ok(false),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(stdout == "True")
    }

    #[cfg(not(windows))]
    {
        Ok(false)
    }
}
