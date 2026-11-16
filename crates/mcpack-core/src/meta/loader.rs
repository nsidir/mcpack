//! Mod loader metadata: Fabric / Quilt (launcher-profile JSON that inherits vanilla) and
//! Forge / NeoForge (installer jars, see [`crate::install::forge`]).

use serde::{Deserialize, Serialize};

use super::mojang::VersionJson;
use crate::error::{Error, Result};
use crate::http::Http;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    #[default]
    Vanilla,
    Fabric,
    Quilt,
    Forge,
    #[serde(rename = "neoforge", alias = "neo_forge", alias = "neo-forge")]
    NeoForge,
}

pub const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/";
pub const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/";
const FORGE_META: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/maven-metadata.json";
const FORGE_PROMOS: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
const NEOFORGE_VERSIONS: &str = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
const NEOFORGE_LEGACY_VERSIONS: &str = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/forge";

impl LoaderKind {
    pub const ALL: [LoaderKind; 5] =
        [LoaderKind::Vanilla, LoaderKind::Fabric, LoaderKind::Quilt, LoaderKind::Forge, LoaderKind::NeoForge];

    pub fn as_str(&self) -> &'static str {
        match self {
            LoaderKind::Vanilla => "vanilla",
            LoaderKind::Fabric => "fabric",
            LoaderKind::Quilt => "quilt",
            LoaderKind::Forge => "forge",
            LoaderKind::NeoForge => "neoforge",
        }
    }

    /// Human-readable name.
    pub fn display(&self) -> &'static str {
        match self {
            LoaderKind::Vanilla => "Vanilla",
            LoaderKind::Fabric => "Fabric",
            LoaderKind::Quilt => "Quilt",
            LoaderKind::Forge => "Forge",
            LoaderKind::NeoForge => "NeoForge",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "vanilla" | "none" | "" => Ok(LoaderKind::Vanilla),
            "fabric" => Ok(LoaderKind::Fabric),
            "quilt" => Ok(LoaderKind::Quilt),
            "forge" | "minecraftforge" => Ok(LoaderKind::Forge),
            "neoforge" | "neoforged" | "neo" => Ok(LoaderKind::NeoForge),
            _ => Err(Error::Invalid(format!("unknown loader '{s}' (supported: vanilla, fabric, quilt, forge, neoforge)"))),
        }
    }

    /// Forge-family loaders install through an installer jar and load `mods.toml` mods.
    pub fn is_forge_like(&self) -> bool {
        matches!(self, LoaderKind::Forge | LoaderKind::NeoForge)
    }

    /// Fabric-family loaders (Quilt loads Fabric mods).
    pub fn is_fabric_like(&self) -> bool {
        matches!(self, LoaderKind::Fabric | LoaderKind::Quilt)
    }

    fn fabric_meta_base(&self) -> Option<&'static str> {
        match self {
            LoaderKind::Fabric => Some("https://meta.fabricmc.net/v2"),
            LoaderKind::Quilt => Some("https://meta.quiltmc.org/v3"),
            _ => None,
        }
    }

    /// Loader names used in Modrinth `loaders` filters (best-first).
    pub fn modrinth_loaders(&self) -> Vec<&'static str> {
        self.modrinth_loaders_for("")
    }

    /// Modrinth loader names for a Minecraft version: NeoForge for 1.20.1 is a Forge fork
    /// that also loads Forge mods.
    pub fn modrinth_loaders_for(&self, mc: &str) -> Vec<&'static str> {
        match self {
            LoaderKind::Fabric => vec!["fabric"],
            LoaderKind::Quilt => vec!["quilt", "fabric"],
            LoaderKind::Forge => vec!["forge"],
            LoaderKind::NeoForge if mc == "1.20.1" => vec!["neoforge", "forge"],
            LoaderKind::NeoForge => vec!["neoforge"],
            LoaderKind::Vanilla => vec![],
        }
    }

    /// CurseForge modLoaderType ids (best-first).
    pub fn curseforge_types(&self) -> Vec<u32> {
        self.curseforge_types_for("")
    }

    pub fn curseforge_types_for(&self, mc: &str) -> Vec<u32> {
        match self {
            LoaderKind::Forge => vec![1],
            LoaderKind::Fabric => vec![4],
            LoaderKind::Quilt => vec![5, 4],
            LoaderKind::NeoForge if mc == "1.20.1" => vec![6, 1],
            LoaderKind::NeoForge => vec![6],
            LoaderKind::Vanilla => vec![],
        }
    }

    /// Loader from a CurseForge modLoaderType id.
    pub fn from_curseforge_type(t: u32) -> Option<Self> {
        match t {
            1 => Some(LoaderKind::Forge),
            4 => Some(LoaderKind::Fabric),
            5 => Some(LoaderKind::Quilt),
            6 => Some(LoaderKind::NeoForge),
            _ => None,
        }
    }

    /// Mod ids the loader itself provides (for jar dependency checks).
    pub fn builtin_mod_ids(&self) -> &'static [&'static str] {
        match self {
            LoaderKind::Fabric => &["fabricloader", "fabric-loader"],
            LoaderKind::Quilt => &["quilt_loader", "quilt_base", "fabricloader", "fabric-loader"],
            LoaderKind::Forge => &["forge", "fml", "javafml", "lowcodefml", "mcp", "mixin"],
            LoaderKind::NeoForge => &["neoforge", "forge", "fml", "javafml", "lowcodefml", "mcp", "mixin"],
            LoaderKind::Vanilla => &[],
        }
    }
}

