use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Domain(#[from] rake_domain::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("HTTP configuration error: {0}")]
    HttpConfig(String),

    #[error("git error: {0}")]
    Git(String),

    #[error(
        "git not found: Rake needs Git to manage buckets. Install it from https://git-scm.com/download/win, then run 'rake bucket add main'."
    )]
    GitNotFound,

    #[error("archive error: {0}")]
    Archive(String),

    #[error("download error: {0}")]
    Download(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("{0}")]
    Custom(String),
}

pub type Result<T> = std::result::Result<T, Error>;
