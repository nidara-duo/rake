#![allow(unsafe_code)]

use crate::{Error, Result};
use async_trait::async_trait;
use windows_sys::Win32::System::Environment::SetEnvironmentVariableW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegSetValueExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};

#[async_trait]
pub trait EnvService: Send + Sync {
    fn add_path(&self, path: &str) -> Result<()>;
    fn remove_path(&self, path: &str) -> Result<()>;
    fn set_env(&self, key: &str, value: &str) -> Result<()>;
    fn remove_env(&self, key: &str) -> Result<()>;
}

pub struct WindowsEnvService;

impl Default for WindowsEnvService {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsEnvService {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EnvService for WindowsEnvService {
    fn add_path(&self, path: &str) -> Result<()> {
        let current_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{};{}", current_path, path);
        self.set_env("PATH", &new_path)
    }

    fn remove_path(&self, path: &str) -> Result<()> {
        let current_path = std::env::var("PATH").unwrap_or_default();
        let new_path = current_path
            .split(';')
            .filter(|p| p != &path)
            .collect::<Vec<_>>()
            .join(";");
        self.set_env("PATH", &new_path)
    }

    fn set_env(&self, key: &str, value: &str) -> Result<()> {
        // 1. Update current process
        let key_wide: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
        let value_wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            if SetEnvironmentVariableW(key_wide.as_ptr(), value_wide.as_ptr()) == 0 {
                return Err(Error::Custom(
                    "Failed to set env var in process".to_string(),
                ));
            }

            // 2. Update Registry (HKCU)
            let subkey: Vec<u16> = "Environment"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            ) != 0
            {
                return Err(Error::Custom("Failed to open registry key".to_string()));
            }

            let result = RegSetValueExW(
                hkey,
                key_wide.as_ptr(),
                0,
                REG_SZ,
                value_wide.as_ptr() as *const u8,
                reg_value_byte_len(&value_wide),
            );

            RegCloseKey(hkey);

            if result != 0 {
                return Err(Error::Custom("Failed to write to registry".to_string()));
            }

            // 3. Broadcast change
            let env_wide: Vec<u16> = "Environment"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut result_ptr: usize = 0;
            SendMessageTimeoutW(
                HWND_BROADCAST as _,
                WM_SETTINGCHANGE,
                0,
                env_wide.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result_ptr,
            );
        }
        Ok(())
    }

    fn remove_env(&self, key: &str) -> Result<()> {
        let key_wide: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let subkey: Vec<u16> = "Environment"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            ) != 0
            {
                return Err(Error::Custom("Failed to open registry key".to_string()));
            }
            let result = RegDeleteValueW(hkey, key_wide.as_ptr());
            RegCloseKey(hkey);

            if result != 0 && result != 2 {
                // 2 = ERROR_FILE_NOT_FOUND (value doesn't exist)
                return Err(Error::Custom("Failed to delete registry value".to_string()));
            }
        }
        Ok(())
    }
}

// ─── Persisted user PATH ──────────────────────────────────────────────────────
//
// These operate on HKCU\Environment\PATH directly, which is what survives a new
// login. Reading the process PATH instead would fold the machine-wide entries into
// the user's own value and persist that duplication.

/// Byte length to report for a NUL-terminated wide string's *data*.
///
/// `RegSetValueExW` expects the length of the text, not of the buffer. Passing the
/// buffer length stores the terminating NUL as part of the value, and reading it back
/// then yields a string with an invisible trailing `\0` — which silently defeats any
/// later comparison against the same path.
fn reg_value_byte_len(value_wide: &[u16]) -> u32 {
    let text_units = value_wide.len().saturating_sub(1);
    (text_units * 2) as u32
}

/// Tell running applications that the environment changed.
fn broadcast_env_change() {
    let env_wide: Vec<u16> = "Environment"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut result_ptr: usize = 0;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST as _,
            WM_SETTINGCHANGE,
            0,
            env_wide.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result_ptr,
        );
    }
}