impl std::fmt::Display for LoaderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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

#[derive(Debug, Deserialize)]
struct MavenVersions {
    versions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ForgePromos {
    promos: std::collections::BTreeMap<String, String>,
}

/// Minecraft version a NeoForge version targets: `20.4.237` → `1.20.4`, `21.0.5-beta` → `1.21`,
/// `26.1.0.12` → `26.1`, `26.1.2.3` → `26.1.2`. `None` for special builds (April Fools etc.).
pub fn neoforge_minecraft(v: &str) -> Option<String> {
    let core = v.split(['-', '+']).next()?;
    let nums: Vec<u32> = core.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    match nums.as_slice() {
        // Year-based Minecraft versions (26.1+): <year>.<drop>.<hotfix>.<build>
        [a, b, c, _] if *a >= 25 => Some(if *c == 0 { format!("{a}.{b}") } else { format!("{a}.{b}.{c}") }),
        [a, b, _] if *a >= 20 && *a < 25 => Some(if *b == 0 { format!("1.{a}") } else { format!("1.{a}.{b}") }),
        _ => None,
    }
}

/// List loader versions available for a Minecraft version (newest first, `bool` = stable).
pub async fn loader_versions(http: &Http, kind: LoaderKind, mc: &str) -> Result<Vec<(String, bool)>> {
    let out = match kind {
        LoaderKind::Vanilla => return Err(Error::Invalid("vanilla has no loader versions".into())),
        LoaderKind::Fabric | LoaderKind::Quilt => {
            let base = kind.fabric_meta_base().expect("fabric-like");
            let list: Vec<LoaderListEntry> = http.json(&format!("{base}/versions/loader/{mc}")).await?;
            list.into_iter()
                .map(|e| {
                    // Quilt has no `stable` flag; treat non-beta versions as stable.
                    let stable = e.loader.stable.unwrap_or(!e.loader.version.contains("beta"));
                    (e.loader.version, stable)
                })
                .collect::<Vec<_>>()
        }
        LoaderKind::Forge => {
            let meta: std::collections::BTreeMap<String, Vec<String>> = http.json(FORGE_META).await?;
            let promos: Option<ForgePromos> = http.json(FORGE_PROMOS).await.ok();
            let recommended = promos.as_ref().and_then(|p| p.promos.get(&format!("{mc}-recommended")).cloned());
            let prefix = format!("{mc}-");
            let mut list: Vec<(String, bool)> = meta
                .get(mc)
                .map(|vs| {
                    vs.iter()
                        .filter_map(|full| full.strip_prefix(&prefix).map(str::to_string))
                        .map(|v| {
                            let short = v.split('-').next().unwrap_or(&v).to_string();
                            let stable = recommended.as_deref() == Some(short.as_str());
                            (v, stable)
                        })
                        .collect()
                })
                .unwrap_or_default();
            list.reverse();
            list
        }
        LoaderKind::NeoForge => {
            if mc == "1.20.1" {
                let v: MavenVersions = http.json(NEOFORGE_LEGACY_VERSIONS).await?;
                let prefix = format!("{mc}-");
                let mut list: Vec<(String, bool)> = v
                    .versions
                    .iter()
                    .filter_map(|full| full.strip_prefix(&prefix).map(|s| (s.to_string(), !s.contains("beta"))))
                    .collect();
                list.reverse();
                list
            } else {
                let v: MavenVersions = http.json(NEOFORGE_VERSIONS).await?;
                let mut list: Vec<(String, bool)> = v
                    .versions
                    .iter()
                    .filter(|s| neoforge_minecraft(s).as_deref() == Some(mc))
                    .map(|s| (s.clone(), !(s.contains("beta") || s.contains("alpha"))))
                    .collect();
                list.reverse();
                list
            }
        }
    };
    if out.is_empty() {
        return Err(Error::NotFound(format!("no {} loader for Minecraft {mc}", kind.display())));
    }
    Ok(out)
}

/// Resolve `latest` (or an explicit version) to a concrete loader version.
pub async fn resolve_loader_version(http: &Http, kind: LoaderKind, mc: &str, spec: &str) -> Result<String> {
    if kind == LoaderKind::Vanilla {
        return Ok(String::new());
    }
    let versions = loader_versions(http, kind, mc).await?;
    if spec.is_empty() || spec == "latest" || spec == "stable" || spec == "recommended" {
        return versions
            .iter()
            .find(|(_, s)| *s)
            .or_else(|| versions.first())
            .map(|(v, _)| v.clone())
            .ok_or_else(|| Error::NotFound("loader version".into()));
    }
    if spec == "newest" {
        return versions.first().map(|(v, _)| v.clone()).ok_or_else(|| Error::NotFound("loader version".into()));
    }
    // Accept "1.20.1-47.3.0" for Forge, and short Forge versions of old builds ("10.13.4.1614").
    let spec = spec.strip_prefix(&format!("{mc}-")).unwrap_or(spec);
    if let Some((v, _)) = versions.iter().find(|(v, _)| v == spec || v.split('-').next() == Some(spec)) {
        Ok(v.clone())
    } else {
        Err(Error::NotFound(format!("{} loader {spec} for Minecraft {mc}", kind.display())))
    }
}

/// Fetch the Fabric/Quilt launcher profile (inherits from vanilla `mc`).
pub async fn profile_json(http: &Http, kind: LoaderKind, mc: &str, loader_version: &str) -> Result<VersionJson> {
    let base = kind
        .fabric_meta_base()
        .ok_or_else(|| Error::Invalid(format!("{} has no launcher profile endpoint", kind.display())))?;
    http.json(&format!("{base}/versions/loader/{mc}/{loader_version}/profile/json")).await
}

/// Maven coordinate (`group:artifact:version`) of a Forge/NeoForge build and its repository.
pub fn forge_artifact(kind: LoaderKind, mc: &str, loader_version: &str) -> Result<(String, &'static str)> {
    match kind {
        LoaderKind::Forge => Ok((format!("net.minecraftforge:forge:{mc}-{loader_version}"), FORGE_MAVEN)),
        LoaderKind::NeoForge if mc == "1.20.1" => Ok((format!("net.neoforged:forge:{mc}-{loader_version}"), NEOFORGE_MAVEN)),
        LoaderKind::NeoForge => Ok((format!("net.neoforged:neoforge:{loader_version}"), NEOFORGE_MAVEN)),
        _ => Err(Error::Invalid(format!("{} has no installer", kind.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neoforge_versions_map_to_minecraft() {
        assert_eq!(neoforge_minecraft("20.4.237").as_deref(), Some("1.20.4"));
        assert_eq!(neoforge_minecraft("20.2.3-beta").as_deref(), Some("1.20.2"));
        assert_eq!(neoforge_minecraft("21.0.5-beta").as_deref(), Some("1.21"));
        assert_eq!(neoforge_minecraft("21.1.77").as_deref(), Some("1.21.1"));
        assert_eq!(neoforge_minecraft("26.1.0.12").as_deref(), Some("26.1"));
        assert_eq!(neoforge_minecraft("26.1.2.3-beta").as_deref(), Some("26.1.2"));
        assert_eq!(neoforge_minecraft("0.25w14craftmine.3-beta"), None);
    }

    #[test]
    fn parse_loaders() {
        assert_eq!(LoaderKind::parse("NeoForge").unwrap(), LoaderKind::NeoForge);
        assert_eq!(LoaderKind::parse("forge").unwrap(), LoaderKind::Forge);
        assert_eq!(serde_json::to_string(&LoaderKind::NeoForge).unwrap(), "\"neoforge\"");
        assert!(LoaderKind::parse("rift").is_err());
    }

    #[test]
    fn artifacts() {
        assert_eq!(forge_artifact(LoaderKind::Forge, "1.20.1", "47.3.0").unwrap().0, "net.minecraftforge:forge:1.20.1-47.3.0");
        assert_eq!(forge_artifact(LoaderKind::NeoForge, "1.21.1", "21.1.77").unwrap().0, "net.neoforged:neoforge:21.1.77");
        assert_eq!(forge_artifact(LoaderKind::NeoForge, "1.20.1", "47.1.106").unwrap().0, "net.neoforged:forge:1.20.1-47.1.106");
    }
}
