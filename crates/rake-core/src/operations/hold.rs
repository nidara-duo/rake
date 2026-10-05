use std::path::PathBuf;

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

    let current_dir = root.join("apps").join(name).join("current");

    let Some(mut info) = crate::infra::install_meta::read_install_record(&current_dir)? else {
        return Err(crate::Error::Domain(rake_domain::Error::PackageNotFound(
            name.to_owned(),
        )));
    };

    info.held = held;
    // Read-modify-write through the canonical writer, so both file spellings
    // stay in sync and `url`/`bucket` are preserved.
    crate::infra::install_meta::write_install_record(&current_dir, &info)?;

    Ok(())
}
