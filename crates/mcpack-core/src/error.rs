use std::path::Path;

use thiserror::Error;

/// All errors produced by mcpack-core. Every variant has a stable machine-readable
/// [`Error::code`] so automation (and AI agents) can branch on it.
#[derive(Debug, Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("http {status} for {url}: {body}")]
    HttpStatus { status: u16, url: String, body: String },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml parse error: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("toml edit error: {0}")]
    TomlEdit(#[from] toml_edit::TomlError),
    #[error("toml serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("hash mismatch for {what}: expected {expected}, got {actual}")]
    HashMismatch { what: String, expected: String, actual: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("resolution failed: {0}")]
    Resolve(String),
    #[error("launch failed: {0}")]
    Launch(String),
    #[error("agent error: {0}")]
    Agent(String),
    #[error("missing configuration: {0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Stable short code, e.g. `not_found`, `resolve`, `http`.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Io { .. } => "io",
            Error::Http(_) | Error::HttpStatus { .. } => "http",
            Error::Json(_) => "json",
            Error::TomlDe(_) | Error::TomlEdit(_) | Error::TomlSer(_) => "toml",
            Error::Zip(_) => "zip",
            Error::HashMismatch { .. } => "hash_mismatch",
            Error::NotFound(_) => "not_found",
            Error::Invalid(_) => "invalid",
            Error::Resolve(_) => "resolve",
            Error::Launch(_) => "launch",
            Error::Agent(_) => "agent",
            Error::Config(_) => "config",
            Error::Other(_) => "other",
        }
    }

    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Error::Io { path: path.as_ref().display().to_string(), source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Attach a path to `std::io::Error`s.
pub trait IoContext<T> {
    fn ctx(self, path: impl AsRef<Path>) -> Result<T>;
}

impl<T> IoContext<T> for std::result::Result<T, std::io::Error> {
    fn ctx(self, path: impl AsRef<Path>) -> Result<T> {
        self.map_err(|e| Error::io(path, e))
    }
}
