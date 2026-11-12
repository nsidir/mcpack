//! Fabric / Quilt loader metadata (both expose launcher-profile JSON that inherits vanilla).

use serde::{Deserialize, Serialize};

use super::mojang::VersionJson;
use crate::error::{Error, Result};
use crate::http::Http;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    #[default]
    Vanilla,
    Fabric,
    Quilt,
}

impl LoaderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LoaderKind::Vanilla => "vanilla",
            LoaderKind::Fabric => "fabric",
            LoaderKind::Quilt => "quilt",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "vanilla" | "none" | "" => Ok(LoaderKind::Vanilla),
            "fabric" => Ok(LoaderKind::Fabric),
            "quilt" => Ok(LoaderKind::Quilt),
            "forge" | "neoforge" => Err(Error::Invalid(format!(
                "loader '{s}' is not supported yet (supported: vanilla, fabric, quilt)"
            ))),
            other => Err(Error::Invalid(format!("unknown loader '{other}'"))),
        }
    }

    fn meta_base(&self) -> Option<&'static str> {
        match self {
            LoaderKind::Fabric => Some("https://meta.fabricmc.net/v2"),
            LoaderKind::Quilt => Some("https://meta.quiltmc.org/v3"),
            LoaderKind::Vanilla => None,
        }
    }

    /// Loader names used in Modrinth `loaders` filters (best-first).
    pub fn modrinth_loaders(&self) -> Vec<&'static str> {
        match self {
            LoaderKind::Fabric => vec!["fabric"],
            LoaderKind::Quilt => vec!["quilt", "fabric"],
            LoaderKind::Vanilla => vec![],
        }
    }

    /// CurseForge modLoaderType ids (best-first).
    pub fn curseforge_types(&self) -> Vec<u32> {
        match self {
            LoaderKind::Fabric => vec![4],
            LoaderKind::Quilt => vec![5, 4],
            LoaderKind::Vanilla => vec![],
        }
    }
}

#[derive(Debug, Deserialize)]
struct LoaderListEntry {
    loader: LoaderVersionInfo,
}

#[derive(Debug, Deserialize)]
struct LoaderVersionInfo {
    version: String,
    #[serde(default)]
    stable: Option<bool>,
}

/// List loader versions available for a Minecraft version (newest first).
pub async fn loader_versions(http: &Http, kind: LoaderKind, mc: &str) -> Result<Vec<(String, bool)>> {
    let base = kind.meta_base().ok_or_else(|| Error::Invalid("vanilla has no loader versions".into()))?;
    let list: Vec<LoaderListEntry> = http.json(&format!("{base}/versions/loader/{mc}")).await?;
    if list.is_empty() {
        return Err(Error::NotFound(format!("no {} loader for Minecraft {mc}", kind.as_str())));
    }
    Ok(list
        .into_iter()
        .map(|e| {
            // Quilt has no `stable` flag; treat non-beta versions as stable.
            let stable = e.loader.stable.unwrap_or(!e.loader.version.contains("beta"));
            (e.loader.version, stable)
        })
        .collect())
}

/// Resolve `latest` (or an explicit version) to a concrete loader version.
pub async fn resolve_loader_version(http: &Http, kind: LoaderKind, mc: &str, spec: &str) -> Result<String> {
    if kind == LoaderKind::Vanilla {
        return Ok(String::new());
    }
    let versions = loader_versions(http, kind, mc).await?;
    if spec.is_empty() || spec == "latest" || spec == "stable" {
        return versions
            .iter()
            .find(|(_, s)| *s)
            .or_else(|| versions.first())
            .map(|(v, _)| v.clone())
            .ok_or_else(|| Error::NotFound("loader version".into()));
    }
    if versions.iter().any(|(v, _)| v == spec) {
        Ok(spec.to_string())
    } else {
        Err(Error::NotFound(format!("{} loader {spec} for Minecraft {mc}", kind.as_str())))
    }
}

/// Fetch the loader launcher profile (inherits from vanilla `mc`).
pub async fn profile_json(http: &Http, kind: LoaderKind, mc: &str, loader_version: &str) -> Result<VersionJson> {
    let base = kind.meta_base().ok_or_else(|| Error::Invalid("vanilla has no loader profile".into()))?;
    http.json(&format!("{base}/versions/loader/{mc}/{loader_version}/profile/json")).await
}
