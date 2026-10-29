use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Arch {
    #[serde(rename = "32bit")]
    Ia32,
    #[serde(rename = "64bit")]
    Amd64,
    #[serde(rename = "arm64")]
    Aarch64,
}

impl Arch {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86" => Arch::Ia32,
            "x86_64" => Arch::Amd64,
            "aarch64" => Arch::Aarch64,
            _ => Arch::Amd64,
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Arch::Ia32 => "32bit",
            Arch::Amd64 => "64bit",
            Arch::Aarch64 => "arm64",
        }
    }
}

/// Parse the spellings that reach us from `install.json` and the CLI.
///
/// Scoop writes `64bit`/`32bit`/`arm64`, but records from other tools (and the
/// occasional hand-edited file) use the architecture names instead, so both are
/// accepted. An unknown value is an error rather than a silent default: the
/// caller decides whether falling back is right for it.
impl std::str::FromStr for Arch {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "32bit" | "x86" | "i386" | "i686" => Ok(Arch::Ia32),
            "64bit" | "x86_64" | "amd64" | "x64" => Ok(Arch::Amd64),
            "arm64" | "aarch64" => Ok(Arch::Aarch64),
            other => Err(format!("unknown architecture '{other}'")),
        }
    }
}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::Arch;

    /// `install.json` is written by Scoop and by rake, and older records use the
    /// architecture name instead of Scoop's spelling. Both have to parse, or a
    /// 32-bit app is read back as 64-bit and its shims are resolved from the
    /// wrong manifest block.
    #[test]
    fn parses_scoop_and_architecture_spellings() {
        for (text, expected) in [
            ("32bit", Arch::Ia32),
            ("64bit", Arch::Amd64),
            ("arm64", Arch::Aarch64),
            ("x86", Arch::Ia32),
            ("i686", Arch::Ia32),
            ("x86_64", Arch::Amd64),
            ("AMD64", Arch::Amd64),
            ("aarch64", Arch::Aarch64),
        ] {
            assert_eq!(text.parse::<Arch>().unwrap(), expected, "{text}");
        }
    }

    #[test]
    fn an_unknown_architecture_is_an_error_not_a_guess() {
        assert!("sparc".parse::<Arch>().is_err());
        assert!("".parse::<Arch>().is_err());
    }
}
