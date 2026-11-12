use serde::{Deserialize, Serialize};

use crate::error::{IoContext, Result};
use crate::paths::Paths;

/// Global launcher settings from `<home>/config.toml`, overridable by env vars.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalConfig {
    /// CurseForge API key (`CURSEFORGE_API_KEY` env overrides).
    pub curseforge_api_key: Option<String>,
    /// Default offline username.
    pub username: String,
    /// Max parallel downloads.
    pub download_concurrency: usize,
    /// Prefer this Java executable over Mojang-provided runtimes.
    pub java_path: Option<String>,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            curseforge_api_key: None,
            username: "Player".into(),
            download_concurrency: 24,
            java_path: None,
        }
    }
}

impl GlobalConfig {
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.config_file();
        let mut cfg: GlobalConfig = if file.exists() {
            let text = std::fs::read_to_string(&file).ctx(&file)?;
            toml::from_str(&text)?
        } else {
            GlobalConfig::default()
        };
        if let Ok(k) = std::env::var("CURSEFORGE_API_KEY") {
            if !k.trim().is_empty() {
                cfg.curseforge_api_key = Some(k.trim().to_string());
            }
        }
        if let Ok(u) = std::env::var("MCPACK_USERNAME") {
            if !u.trim().is_empty() {
                cfg.username = u.trim().to_string();
            }
        }
        if let Ok(j) = std::env::var("MCPACK_JAVA") {
            if !j.trim().is_empty() {
                cfg.java_path = Some(j.trim().to_string());
            }
        }
        if cfg.download_concurrency == 0 {
            cfg.download_concurrency = 8;
        }
        Ok(cfg)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let file = paths.config_file();
        if let Some(p) = file.parent() {
            std::fs::create_dir_all(p).ctx(p)?;
        }
        std::fs::write(&file, toml::to_string_pretty(self)?).ctx(&file)
    }
}
