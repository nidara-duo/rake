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

    /// Resolve the `installer` block, preferring the architecture-specific one.
    ///
    /// Scoop reads this through `arch_specific 'installer'`, so a manifest may
    /// override the installer hook per architecture.
    pub fn resolve_installer(&self, arch: Arch) -> Option<&InstallerSpec> {
        self.arch_spec(arch)
            .and_then(|s| s.installer.as_ref())
            .or(self.installer.as_ref())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Manifest {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("manifest should parse: {e}\n{json}"))
    }

    /// A manifest is untrusted input from a bucket, and every field is optional except
    /// `version`. These tests pin the parsing and the architecture fallback, because a
    /// silent mistake there produces wrong downloads rather than an error.
    #[test]
    fn version_only_is_the_minimum_valid_manifest() {
        let m = parse(r#"{"version":"1.2.3"}"#);
        assert_eq!(m.version(), "1.2.3");
        assert!(m.description.is_none());
        assert!(m.url.is_none());
        assert!(m.homepage().is_none());
    }

    #[test]
    fn architecture_keys_are_scoped_the_scoop_way() {
        // Scoop writes "64bit"/"32bit"/"arm64", not "amd64"/"ia32"/"aarch64".
        let m = parse(
            r#"{"version":"1","architecture":{
                 "64bit":{"url":"https://x/win.zip"},
                 "32bit":{"url":"https://x/win32.zip"},
                 "arm64":{"url":"https://x/winarm.zip"}}}"#,
        );
        assert_eq!(
            m.arch_spec(Arch::Amd64)
                .unwrap()
                .url
                .as_ref()
                .unwrap()
                .as_slice(),
            ["https://x/win.zip"]
        );
        assert_eq!(
            m.arch_spec(Arch::Ia32)
                .unwrap()
                .url
                .as_ref()
                .unwrap()
                .as_slice(),
            ["https://x/win32.zip"]
        );
        assert_eq!(
            m.arch_spec(Arch::Aarch64)
                .unwrap()
                .url
                .as_ref()
                .unwrap()
                .as_slice(),
            ["https://x/winarm.zip"]
        );
    }

    #[test]
    fn arch_spec_is_none_when_the_manifest_has_no_architecture() {
        let m = parse(r#"{"version":"1","url":"https://x/a.zip"}"#);
        assert!(m.arch_spec(Arch::Amd64).is_none());
    }

    #[test]
    fn arch_spec_is_none_for_an_arch_the_manifest_omits() {
        let m = parse(r#"{"version":"1","architecture":{"64bit":{"url":"https://x/a.zip"}}}"#);
        assert!(m.arch_spec(Arch::Amd64).is_some());
        assert!(m.arch_spec(Arch::Ia32).is_none());
    }

    /// The fallback every resolver relies on: a missing architecture-specific value
    /// falls through to the top-level one rather than yielding nothing.
    #[test]
    fn resolvers_fall_back_to_the_top_level_value() {
        let m = parse(
            r#"{
            "version":"1",
            "extract_dir":"top",
            "extract_to":"top-out",
            "bin":"top.exe",
            "env_add_path":"top-path",
            "pre_install":"top-pre",
            "post_install":"top-post",
            "pre_uninstall":"top-preun",
            "post_uninstall":"top-postun",
            "hash":"tophash",
            "shortcuts":[["a","b"]],
            "env_set":{"TOP":"1"}
        }"#,
        );

        assert_eq!(
            m.resolve_extract_dir(Arch::Amd64).unwrap().as_slice(),
            ["top"]
        );
        assert_eq!(
            m.resolve_extract_to(Arch::Amd64).unwrap().as_slice(),
            ["top-out"]
        );
        assert!(m.resolve_bin(Arch::Amd64).is_some());
        assert_eq!(
            m.resolve_env_add_path(Arch::Amd64).unwrap().as_slice(),
            ["top-path"]
        );
        assert!(m.resolve_pre_install(Arch::Amd64).is_some());
        assert!(m.resolve_post_install(Arch::Amd64).is_some());
        assert!(m.resolve_pre_uninstall(Arch::Amd64).is_some());
        assert!(m.resolve_post_uninstall(Arch::Amd64).is_some());
        assert_eq!(m.resolve_hashes(Arch::Amd64).unwrap(), ["tophash"]);
        assert!(m.resolve_shortcuts(Arch::Amd64).is_some());
        assert!(m.resolve_env_set(Arch::Amd64).is_some());
    }

    /// And the architecture-specific value wins when present.
    #[test]
    fn resolvers_prefer_the_architecture_specific_value() {
        let m = parse(
            r#"{
            "version":"1",
            "extract_dir":"top",
            "hash":"tophash",
            "bin":"top.exe",
            "architecture":{"64bit":{"extract_dir":"arch","hash":"archhash","bin":"arch.exe"}}
        }"#,
        );

        assert_eq!(
            m.resolve_extract_dir(Arch::Amd64).unwrap().as_slice(),
            ["arch"]
        );
        assert_eq!(m.resolve_hashes(Arch::Amd64).unwrap(), ["archhash"]);

        // A different architecture still sees the top-level value.
        assert_eq!(
            m.resolve_extract_dir(Arch::Ia32).unwrap().as_slice(),
            ["top"]
        );
        assert_eq!(m.resolve_hashes(Arch::Ia32).unwrap(), ["tophash"]);
    }

    /// Hashes are positional: they must come back in URL order, because download
    /// matches `hashes[i]` to `urls[i]`. Reordering them silently installs a file
    /// whose hash belongs to a different mirror.
    #[test]
    fn hashes_keep_url_order() {
        let m = parse(
            r#"{
            "version":"1",
            "url":["https://a/1.zip","https://b/2.zip","https://c/3.zip"],
            "hash":["h1","h2","h3"]
        }"#,
        );
        assert_eq!(m.resolve_hashes(Arch::Amd64).unwrap(), ["h1", "h2", "h3"]);

        let m = parse(
            r#"{
            "version":"1",
            "architecture":{"64bit":{
                "url":["https://a/1.zip","https://b/2.zip"],
                "hash":["x1","x2"]}}
        }"#,
        );
        assert_eq!(m.resolve_hashes(Arch::Amd64).unwrap(), ["x1", "x2"]);
    }

    #[test]
    fn a_single_url_and_hash_are_accepted_without_arrays() {
        let m = parse(r#"{"version":"1","url":"https://a/1.zip","hash":"h1"}"#);
        assert_eq!(m.resolve_hashes(Arch::Amd64).unwrap(), ["h1"]);
    }

    #[test]
    fn resolvers_return_none_when_absent_everywhere() {
        let m = parse(r#"{"version":"1","url":"https://a/1.zip"}"#);
        assert!(m.resolve_extract_dir(Arch::Amd64).is_none());
        assert!(m.resolve_hashes(Arch::Amd64).is_none());
        assert!(m.resolve_env_set(Arch::Amd64).is_none());
        assert!(m.resolve_shortcuts(Arch::Amd64).is_none());
        assert!(m.resolve_installer(Arch::Amd64).is_none());
        assert!(m.resolve_bin(Arch::Amd64).is_none());
    }

    /// A hash spec may carry an algorithm prefix, which is the form the download
    /// verifier has to understand. Parsing must not choke on it.
    #[test]
    fn prefixed_hash_specs_parse() {
        let m = parse(r#"{"version":"1","hash":["sha512:abc","plain64hex"]}"#);
        assert_eq!(m.resolve_hashes(Arch::Amd64).unwrap().len(), 2);
    }

    #[test]
    fn installer_can_be_overridden_per_architecture() {
        let m = parse(
            r#"{
            "version":"1",
            "installer":{"file":"top.exe","args":"/top"},
            "architecture":{"64bit":{"installer":{"file":"arch.exe"}}}
        }"#,
        );

        assert_eq!(
            m.resolve_installer(Arch::Amd64).unwrap().file.as_deref(),
            Some("arch.exe")
        );
        assert_eq!(
            m.resolve_installer(Arch::Ia32).unwrap().file.as_deref(),
            Some("top.exe")
        );
    }

    #[test]
    fn installer_script_survives_parsing() {
        let m = parse(
            r#"{"version":"1","installer":{"script":["Add-Path -Path \"$persist_dir\\bin\" -Global:$global"]}}"#,
        );
        let script = m
            .resolve_installer(Arch::Amd64)
            .unwrap()
            .script
            .as_ref()
            .unwrap();
        assert!(
            script
                .as_slice()
                .iter()
                .any(|line| line.contains("Add-Path")),
            "the hook body must survive verbatim"
        );
    }

    #[test]
    fn notes_accept_a_string_or_an_array() {
        let one = parse(r#"{"version":"1","notes":"only line"}"#);
        assert_eq!(one.notes.as_ref().unwrap().as_slice().len(), 1);

        let many = parse(r#"{"version":"1","notes":["one","two","three"]}"#);
        assert_eq!(many.notes.as_ref().unwrap().as_slice().len(), 3);
    }

    /// Scoop allows a licence as a bare identifier or as an object with a URL, and
    /// `identifier()` has to yield the same string for both.
    #[test]
    fn license_identifier_is_read_from_both_shapes() {
        let bare = parse(r#"{"version":"1","license":"MIT"}"#);
        assert_eq!(bare.license.as_ref().unwrap().identifier(), "MIT");

        let full =
            parse(r#"{"version":"1","license":{"identifier":"Apache-2.0","url":"https://x"}}"#);
        assert_eq!(full.license.as_ref().unwrap().identifier(), "Apache-2.0");
    }

    #[test]
    fn arch_map_ignores_unknown_architecture_keys() {
        // A future Scoop architecture must not break parsing of the ones we know.
        let m = parse(
            r#"{
            "version":"1",
            "architecture":{"64bit":{"url":"https://x/a.zip"},"sparc":{"url":"https://x/s.zip"}}
        }"#,
        );
        assert!(m.arch_spec(Arch::Amd64).is_some());
        assert_eq!(m.resolve_hashes(Arch::Amd64), None);
    }

    #[test]
    fn a_manifest_without_version_is_rejected() {
        assert!(serde_json::from_str::<Manifest>(r#"{"description":"no version"}"#).is_err());
    }
}
