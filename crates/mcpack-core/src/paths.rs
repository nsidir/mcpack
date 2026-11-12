use std::path::{Path, PathBuf};

/// Global launcher directories (shared caches). Pack instances live next to their pack
/// (`<pack>/run/<profile>`), so packs stay self-contained.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    /// `$MCPACK_HOME`, else `<data_dir>/mcpack` (e.g. `%APPDATA%\mcpack`).
    pub fn from_env() -> Self {
        let home = std::env::var_os("MCPACK_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|d| d.join("mcpack")))
            .unwrap_or_else(|| PathBuf::from(".mcpack"));
        Self { home }
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    pub fn cache(&self) -> PathBuf {
        self.home.join("cache")
    }
    pub fn libraries(&self) -> PathBuf {
        self.cache().join("libraries")
    }
    pub fn assets(&self) -> PathBuf {
        self.cache().join("assets")
    }
    pub fn versions(&self) -> PathBuf {
        self.cache().join("versions")
    }
    pub fn natives(&self) -> PathBuf {
        self.cache().join("natives")
    }
    pub fn java(&self) -> PathBuf {
        self.cache().join("java")
    }
    pub fn meta(&self) -> PathBuf {
        self.cache().join("meta")
    }
    /// Content-addressed store for mod/resourcepack files: `files/<sha1[0..2]>/<sha1>/<filename>`.
    pub fn file_store(&self) -> PathBuf {
        self.cache().join("files")
    }
    pub fn stored_file(&self, sha1: &str, filename: &str) -> PathBuf {
        let prefix = if sha1.len() >= 2 { &sha1[..2] } else { "xx" };
        self.file_store().join(prefix).join(sha1).join(filename)
    }
    pub fn config_file(&self) -> PathBuf {
        self.home.join("config.toml")
    }
    pub fn agent_dir(&self) -> PathBuf {
        self.home.join("agent")
    }
}

/// Instance (game directory) for a pack profile.
pub fn instance_dir(pack_dir: &Path, profile: &str) -> PathBuf {
    pack_dir.join("run").join(profile)
}
