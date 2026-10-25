use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version(String);

impl Version {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Version {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl<'a> From<&'a str> for Version {
    fn from(s: &'a str) -> Self {
        Self(s.to_owned())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_versions(&self.0, &other.0)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compare two versions the way Scoop's `compare_versions($a, $b)` does.
///
/// This is a port of `Compare-Version` and `SplitVersion` from
/// `assets/scoop/lib/versions.ps1`, not an approximation: the previous
/// implementation split each version into a release part and a `-suffix`, which
/// mis-ordered a letter glued to a numeric segment. `1.23b` was compared
/// against `1.23.1b` as `[1, 23b]` vs `[1, 23, 1b]`, so the third segment
/// decided it and the older build looked newer — `status` and `update` then
/// reported the app as up to date.
///
/// The etalon algorithm instead re-splits around letter runs at every level:
/// `1.23b` becomes `[1.23, b]` and `1.23.1b` becomes `[1.23.1, b]`, so the
/// numeric part is compared first and `1.23.1b` wins on the third component.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    // `compare_versions` is `Compare-Version -ReferenceVersion $b -DifferenceVersion $a`:
    // the result is the sign of `a - b`.
    compare_against_reference(b, a)
}

/// The `Compare-Version` half of the port. Returns the sign of
/// `difference - reference`, matching PowerShell's return value.
fn compare_against_reference(reference: &str, difference: &str) -> Ordering {
    // versions.ps1:141 — `+` marks a post-release. It is folded into `-` so
    // that `1.0+build1` sorts above `1.0` ("build1" is not an alpha/beta/rc/pre
    // marker) while `1.0+beta` sorts below it.
    let reference = reference.replace('+', "-");
    let difference = difference.replace('+', "-");

    // PowerShell's `-eq` on strings is case-insensitive, so this early return
    // has to be too.
    if difference.eq_ignore_ascii_case(&reference) {
        return Ordering::Equal;
    }

    let reference = split_version(&reference, '-');
    let difference = split_version(&difference, '-');

    compare_segments(&reference, &difference)
}

/// Compare two already-split versions segment by segment.
///
/// Returns the sign of `difference - reference`, like `Compare-Version`.
fn compare_segments(reference: &[String], difference: &[String]) -> Ordering {
    let max = reference.len().max(difference.len());

    for i in 0..max {
        match (reference.get(i), difference.get(i)) {
            // One side ran out of segments. A trailing alpha/beta/rc/pre
            // marker makes a version the older one ("1.1" > "1.1-beta"), any
            // other trailing segment makes it the newer one ("1.1.0" > "1.1").
            (None, Some(d)) => {
                return if is_prerelease_marker(d) {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (Some(r), None) => {
                return if is_prerelease_marker(r) {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (Some(r), Some(d)) => {
                // versions.ps1:188-207 — a segment that still contains `.` or
                // `_` is compared recursively with that separator. Both checks
                // can fire; the etalon tries `.` first.
                let ord = if r.contains('.') || d.contains('.') {
                    compare_parts(r, d, '.')
                } else if r.contains('_') || d.contains('_') {
                    compare_parts(r, d, '_')
                } else {
                    compare_atoms(d, r)
                };

                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (None, None) => return Ordering::Equal,
        }
    }

    Ordering::Equal
}

fn compare_parts(reference: &str, difference: &str, delimiter: char) -> Ordering {
    let reference = split_version(reference, delimiter);
    let difference = split_version(difference, delimiter);
    compare_segments(&reference, &difference)
}

/// `SplitVersion` from versions.ps1:231.
///
/// Each run of ASCII letters is wrapped in `delimiter`, the string is split on
/// it, empty segments are dropped and nothing is converted yet — numeric
/// conversion happens in [`compare_atoms`], because a segment that contains a
/// separator is compared as text first.
fn split_version(version: &str, delimiter: char) -> Vec<String> {
    let mut expanded = String::with_capacity(version.len() + 8);
    let mut in_letters = false;

    for ch in version.chars() {
        let is_letter = ch.is_ascii_alphabetic();
        if is_letter != in_letters {
            expanded.push(delimiter);
            in_letters = is_letter;
        }
        expanded.push(ch);
    }

    if in_letters {
        expanded.push(delimiter);
    }

    expanded
        .split(delimiter)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// versions.ps1:172 — the markers that make a version a pre-release.
fn is_prerelease_marker(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    ["alpha", "beta", "rc", "pre"]
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Compare one segment of each version. Returns the sign of
/// `difference - reference`.
///
/// versions.ps1:209-217 stringifies a numeric segment when the other side is
/// text, so `1` is compared as `"1"` against `"beta"`, not as a number.
fn compare_atoms(difference: &str, reference: &str) -> Ordering {
    match (parse_number(difference), parse_number(reference)) {
        (Some(d), Some(r)) => d.cmp(&r),
        (Some(d), None) => d.to_string().cmp(&reference.to_ascii_lowercase()),
        (None, Some(r)) => difference.to_ascii_lowercase().cmp(&r.to_string()),
        (None, None) => difference
            .to_ascii_lowercase()
            .cmp(&reference.to_ascii_lowercase()),
    }
}

fn parse_number(segment: &str) -> Option<u128> {
    if segment.bytes().all(|b| b.is_ascii_digit()) {
        segment.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_numeric() {
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0.0", "1.0.1"), Ordering::Less);
        assert_eq!(compare_versions("2.0", "1.9.9"), Ordering::Greater);
    }

    #[test]
    fn test_prerelease() {
        assert_eq!(compare_versions("1.0.0-beta", "1.0.0"), Ordering::Less);
        assert_eq!(
            compare_versions("1.0.0-alpha", "1.0.0-beta"),
            Ordering::Less
        );
    }

    #[test]
    fn test_complex() {
        assert_eq!(compare_versions("12.0.1", "12.0.1.0"), Ordering::Less);
        assert_eq!(compare_versions("2024.01.01", "2024.01.02"), Ordering::Less);
    }

    #[test]
    fn test_dates() {
        assert_eq!(
            compare_versions("2024.01.01", "2023.12.31"),
            Ordering::Greater
        );
    }

    #[test]
    fn test_nightly() {
        assert_eq!(compare_versions("nightly", "1.0.0"), Ordering::Greater);
        assert_eq!(compare_versions("nightly", "nightly"), Ordering::Equal);
    }

    #[test]
    fn test_rc_vs_release() {
        assert_eq!(compare_versions("1.0.0-rc1", "1.0.0"), Ordering::Less);
        assert_eq!(
            compare_versions("1.0.0-rc2", "1.0.0-rc1"),
            Ordering::Greater
        );
    }

    #[test]
    fn test_mixed_strings() {
        assert_eq!(compare_versions("12beta3", "12beta2"), Ordering::Greater);
        assert_eq!(compare_versions("12alpha1", "12beta1"), Ordering::Less);
    }

    /// A letter glued to the last numeric segment must not outrank a real patch
    /// release. `zen-browser` shipped `1.23b`, then `1.23.1b`; the old parser
    /// read those as `[1, 23b]` and `[1, 23, 1b]`, decided at segment 3 that
    /// `23b > 23`, and so treated the update as a downgrade.
    #[test]
    fn test_letter_suffix_after_a_numeric_segment() {
        assert_eq!(compare_versions("1.23b", "1.23.1b"), Ordering::Less);
        assert_eq!(compare_versions("1.23.1b", "1.23b"), Ordering::Greater);
        assert_eq!(compare_versions("1.23b", "1.23b"), Ordering::Equal);
        assert_eq!(compare_versions("1.23b", "1.24b"), Ordering::Less);
    }

    #[test]
    fn test_plain_patch_bump() {
        assert_eq!(compare_versions("1.96.61", "1.97.56"), Ordering::Less);
        assert_eq!(compare_versions("1.97.56", "1.96.61"), Ordering::Greater);
    }

    /// versions.ps1:141 treats `+` as a post-release marker: the suffix is
    /// compared as if it followed a `-`, and a non-pre-release suffix is newer
    /// than the bare version.
    #[test]
    fn test_post_release_suffix() {
        assert_eq!(compare_versions("1.0+build1", "1.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0+build1"), Ordering::Less);
        assert_eq!(compare_versions("1.0+beta", "1.0"), Ordering::Less);
    }

    /// An underscore is a separator, not part of a segment: `1.0_1` splits into
    /// `[1.0, 1]`, so it sorts above `1.0` and above `1.0.1` (the extra
    /// trailing segment is not a pre-release marker), exactly as
    /// `SplitVersion` recursing on `_` decides it.
    #[test]
    fn test_underscore_separator() {
        assert_eq!(compare_versions("1.0_1", "1.0.1"), Ordering::Greater);
        assert_eq!(compare_versions("1.0_1", "1.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0_1", "1.0_2"), Ordering::Less);
        assert_eq!(compare_versions("1.0_1", "1.0_1"), Ordering::Equal);
    }

    #[test]
    fn test_leading_zeroes() {
        assert_eq!(compare_versions("1.01", "1.1"), Ordering::Equal);
        assert_eq!(compare_versions("1.010", "1.01"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.1", "1.0.01"), Ordering::Equal);
    }

    #[test]
    fn test_empty_segments() {
        assert_eq!(compare_versions("1..0", "1.0"), Ordering::Equal);
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(compare_versions("1.0-BETA", "1.0-beta"), Ordering::Equal);
    }

    #[test]
    fn test_version_struct() {
        let v1 = Version::new("2.0.0");
        let v2 = Version::new("1.9.9");
        assert!(v1 > v2);
        assert!(v2 < v1);
        assert_eq!(v1, Version::new("2.0.0"));
    }
}
