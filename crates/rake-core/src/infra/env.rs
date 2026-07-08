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
                (value_wide.len() * 2) as u32,
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
