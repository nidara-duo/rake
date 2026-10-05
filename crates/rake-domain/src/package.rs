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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::Arch;

    /// The on-disk record is a stable ABI shared with Scoop, and real machines contain
    /// every historical spelling. Getting one wrong silently installs the wrong
    /// architecture's files, so the whole table is pinned here.
    #[test]
    fn arch_enum_reads_every_historical_spelling() {
        for (text, expected) in [
            ("32bit", Arch::Ia32),
            ("x86", Arch::Ia32),
            ("i386", Arch::Ia32),
            ("i686", Arch::Ia32),
            ("64bit", Arch::Amd64),
            ("x86_64", Arch::Amd64),
            ("amd64", Arch::Amd64),
            ("x64", Arch::Amd64),
            ("arm64", Arch::Aarch64),
            ("aarch64", Arch::Aarch64),
        ] {
            let record = InstallRecord {
                version: "1".to_owned(),
                bucket: None,
                arch: text.to_owned(),
                held: false,
                url: None,
            };
            assert_eq!(record.arch_enum(), expected, "arch {text}");
        }
    }

    #[test]
    fn arch_enum_is_case_insensitive() {
        for (text, expected) in [
            ("64BIT", Arch::Amd64),
            ("X86_64", Arch::Amd64),
            ("AmD64", Arch::Amd64),
            ("32BIT", Arch::Ia32),
            ("X86", Arch::Ia32),
            ("ARM64", Arch::Aarch64),
            ("AaRcH64", Arch::Aarch64),
        ] {
            let record = InstallRecord {
                version: "1".to_owned(),
                bucket: None,
                arch: text.to_owned(),
                held: false,
                url: None,
            };
            assert_eq!(record.arch_enum(), expected, "arch {text}");
        }
    }

    /// An unrecognised value falls back to the running machine rather than guessing
    /// 64-bit, so a future Scoop spelling does not break installs.
    #[test]
    fn unknown_arch_falls_back_to_the_current_machine() {
        let record = InstallRecord {
            version: "1".to_owned(),
            bucket: None,
            arch: "riscv64".to_owned(),
            held: false,
            url: None,
        };
        assert_eq!(record.arch_enum(), Arch::current());
    }

    /// Scoop writes `architecture` and `hold`; older rake builds wrote `arch` and
    /// `held`. Both spellings must decode, because both are on disk right now.
    #[test]
    fn scoop_key_names_decode() {
        let r: InstallRecord =
            serde_json::from_str(r#"{"version":"1","architecture":"64bit","hold":true}"#).unwrap();
        assert_eq!(r.arch, "64bit");
        assert!(r.held);
    }

    #[test]
    fn legacy_key_names_decode() {
        let r: InstallRecord =
            serde_json::from_str(r#"{"version":"1","arch":"32bit","held":true}"#).unwrap();
        assert_eq!(r.arch, "32bit");
        assert!(r.held);
    }

    #[test]
    fn absent_fields_default_rather_than_fail() {
        let r: InstallRecord = serde_json::from_str(r#"{"version":"1"}"#).unwrap();
        assert_eq!(r.version, "1");
        assert_eq!(r.arch, "");
        assert!(!r.held);
        assert!(r.bucket.is_none());
        assert!(r.url.is_none());
    }

    /// Null-valued keys are absent, not null: Scoop strips them, and a literal `"url":
    /// null` in the wild must not turn into Some("null").
    #[test]
    fn null_url_is_treated_as_absent() {
        let r: InstallRecord =
            serde_json::from_str(r#"{"version":"1","architecture":"64bit","url":null}"#).unwrap();
        assert!(r.url.is_none());
    }

    /// `url` is the manifest location, never the archive. A bucket-sourced install must
    /// not carry one, because Scoop's `manifest()` prefers it over the bucket.
    #[test]
    fn bucket_install_round_trips_without_a_url() {
        let record = InstallRecord {
            version: "1.2.3".to_owned(),
            bucket: Some("main".to_owned()),
            arch: "64bit".to_owned(),
            held: false,
            url: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            !json.contains("\"url\""),
            "empty url must be omitted: {json}"
        );

        let back: InstallRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bucket.as_deref(), Some("main"));
        assert!(back.url.is_none());
    }

    #[test]
    fn ident_display_is_bucket_slash_name() {
        let ident = PackageIdent::new("main", "git");
        assert_eq!(ident.as_str(), "main/git");
        assert_eq!(ident.to_string(), "main/git");
    }

    fn manifest() -> Manifest {
        serde_json::from_str(r#"{"version":"1.2.3"}"#).unwrap()
    }

    #[test]
    fn installed_package_exposes_its_version() {
        let pkg = Package::new(
            PackageIdent::new("main", "git"),
            manifest(),
            Some(PackageSource::Bucket("main".to_owned())),
            PackageStatus::Installed(InstallState {
                version: "1.2.3".to_owned(),
                bucket: Some("main".to_owned()),
                arch: "64bit".to_owned(),
                held: false,
                url: None,
            }),
        );

        assert!(pkg.status.is_installed());
        assert_eq!(pkg.version(), "1.2.3");
        assert_eq!(pkg.status.version(), Some("1.2.3"));
        assert_eq!(pkg.name(), "git");
        assert_eq!(pkg.bucket(), "main");
        assert!(!pkg.status.is_held());
        assert!(!pkg.is_nightly());
        assert_eq!(pkg.homepage(), None);
    }

    #[test]
    fn not_installed_package_has_no_version() {
        let pkg = Package::new(
            PackageIdent::new("main", "git"),
            manifest(),
            Some(PackageSource::Bucket("main".to_owned())),
            PackageStatus::NotInstalled,
        );
        assert!(!pkg.status.is_installed());
        assert_eq!(pkg.status.version(), None);
        assert!(!pkg.status.is_held());
    }

    /// `nightly` is a directory name rather than a version, so it needs its own marker.
    #[test]
    fn nightly_is_recognised() {
        let pkg = Package::new(
            PackageIdent::new("main", "vscode"),
            serde_json::from_str(r#"{"version":"nightly"}"#).unwrap(),
            None,
            PackageStatus::Installed(InstallState {
                version: "nightly".to_owned(),
                bucket: None,
                arch: "64bit".to_owned(),
                held: false,
                url: None,
            }),
        );
        assert!(pkg.is_nightly());
    }

    #[test]
    fn held_install_reports_held() {
        let pkg = Package::new(
            PackageIdent::new("main", "pinned"),
            manifest(),
            None,
            PackageStatus::Installed(InstallState {
                version: "1.2.3".to_owned(),
                bucket: Some("main".to_owned()),
                arch: "64bit".to_owned(),
                held: true,
                url: None,
            }),
        );
        assert!(pkg.status.is_held());
    }
}
