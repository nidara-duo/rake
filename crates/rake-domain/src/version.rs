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

pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let a = strip_build(a);
    let b = strip_build(b);

    let (a_release, a_pre) = a.split_once('-').unwrap_or((a, ""));
    let (b_release, b_pre) = b.split_once('-').unwrap_or((b, ""));

    let ord = compare_dot_segments(a_release, b_release);
    if ord != Ordering::Equal {
        return ord;
    }

    match (a_pre.is_empty(), b_pre.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => compare_dot_segments(a_pre, b_pre),
    }
}

fn strip_build(v: &str) -> &str {
    v.split('+').next().unwrap_or(v)
}

fn compare_dot_segments(a: &str, b: &str) -> Ordering {
    let segs_a = split_on_dots(a);
    let segs_b = split_on_dots(b);
    let max_len = segs_a.len().max(segs_b.len());

    for i in 0..max_len {
        match (segs_a.get(i), segs_b.get(i)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => {
                let ord = compare_subsegments(a, b);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }

    Ordering::Equal
}

fn split_on_dots(v: &str) -> Vec<Vec<String>> {
    v.split(&['.', '_'][..])
        .filter(|s| !s.is_empty())
        .map(split_numeric)
        .collect()
}

fn split_numeric(s: &str) -> Vec<String> {
    if s.is_empty() {
        return vec!["0".to_string()];
    }
    let mut result = Vec::new();
    let mut buf = String::new();
    let mut in_digit = s.chars().next().is_some_and(|c| c.is_ascii_digit());

    for c in s.chars() {
        let is_digit = c.is_ascii_digit();
        if is_digit != in_digit && !buf.is_empty() {
            result.push(std::mem::take(&mut buf));
            in_digit = is_digit;
        }
        buf.push(c);
    }

    if !buf.is_empty() {
        result.push(buf);
    }

    result
}

fn compare_subsegments(a: &[String], b: &[String]) -> Ordering {
    let max = a.len().max(b.len());

    for i in 0..max {
        match (a.get(i), b.get(i)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => {
                let ord = if a.bytes().all(|c| c.is_ascii_digit())
                    && b.bytes().all(|c| c.is_ascii_digit())
                {
                    a.parse::<u64>()
                        .ok()
                        .and_then(|an| b.parse::<u64>().ok().map(|bn| an.cmp(&bn)))
                        .unwrap_or_else(|| a.cmp(b))
                } else {
                    let al = a.to_lowercase();
                    let bl = b.to_lowercase();
                    al.cmp(&bl)
                };

                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }

    Ordering::Equal
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
