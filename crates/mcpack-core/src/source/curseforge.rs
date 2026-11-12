//! CurseForge API v1 client (requires an API key).

use serde::Deserialize;

use super::{DepRef, ResolvedVersion, SearchFilters, SearchHit, VersionSummary};
use crate::error::{Error, Result};
use crate::http::Http;
use crate::meta::loader::LoaderKind;
use crate::pack::schema::{Channel, ModKind, Source};

pub const API: &str = "https://api.curseforge.com/v1";
const MINECRAFT_GAME_ID: u32 = 432;

#[derive(Debug, Deserialize)]
struct Data<T> {
    data: T,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfMod {
    pub id: u64,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub class_id: Option<u32>,
    #[serde(default)]
    pub download_count: f64,
    #[serde(default)]
    pub links: Option<CfLinks>,
    #[serde(default)]
    pub authors: Vec<CfAuthor>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfLinks {
    #[serde(default)]
    pub website_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CfAuthor {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfFile {
    pub id: u64,
    pub mod_id: u64,
    pub display_name: String,
    pub file_name: String,
    pub release_type: u32,
    #[serde(default)]
    pub file_date: String,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub hashes: Vec<CfHash>,
    #[serde(default)]
    pub file_length: Option<u64>,
    #[serde(default)]
    pub dependencies: Vec<CfDep>,
    #[serde(default)]
    pub game_versions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CfHash {
    pub value: String,
    pub algo: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfDep {
    pub mod_id: u64,
    pub relation_type: u32,
}

pub fn kind_from_class(class_id: Option<u32>) -> ModKind {
    match class_id {
        Some(12) => ModKind::Resourcepack,
        Some(6552) => ModKind::Shaderpack,
        Some(6945) => ModKind::Datapack,
        _ => ModKind::Mod,
    }
}

fn class_for_kind(k: ModKind) -> u32 {
    match k {
        ModKind::Mod => 6,
        ModKind::Resourcepack => 12,
        ModKind::Shaderpack => 6552,
        ModKind::Datapack => 6945,
    }
}

fn release_type_str(t: u32) -> &'static str {
    match t {
        1 => "release",
        2 => "beta",
        _ => "alpha",
    }
}

#[derive(Clone)]
pub struct CurseForge {
    http: Http,
    key: String,
}

impl CurseForge {
    pub fn new(http: Http, key: Option<String>) -> Result<Self> {
        let key = key.filter(|k| !k.trim().is_empty()).ok_or_else(|| {
            Error::Config(
                "CurseForge requires an API key: set CURSEFORGE_API_KEY or `curseforge_api_key` in the mcpack config.toml (get one at https://console.curseforge.com)".into(),
            )
        })?;
        Ok(Self { http, key })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str, q: &[(&str, String)]) -> Result<T> {
        let key = self.key.clone();
        let d: Data<T> = self
            .http
            .json_with(&format!("{API}{path}"), |r| r.header("x-api-key", &key).header("Accept", "application/json").query(q))
            .await?;
        Ok(d.data)
    }

    pub async fn get_mod(&self, id_or_slug: &str) -> Result<CfMod> {
        if let Ok(id) = id_or_slug.parse::<u64>() {
            return self.get(&format!("/mods/{id}"), &[]).await.map_err(|e| match e {
                Error::HttpStatus { status: 404, .. } => Error::NotFound(format!("CurseForge mod {id}")),
                other => other,
            });
        }
        let hits: Vec<CfMod> = self
            .get(
                "/mods/search",
                &[("gameId", MINECRAFT_GAME_ID.to_string()), ("slug", id_or_slug.to_string())],
            )
            .await?;
        // Prefer the mods class when a slug is shared across classes.
        let mut hits = hits;
        hits.sort_by_key(|m| if m.class_id == Some(6) { 0 } else { 1 });
        hits.into_iter()
            .next()
            .ok_or_else(|| Error::NotFound(format!("CurseForge project with slug '{id_or_slug}'")))
    }

    async fn files(&self, mod_id: u64, mc: Option<&str>, loader_type: Option<u32>) -> Result<Vec<CfFile>> {
        let mut q = vec![("pageSize", "50".to_string())];
        if let Some(mc) = mc {
            q.push(("gameVersion", mc.to_string()));
        }
        if let Some(t) = loader_type {
            q.push(("modLoaderType", t.to_string()));
        }
        let mut files: Vec<CfFile> = self.get(&format!("/mods/{mod_id}/files"), &q).await?;
        files.sort_by(|a, b| b.file_date.cmp(&a.file_date));
        Ok(files)
    }

    async fn file(&self, mod_id: u64, file_id: u64) -> Result<CfFile> {
        self.get(&format!("/mods/{mod_id}/files/{file_id}"), &[]).await
    }

    async fn candidate_files(&self, m: &CfMod, kind: ModKind, mc: &str, loader: LoaderKind) -> Result<Vec<CfFile>> {
        if kind == ModKind::Mod {
            for t in loader.curseforge_types() {
                let fs = self.files(m.id, Some(mc), Some(t)).await?;
                if !fs.is_empty() {
                    return Ok(fs);
                }
            }
            Ok(vec![])
        } else {
            self.files(m.id, Some(mc), None).await
        }
    }

    pub async fn search(&self, query: &str, f: &SearchFilters) -> Result<Vec<SearchHit>> {
        let kind = f.kind.unwrap_or(ModKind::Mod);
        let mut q = vec![
            ("gameId", MINECRAFT_GAME_ID.to_string()),
            ("searchFilter", query.to_string()),
            ("classId", class_for_kind(kind).to_string()),
            ("sortField", "2".to_string()),
            ("sortOrder", "desc".to_string()),
            ("pageSize", (if f.limit == 0 { 10 } else { f.limit.min(50) }).to_string()),
        ];
        if let Some(mc) = &f.minecraft {
            q.push(("gameVersion", mc.clone()));
        }
        if let (Some(l), ModKind::Mod) = (f.loader, kind) {
            if let Some(t) = l.curseforge_types().first() {
                q.push(("modLoaderType", t.to_string()));
            }
        }
        let mods: Vec<CfMod> = self.get("/mods/search", &q).await?;
        Ok(mods
            .into_iter()
            .map(|m| SearchHit {
                source: Source::Curseforge,
                id: m.id.to_string(),
                slug: Some(m.slug.clone()),
                title: m.name.clone(),
                description: m.summary.clone(),
                kind: Some(kind_from_class(m.class_id)),
                downloads: m.download_count as u64,
                author: m.authors.first().map(|a| a.name.clone()),
                url: m.links.as_ref().and_then(|l| l.website_url.clone()),
            })
            .collect())
    }

    pub async fn list_versions(&self, id: &str, mc: Option<&str>, loader: Option<LoaderKind>) -> Result<Vec<VersionSummary>> {
        let m = self.get_mod(id).await?;
        let kind = kind_from_class(m.class_id);
        let lt = if kind == ModKind::Mod { loader.and_then(|l| l.curseforge_types().first().copied()) } else { None };
        let files = self.files(m.id, mc, lt).await?;
        Ok(files
            .into_iter()
            .map(|f| VersionSummary {
                id: f.id.to_string(),
                version_number: f.display_name.clone(),
                version_type: release_type_str(f.release_type).to_string(),
                game_versions: f.game_versions.clone(),
                loaders: vec![],
                date: f.file_date.clone(),
                filename: Some(f.file_name.clone()),
            })
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn resolve(
        &self,
        id: &str,
        version_spec: &str,
        kind_hint: Option<ModKind>,
        channel: Option<Channel>,
        mc: &str,
        loader: LoaderKind,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedVersion> {
        let m = self.get_mod(id).await?;
        let kind = kind_hint.unwrap_or_else(|| kind_from_class(m.class_id));
        let file = if version_spec == "latest" || version_spec.is_empty() {
            let files = self.candidate_files(&m, kind, mc, loader).await?;
            let ch = channel.unwrap_or(Channel::Release);
            match files.iter().find(|f| ch.allows(release_type_str(f.release_type))) {
                Some(f) => f.clone(),
                None => {
                    let f = files.first().cloned().ok_or_else(|| {
                        Error::Resolve(format!("no CurseForge file of '{}' for Minecraft {mc} / {}", m.name, loader.as_str()))
                    })?;
                    warnings.push(format!(
                        "{}: no {:?} file; using {} ({})",
                        m.name,
                        ch,
                        f.display_name,
                        release_type_str(f.release_type)
                    ));
                    f
                }
            }
        } else if let Ok(fid) = version_spec.parse::<u64>() {
            self.file(m.id, fid).await?
        } else {
            let files = self.files(m.id, None, None).await?;
            files
                .into_iter()
                .find(|f| f.display_name == version_spec || f.file_name == version_spec || f.display_name.contains(version_spec))
                .ok_or_else(|| Error::Resolve(format!("'{}' has no file matching '{version_spec}'", m.name)))?
        };
        Ok(self.to_resolved(&m, kind, &file, warnings))
    }

    pub async fn resolve_dep(&self, dep: &DepRef, mc: &str, loader: LoaderKind, warnings: &mut Vec<String>) -> Result<ResolvedVersion> {
        let pid = dep
            .project_id
            .as_deref()
            .ok_or_else(|| Error::Resolve("CurseForge dependency without mod id".into()))?;
        self.resolve(pid, "latest", None, None, mc, loader, warnings).await
    }

    fn to_resolved(&self, m: &CfMod, kind: ModKind, f: &CfFile, warnings: &mut Vec<String>) -> ResolvedVersion {
        let mut required = Vec::new();
        let mut optional = Vec::new();
        let mut incompatible = Vec::new();
        for d in &f.dependencies {
            let r = DepRef { project_id: Some(d.mod_id.to_string()), version_id: None };
            match d.relation_type {
                3 => required.push(r),
                2 => optional.push(r),
                5 => incompatible.push(r),
                _ => {}
            }
        }
        let page = m
            .links
            .as_ref()
            .and_then(|l| l.website_url.clone())
            .map(|u| format!("{}/files/{}", u.trim_end_matches('/'), f.id));
        if f.download_url.is_none() {
            warnings.push(format!(
                "{} ({}) disallows third-party downloads; download it manually from {} and place it in <pack>/manual/ or your Downloads folder",
                m.name,
                f.file_name,
                page.as_deref().unwrap_or("CurseForge")
            ));
        }
        ResolvedVersion {
            source: Source::Curseforge,
            project_id: m.id.to_string(),
            slug: Some(m.slug.clone()),
            name: m.name.clone(),
            kind,
            version_id: f.id.to_string(),
            version_number: f.display_name.clone(),
            version_type: release_type_str(f.release_type).to_string(),
            filename: f.file_name.clone(),
            url: f.download_url.clone(),
            page_url: page,
            sha1: f.hashes.iter().find(|h| h.algo == 1).map(|h| h.value.clone()),
            sha512: None,
            size: f.file_length,
            required,
            optional,
            incompatible,
        }
    }
}
