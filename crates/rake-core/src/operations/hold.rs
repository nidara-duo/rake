use std::path::PathBuf;

use rake_domain::package::InstallRecord;

use crate::Result;
use crate::session::Session;

/// Set the hold flag on an installed package's install.json.
pub fn set_held(session: &Session, name: &str, held: bool) -> Result<()> {
    let _guard = session.write_lock()?;
    let root = session
        .config()
        .root_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("apps"));

    let install_json = root
        .join("apps")
        .join(name)
        .join("current")
        .join("install.json");
    if !install_json.exists() {
        return Err(crate::Error::Domain(rake_domain::Error::PackageNotFound(
            name.to_owned(),
        )));
    }

    let content = std::fs::read_to_string(&install_json)?;
    let mut info: InstallRecord = serde_json::from_str(&content)?;
    info.held = held;
    std::fs::write(&install_json, serde_json::to_string_pretty(&info)?)?;

    Ok(())
}
