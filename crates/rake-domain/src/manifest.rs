use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::arch::Arch;
use crate::one_or_many::OneOrMany;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<License>,
    pub url: Option<OneOrMany<String>>,
    pub hash: Option<OneOrMany<String>>,
    pub architecture: Option<ArchitectureMap>,
    pub depends: Option<OneOrMany<String>>,
    pub bin: Option<OneOrMany<OneOrMany<String>>>,
    pub extract_dir: Option<OneOrMany<String>>,
    pub extract_to: Option<OneOrMany<String>>,
    pub persist: Option<OneOrMany<OneOrMany<String>>>,
    pub env_add_path: Option<OneOrMany<String>>,
    pub env_set: Option<HashMap<String, String>>,
    pub shortcuts: Option<Vec<Vec<String>>>,
    pub innosetup: Option<bool>,
    pub checkver: Option<serde_json::Value>,
    pub autoupdate: Option<serde_json::Value>,
    pub pre_install: Option<OneOrMany<String>>,
    pub post_install: Option<OneOrMany<String>>,
    pub pre_uninstall: Option<OneOrMany<String>>,
    pub post_uninstall: Option<OneOrMany<String>>,
    pub installer: Option<InstallerSpec>,
    pub uninstaller: Option<UninstallerSpec>,
    pub cookie: Option<HashMap<String, String>>,
    pub notes: Option<OneOrMany<String>>,
    pub suggest: Option<HashMap<String, OneOrMany<String>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionManifest {
    pub version: String,
}

impl Manifest {
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn homepage(&self) -> Option<&str> {
        self.homepage.as_deref()
    }

    pub fn arch_spec(&self, arch: Arch) -> Option<&ArchSpec> {
        self.architecture.as_ref().and_then(|m| match arch {
            Arch::Amd64 => m.amd64.as_ref(),
            Arch::Ia32 => m.ia32.as_ref(),
            Arch::Aarch64 => m.aarch64.as_ref(),
        })
    }

    pub fn resolve_extract_dir(&self, arch: Arch) -> Option<&OneOrMany<String>> {
        self.arch_spec(arch)
            .and_then(|s| s.extract_dir.as_ref())
            .or(self.extract_dir.as_ref())
    }

    pub fn resolve_bin(&self, arch: Arch) -> Option<&OneOrMany<OneOrMany<String>>> {
        self.arch_spec(arch)
            .and_then(|s| s.bin.as_ref())
            .or(self.bin.as_ref())
    }

    /// Resolve hashes for the given arch, returning them in URL order.
    pub fn resolve_hashes(&self, arch: Arch) -> Option<Vec<String>> {
        let hashes = self
            .arch_spec(arch)
            .and_then(|s| s.hash.as_ref())
            .or(self.hash.as_ref())?;
        Some(hashes.as_slice().to_vec())
    }

    pub fn resolve_shortcuts(&self, arch: Arch) -> Option<&Vec<Vec<String>>> {
        self.arch_spec(arch)
            .and_then(|s| s.shortcuts.as_ref())
            .or(self.shortcuts.as_ref())
    }

    pub fn resolve_env_add_path(&self, arch: Arch) -> Option<&OneOrMany<String>> {
        self.arch_spec(arch)
            .and_then(|s| s.env_add_path.as_ref())
            .or(self.env_add_path.as_ref())
    }

    pub fn resolve_extract_to(&self, arch: Arch) -> Option<&OneOrMany<String>> {
        self.arch_spec(arch)
            .and_then(|s| s.extract_to.as_ref())
            .or(self.extract_to.as_ref())
    }

    pub fn resolve_pre_install(&self, arch: Arch) -> Option<&OneOrMany<String>> {
        self.arch_spec(arch)
            .and_then(|s| s.pre_install.as_ref())
            .or(self.pre_install.as_ref())
    }

    pub fn resolve_post_install(&self, arch: Arch) -> Option<&OneOrMany<String>> {
        self.arch_spec(arch)
            .and_then(|s| s.post_install.as_ref())
            .or(self.post_install.as_ref())
    }

    pub fn resolve_pre_uninstall(&self, arch: Arch) -> Option<&OneOrMany<String>> {
        self.arch_spec(arch)
            .and_then(|s| s.pre_uninstall.as_ref())
            .or(self.pre_uninstall.as_ref())
    }

    pub fn resolve_post_uninstall(&self, arch: Arch) -> Option<&OneOrMany<String>> {
        self.arch_spec(arch)
            .and_then(|s| s.post_uninstall.as_ref())
            .or(self.post_uninstall.as_ref())
    }

    pub fn resolve_env_set(
        &self,
        arch: Arch,
    ) -> Option<&std::collections::HashMap<String, String>> {
        self.arch_spec(arch)
            .and_then(|s| s.env_set.as_ref())
            .or(self.env_set.as_ref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum License {
    Identifier(String),
    Full {
        identifier: String,
        url: Option<String>,
    },
}

impl License {
    pub fn identifier(&self) -> &str {
        match self {
            License::Identifier(s) => s,
            License::Full { identifier, .. } => identifier,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureMap {
    #[serde(rename = "32bit")]
    pub ia32: Option<ArchSpec>,
    #[serde(rename = "64bit")]
    pub amd64: Option<ArchSpec>,
    #[serde(rename = "arm64")]
    pub aarch64: Option<ArchSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchSpec {
    pub url: Option<OneOrMany<String>>,
    pub hash: Option<OneOrMany<String>>,
    pub bin: Option<OneOrMany<OneOrMany<String>>>,
    pub extract_dir: Option<OneOrMany<String>>,
    pub extract_to: Option<OneOrMany<String>>,
    pub env_add_path: Option<OneOrMany<String>>,
    pub env_set: Option<HashMap<String, String>>,
    pub installer: Option<InstallerSpec>,
    pub uninstaller: Option<UninstallerSpec>,
    pub shortcuts: Option<Vec<Vec<String>>>,
    pub pre_install: Option<OneOrMany<String>>,
    pub post_install: Option<OneOrMany<String>>,
    pub pre_uninstall: Option<OneOrMany<String>>,
    pub post_uninstall: Option<OneOrMany<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallerSpec {
    pub args: Option<OneOrMany<String>>,
    pub file: Option<String>,
    pub script: Option<OneOrMany<String>>,
    pub keep: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallerSpec {
    pub args: Option<OneOrMany<String>>,
    pub file: Option<String>,
    pub script: Option<OneOrMany<String>>,
}