/// Read the persisted user PATH together with its registry type.
///
/// The type is returned so it can be written back unchanged: `REG_EXPAND_SZ` holds
/// values like `%USERPROFILE%\bin`, and rewriting it as `REG_SZ` would leave those
/// entries unexpanded.
fn read_user_path_raw() -> Result<Option<(String, u32)>> {
    use windows_sys::Win32::System::Registry::{KEY_QUERY_VALUE, RegQueryValueExW};

    unsafe {
        let subkey: Vec<u16> = "Environment"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let value_name: Vec<u16> = "PATH".encode_utf16().chain(std::iter::once(0)).collect();

        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut hkey,
        ) != 0
        {
            return Ok(None);
        }

        let mut kind: u32 = 0;
        let mut bytes: u32 = 0;
        let queried = RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            std::ptr::null_mut(),
            &mut bytes,
        );
        if queried != 0 {
            RegCloseKey(hkey);
            return Ok(None);
        }

        // `bytes` excludes the terminating NUL, so reserve room for it.
        let mut buf = vec![0u16; (bytes as usize) / 2 + 1];
        let read = RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            buf.as_mut_ptr().cast(),
            &mut bytes,
        );
        RegCloseKey(hkey);

        if read != 0 {
            return Ok(None);
        }

        let len = (bytes as usize) / 2;
        let text = String::from_utf16_lossy(&buf[..len]);
        // A REG_SZ written with its terminator included comes back with a trailing NUL.
        // Other installers have been observed storing PATH that way too, so normalise it
        // away here rather than letting it poison every later comparison.
        Ok(Some((text.trim_end_matches('\0').to_string(), kind)))
    }
}

/// Write the persisted user PATH, preserving the registry type it had.
fn write_user_path_raw(value: &str, kind: u32) -> Result<()> {
    use windows_sys::Win32::System::Registry::{KEY_SET_VALUE, REG_EXPAND_SZ, RegSetValueExW};

    // An absent or non-string value would give us nothing to preserve; REG_EXPAND_SZ
    // is the safer default because Windows accepts it either way.
    let kind = if kind == REG_EXPAND_SZ || kind == 0 {
        kind
    } else {
        REG_SZ
    };

    unsafe {
        let subkey: Vec<u16> = "Environment"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let value_name: Vec<u16> = "PATH".encode_utf16().chain(std::iter::once(0)).collect();
        let value_wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();

        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        ) != 0
        {
            return Err(Error::Custom(
                "Failed to open HKCU\\Environment".to_string(),
            ));
        }

        let result = RegSetValueExW(
            hkey,
            value_name.as_ptr(),
            0,
            kind,
            value_wide.as_ptr() as *const u8,
            reg_value_byte_len(&value_wide),
        );
        RegCloseKey(hkey);

        if result != 0 {
            return Err(Error::Custom(
                "Failed to write HKCU\\Environment\\PATH".to_string(),
            ));
        }
    }

    broadcast_env_change();
    Ok(())
}

