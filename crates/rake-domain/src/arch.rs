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
}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Arch::Ia32 => write!(f, "32bit"),
            Arch::Amd64 => write!(f, "64bit"),
            Arch::Aarch64 => write!(f, "arm64"),
        }
    }
}
