use std::path::PathBuf;

use rake_domain::package::Package;

use crate::infra::install_meta;
use crate::session::Session;

/// The directories one install batch writes to, resolved once.
///
/// Passed around as one value because every step needs a different subset
/// of them, and a seven-argument parameter list on each is where a mix-up lives.
#[derive(Debug, Clone)]
pub struct InstallRoots {
    /// `$SCOOP` — also the base the PowerShell library is loaded from.
    pub root: PathBuf,
    /// Directory containing installed applications (`<root>/apps`).
    pub apps: PathBuf,
    /// Directory containing command shims (`<root>/shims`).
    pub shims: PathBuf,
    /// Directory containing persisted application data (`<root>/persist`).
    pub persist: PathBuf,
    /// Directory containing PowerShell module links (`<root>/modules`).
    pub modules: PathBuf,
    /// Global Scoop root directory (`C:\ProgramData\rake` by default).
    pub global: PathBuf,
}

impl InstallRoots {
    /// Resolve installation roots from the active [`Session`] configuration.
    pub fn from_session(session: &Session) -> Self {
        let root = session
            .config()
            .root_path
            .as_ref()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("apps"));
        let global = session
            .config()
            .global_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\rake"));

        Self {
            apps: root.join("apps"),
            shims: root.join("shims"),
            persist: root.join("persist"),
            modules: root.join("modules"),
            root,
            global,
        }
    }

    /// Build the [`AppLayout`] for a single application name under these roots.
    pub fn app_layout(&self, app_name: &str) -> AppLayout {
        AppLayout::new(self, app_name)
    }
}

/// Canonical directory layout for a single application under `<root>/apps/<app>`.
#[derive(Debug, Clone)]
pub struct AppLayout {
    /// Application name.
    pub app: String,
    /// Application root directory (`<root>/apps/<app>`).
    pub app_dir: PathBuf,
    /// Active version junction path (`<root>/apps/<app>/current`).
    pub current_dir: PathBuf,
    /// Persisted data directory (`<root>/persist/<app>`).
    pub persist_dir: PathBuf,
}

impl AppLayout {
    /// Construct an [`AppLayout`] for `app` under `roots`.
    pub fn new(roots: &InstallRoots, app: &str) -> Self {
        let app_dir = roots.apps.join(app);
        Self {
            app: app.to_owned(),
            current_dir: app_dir.join("current"),
            app_dir,
            persist_dir: roots.persist.join(app),
        }
    }

    /// The version directory name for a package — `"nightly"` for nightlies,
    /// which reuse one directory instead of accumulating versions.
    pub fn version_component(pkg: &Package) -> String {
        if pkg.is_nightly() {
            "nightly".to_owned()
        } else {
            pkg.version().to_owned()
        }
    }

    /// Return the directory path for a specific version component.
    pub fn version_dir(&self, version: &str) -> PathBuf {
        self.app_dir.join(version)
    }

    /// Return the version directory path for `pkg`.
    pub fn pkg_version_dir(&self, pkg: &Package) -> PathBuf {
        self.version_dir(&Self::version_component(pkg))
    }

    /// Return the active application directory: `<version_dir>` when
    /// `no_junction` is enabled, or `current` otherwise.
    pub fn active_dir(&self, no_junction: bool, version: &str) -> PathBuf {
        if no_junction {
            self.version_dir(version)
        } else {
            self.current_dir.clone()
        }
    }

    /// Resolve the installed version directory on disk for this application.
    ///
    /// Resolution order:
    /// 1. `expected_version` when provided and its directory exists;
    /// 2. the leaf directory name of the `current` junction target (normalizing
    ///    Windows `\\?\` verbatim prefixes);
    /// 3. the `version` recorded in the installed manifest under `current`;
    /// 4. [`install_meta::fallback_version`] when that directory exists.
    pub fn resolve_installed_version_dir(&self, expected_version: Option<&str>) -> Option<PathBuf> {
        if let Some(ver) = expected_version {
            let dir = self.version_dir(ver);
            if dir.exists() {
                return Some(dir);
            }
        }

        if let Ok(target) = std::fs::read_link(&self.current_dir)
            && let Some(leaf) = target.file_name()
        {
            let dir = self.app_dir.join(leaf);
            if dir.exists() {
                return Some(dir);
            }
        }

        if let Ok(Some(manifest)) = install_meta::read_installed_manifest(&self.current_dir) {
            let dir = self.version_dir(manifest.version());
            if dir.exists() {
                return Some(dir);
            }
        }

        install_meta::fallback_version(&self.app_dir, &self.current_dir)
            .map(|ver| self.version_dir(&ver))
            .filter(|dir| dir.exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rake_domain::config::Config;
    use rake_domain::manifest::Manifest;
    use rake_domain::package::{PackageIdent, PackageStatus};

    fn test_roots(root: &std::path::Path) -> InstallRoots {
        let (session, _) = Session::from_config_recording_env(Config {
            root_path: Some(root.to_path_buf()),
            ..Default::default()
        });
        InstallRoots::from_session(&session)
    }

    fn pkg_with_version(version: &str) -> Package {
        let manifest: Manifest =
            serde_json::from_value(serde_json::json!({ "version": version })).unwrap();
        Package::new(
            PackageIdent::new("main", "demo"),
            manifest,
            None,
            PackageStatus::NotInstalled,
        )
    }

    #[test]
    fn version_component_uses_nightly_for_nightly_packages() {
        let nightly = pkg_with_version("nightly");
        assert_eq!(AppLayout::version_component(&nightly), "nightly");

        let stable = pkg_with_version("1.2.3");
        assert_eq!(AppLayout::version_component(&stable), "1.2.3");
    }

    #[test]
    fn active_dir_honours_no_junction() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = test_roots(tmp.path());
        let layout = roots.app_layout("demo");

        assert_eq!(layout.active_dir(false, "1.0.0"), layout.current_dir);
        assert_eq!(
            layout.active_dir(true, "1.0.0"),
            layout.version_dir("1.0.0")
        );
    }

    #[test]
    fn resolve_installed_version_dir_prefers_expected_then_junction_with_normalized_leaf() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = test_roots(tmp.path());
        let layout = roots.app_layout("demo");

        let v1 = layout.version_dir("1.0.0");
        let v2 = layout.version_dir("2.0.0");
        std::fs::create_dir_all(&v1).unwrap();
        std::fs::create_dir_all(&v2).unwrap();
        crate::infra::fs::create_junction(&v2, &layout.current_dir).unwrap();

        assert_eq!(
            layout.resolve_installed_version_dir(Some("1.0.0")),
            Some(v1)
        );

        let from_junction = layout.resolve_installed_version_dir(None).unwrap();
        assert_eq!(from_junction, v2);
        assert!(
            !from_junction.to_string_lossy().starts_with(r"\\?\"),
            "junction target must be normalized without verbatim prefix: {}",
            from_junction.display()
        );
    }
}