/// Split a PATH value into entries.
///
/// Empty entries left by stray separators are dropped, and embedded NULs are stripped:
/// a NUL-terminated value written by another installer would otherwise survive as an
/// entry that never matches anything, and would be carried forward on every write.
fn split_path(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(|e| e.trim_matches(|c: char| c == '\0' || c.is_whitespace()))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Windows paths are case-insensitive, so membership must be too.
///
/// Whitespace and a trailing separator are ignored as well: entries written by other
/// tools routinely carry a trailing space, and `C:\...\bin`, `C:\...\bin\` and
/// `C:\...\bin ` must all be recognised as the same directory, or the value grows a
/// duplicate on every run.
fn same_entry(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> &str {
        s.trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .trim_end_matches('\\')
    }
    norm(a).eq_ignore_ascii_case(norm(b))
}

/// Add `dir` to the persisted user PATH if it is not already there.
///
/// Returns whether the value changed, so callers can report honestly instead of
/// claiming success on a no-op.
pub fn add_user_path(dir: &std::path::Path) -> Result<bool> {
    let dir = dir.to_string_lossy().to_string();
    let (current, kind) = read_user_path_raw()?.unwrap_or_default();

    let mut entries = split_path(&current);
    if entries.iter().any(|e| same_entry(e, &dir)) {
        return Ok(false);
    }
    entries.push(dir);

    write_user_path_raw(&entries.join(";"), kind)?;
    Ok(true)
}

/// Remove `dir` from the persisted user PATH if present.
///
/// Returns whether the value changed.
pub fn remove_user_path(dir: &std::path::Path) -> Result<bool> {
    let dir = dir.to_string_lossy().to_string();
    let Some((current, kind)) = read_user_path_raw()? else {
        return Ok(false);
    };

    let entries = split_path(&current);
    let kept: Vec<String> = entries
        .iter()
        .filter(|e| !same_entry(e, &dir))
        .cloned()
        .collect();

    if kept.len() == entries.len() {
        return Ok(false);
    }

    write_user_path_raw(&kept.join(";"), kind)?;
    Ok(true)
}

#[cfg(test)]
mod path_tests {
    use super::*;

    /// Regression: writing the buffer length instead of the text length stored a
    /// trailing NUL, so the value read back was not equal to what was written and
    /// membership tests never matched.
    #[test]
    fn data_length_excludes_the_terminating_nul() {
        let value: Vec<u16> = r"C:\rake\bin"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        assert_eq!(
            reg_value_byte_len(&value),
            (r"C:\rake\bin".len() * 2) as u32
        );
        assert!(reg_value_byte_len(&value) < (value.len() * 2) as u32);
    }

    #[test]
    fn data_length_of_an_empty_value_is_zero() {
        assert_eq!(reg_value_byte_len(&[0u16]), 0);
        assert_eq!(reg_value_byte_len(&[]), 0);
    }

    #[test]
    fn membership_ignores_case_trailing_separator_and_whitespace() {
        assert!(same_entry(r"C:\Rake\bin", r"c:\rake\bin"));
        assert!(same_entry(r"C:\Rake\bin\", r"C:\Rake\bin"));
        assert!(same_entry("C:\\Rake\\bin ", r"C:\Rake\bin"));
        assert!(same_entry(r"C:\Rake\bin", " C:\\Rake\\bin\\ "));
        assert!(!same_entry(r"C:\Rake\bin", r"C:\Rake\bins"));
    }

    /// Duplicate entries already present in the value must collapse to one, otherwise
    /// every run appends another and the list grows without bound.
    #[test]
    fn collapses_existing_duplicates() {
        let stored = r"C:\a;;C:\Rake\bin;C:\Rake\bin ;C:\Rake\bin\;C:\b";
        let kept: Vec<String> = split_path(stored)
            .into_iter()
            .filter(|e| !same_entry(e, r"C:\rake\bin"))
            .collect();
        assert_eq!(kept, vec![r"C:\a".to_string(), r"C:\b".to_string()]);
    }

    #[test]
    fn split_drops_empty_entries_from_stray_separators() {
        assert_eq!(
            split_path(r"C:\a;;C:\b;"),
            vec![r"C:\a".to_string(), r"C:\b".to_string()]
        );
        assert!(split_path("").is_empty());
        assert!(split_path(";;").is_empty());
    }

    /// Another installer left a NUL-terminated PATH behind. It must neither survive as a
    /// phantom entry nor defeat the membership test for a real one.
    #[test]
    fn strips_nuls_written_by_other_tools() {
        assert_eq!(
            split_path("C:\\a\0;C:\\b"),
            vec!["C:\\a".to_string(), "C:\\b".to_string()]
        );
        assert!(split_path("\0\0\0").is_empty());
        assert!(same_entry("C:\\Rake\\bin\0", r"C:\Rake\bin"));
    }
}
