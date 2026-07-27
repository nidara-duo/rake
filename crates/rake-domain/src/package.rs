use serde::{Deserialize, Serialize};

use crate::manifest::Manifest;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageIdent {
    pub bucket: String,
    pub name: String,
}

impl PackageIdent {
    pub fn new(bucket: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            name: name.into(),
        }
    }

    pub fn as_str(&self) -> String {
        format!("{}/{}", self.bucket, self.name)
    }
}

impl std::fmt::Display for PackageIdent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.bucket, self.name)
    }
}

#[derive(Debug, Clone)]
pub enum PackageSource {
    Bucket(String),
    File(String),
}

#[derive(Debug, Clone)]
pub struct InstallState {
    pub version: String,
    pub bucket: Option<String>,
    pub arch: String,
    pub held: bool,
    pub url: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PackageStatus {
    NotInstalled,
    Installed(InstallState),
}

impl PackageStatus {
    pub fn version(&self) -> Option<&str> {
        match self {
            PackageStatus::Installed(s) => Some(&s.version),
            PackageStatus::NotInstalled => None,
        }
    }

    pub fn is_installed(&self) -> bool {
        matches!(self, PackageStatus::Installed(_))
    }

    pub fn is_held(&self) -> bool {
        match self {
            PackageStatus::Installed(s) => s.held,
            PackageStatus::NotInstalled => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Package {
    pub ident: PackageIdent,
    pub manifest: Manifest,
    pub source: Option<PackageSource>,
    pub status: PackageStatus,
}

impl Package {
    pub fn new(
        ident: PackageIdent,
        manifest: Manifest,
        source: Option<PackageSource>,
        status: PackageStatus,
    ) -> Self {
        Self {
            ident,
            manifest,
            source,
            status,
        }
    }

    pub fn name(&self) -> &str {
        &self.ident.name
    }

    pub fn bucket(&self) -> &str {
        &self.ident.bucket
    }

    pub fn version(&self) -> &str {
        self.manifest.version()
    }

    pub fn is_nightly(&self) -> bool {
        self.version() == "nightly"
    }

    pub fn homepage(&self) -> Option<&str> {
        self.manifest.homepage()
    }
}

/// The on-disk `install.json` record written into each version directory.
///
/// IMPORTANT — treat this as a **stable external ABI**, not an internal
/// data structure.  The semantics of every field are defined by the
/// official Scoop (PowerShell) implementation.  Rake must produce files
/// that are indistinguishable from Scoop's output whenever possible.
///
/// Key Scoop compatibility rules:
///
/// * `url` — holds the **manifest location**, not the downloaded archive.
///   Scoop's `manifest()` function (lib/manifest.ps1) gives `url`
///   unconditional precedence over `bucket`: if `url` is non-null, Scoop
///   fetches it and parses it as a manifest JSON.  Therefore:
///   - For bucket-sourced packages: `url` MUST be null/absent.
///   - For URL-sourced packages: `url` MUST be the manifest URL.
///   - Binary/artifact download URLs MUST NEVER appear here.
///
/// * `bucket` — set for bucket-based installs, null/absent for URL-sourced
///   or local-manifest installs.
///
/// * `arch` — serialised as `"architecture"` (Scoop's key).  `alias`
///   on the Rust field provides backward-compat reading of `"arch"`.
///
/// * `held` — serialised as `"hold"` (Scoop's key).  `alias` provides
///   backward-compat reading of `"held"`.
///
/// **Serialisation contract** (serde attributes below enforce this):
/// - Fields with `skip_serializing_if` MUST NOT appear in JSON when empty.
///   Scoop skips null fields entirely — `"url": null` is NOT valid Scoop.
/// - `#[serde(default)]` ensures missing fields decode as the zero value.
///
/// This is the **single canonical** definition.  Previously, four
/// near-identical `InstallInfo` structs existed (in `install`, `query`,
/// `hold`, `reset`, `uninstall`) with subtly diverging field names and
/// sets — most critically, `install.rs` wrote the arch under key `arch`
/// while `query.rs` read it as `architecture`, so the installed arch and
/// (worse) the `held` flag were silently lost on every read.  `hold.rs`
/// additionally re-serialised from a struct lacking `url`, deleting that
/// field on hold.  Consolidating here makes that class of drift impossible.
///
/// There must be exactly **one** canonical writer of `InstallRecord`.
/// Every other operation should preserve the existing record by
/// read-modify-write rather than rebuilding it from scratch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    #[serde(default)]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(default, alias = "arch", rename = "architecture")]
    pub arch: String,
    #[serde(default, alias = "held", rename = "hold")]
    pub held: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl InstallRecord {
    /// Parse the canonical `arch` string back into [`Arch`], with the same
    /// spelling variants Scoop manifests may have stored historically.
    pub fn arch_enum(&self) -> crate::arch::Arch {
        match self.arch.to_ascii_lowercase().as_str() {
            "32bit" | "x86" | "i386" | "i686" => crate::arch::Arch::Ia32,
            "64bit" | "x86_64" | "amd64" | "x64" => crate::arch::Arch::Amd64,
            "arm64" | "aarch64" => crate::arch::Arch::Aarch64,
            _ => crate::arch::Arch::current(),
        }
    }
}
