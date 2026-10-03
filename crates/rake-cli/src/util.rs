use rake_core::session::Session;

/// Whether at least one bucket has been added.
///
/// Commands that search manifests must tell "you have no buckets yet" apart from "the
/// package you asked for does not exist". On a fresh install the first is the real
/// blocker, while the second sends people hunting for a typo that isn't there.
pub fn has_buckets(session: &Session) -> bool {
    matches!(rake_core::bucket::added_buckets(session), Ok(b) if !b.is_empty())
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}
