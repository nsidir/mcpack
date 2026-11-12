//! Modrinth API v2 client.

use serde::Deserialize;

use super::{DepRef, ResolvedVersion, SearchFilters, SearchHit, VersionSummary};
use crate::error::{Error, Result};
use crate::http::Http;
use crate::meta::loader::LoaderKind;
use crate::pack::schema::{Channel, ModKind, Source};

pub const API: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub id: String,
    pub slug: Option<String>,
    pub title: String,
    pub project_type: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    pub version_number: String,
    pub version_type: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub date_published: String,
    pub files: Vec<VFile>,
    #[serde(default)]
    pub dependencies: Vec<VDep>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VFile {
    pub hashes: VHashes,
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VHashes {
    pub sha1: Option<String>,
    pub sha512: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VDep {
    pub version_id: Option<String>,
    pub project_id: Option<String>,
    pub dependency_type: String,
}

#[derive(Debug, Deserialize)]
struct SearchResp {
    hits: Vec<RawHit>,
}

#[derive(Debug, Deserialize)]
struct RawHit {
    project_id: String,
    slug: Option<String>,
    title: String,
    #[serde(default)]
    description: String,
    project_type: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    author: Option<String>,
}

pub fn kind_from_project_type(t: &str) -> Option<ModKind> {
    match t {
        "mod" => Some(ModKind::Mod),
        "resourcepack" => Some(ModKind::Resourcepack),
        "shader" => Some(ModKind::Shaderpack),
        "datapack" => Some(ModKind::Datapack),
        _ => None,
    }
}

fn project_type_facet(k: ModKind) -> &'static str {
    match k {
        ModKind::Mod => "mod",
        ModKind::Resourcepack => "resourcepack",
        ModKind::Shaderpack => "shader",
        ModKind::Datapack => "datapack",
    }
}

/// Loader filter values for a given content kind.
fn loader_filter(kind: ModKind, loader: LoaderKind) -> Vec<&'static str> {
    match kind {
        ModKind::Mod => loader.modrinth_loaders(),
        ModKind::Resourcepack => vec!["minecraft"],
        ModKind::Datapack => vec!["datapack"],
        ModKind::Shaderpack => vec![],
    }
}

#[derive(Clone)]
pub struct Modrinth {
    http: Http,
}

impl Modrinth {
    pub fn new(http: Http) -> Self {
        Self { http }
    }

    pub async fn project(&self, id: &str) -> Result<Project> {
        self.http.json(&format!("{API}/project/{}", enc(id))).await.map_err(|e| match e {
            Error::HttpStatus { status: 404, .. } => Error::NotFound(format!("Modrinth project '{id}'")),
            other => other,
        })
    }

    pub async fn version(&self, id: &str) -> Result<Version> {
        self.http.json(&format!("{API}/version/{}", enc(id))).await.map_err(|e| match e {
            Error::HttpStatus { status: 404, .. } => Error::NotFound(format!("Modrinth version '{id}'")),
            other => other,
        })
    }

    pub async fn versions(
        &self,
        project: &str,
        mc: Option<&str>,
        loaders: &[&str],
    ) -> Result<Vec<Version>> {
        let mut q: Vec<(&str, String)> = Vec::new();
        if !loaders.is_empty() {
            q.push(("loaders", serde_json::to_string(loaders)?));
        }
        if let Some(mc) = mc {
            q.push(("game_versions", serde_json::to_string(&[mc])?));
        }
        self.http
            .json_with(&format!("{API}/project/{}/version", enc(project)), |r| r.query(&q))
            .await
    }

    pub async fn search(&self, query: &str, f: &SearchFilters) -> Result<Vec<SearchHit>> {
        let mut facets: Vec<Vec<String>> = Vec::new();
        let kind = f.kind.unwrap_or(ModKind::Mod);
        facets.push(vec![format!("project_type:{}", project_type_facet(kind))]);
        if let Some(mc) = &f.minecraft {
            facets.push(vec![format!("versions:{mc}")]);
        }
        if let Some(l) = f.loader {
            let ls = loader_filter(kind, l);
            if !ls.is_empty() && kind == ModKind::Mod {
                facets.push(ls.iter().map(|l| format!("categories:{l}")).collect());
            }
        }
        let limit = if f.limit == 0 { 10 } else { f.limit.min(100) };
        let q = vec![
            ("query", query.to_string()),
            ("facets", serde_json::to_string(&facets)?),
            ("limit", limit.to_string()),
        ];
        let resp: SearchResp = self.http.json_with(&format!("{API}/search"), |r| r.query(&q)).await?;
        Ok(resp
            .hits
            .into_iter()
            .map(|h| SearchHit {
                source: Source::Modrinth,
                url: Some(format!(
                    "https://modrinth.com/{}/{}",
                    h.project_type,
                    h.slug.clone().unwrap_or_else(|| h.project_id.clone())
                )),
                kind: kind_from_project_type(&h.project_type),
                id: h.project_id,
                slug: h.slug,
                title: h.title,
                description: h.description,
                downloads: h.downloads,
                author: h.author,
            })
            .collect())
    }

    pub async fn list_versions(
        &self,
        id: &str,
        mc: Option<&str>,
        loader: Option<LoaderKind>,
    ) -> Result<Vec<VersionSummary>> {
        let project = self.project(id).await?;
        let kind = kind_from_project_type(&project.project_type).unwrap_or(ModKind::Mod);
        let loaders = loader.map(|l| loader_filter(kind, l)).unwrap_or_default();
        let vs = self.versions(&project.id, mc, &loaders).await?;
        Ok(vs
            .into_iter()
            .map(|v| VersionSummary {
                filename: primary_file(&v).map(|f| f.filename.clone()),
                id: v.id,
                version_number: v.version_number,
                version_type: v.version_type,
                game_versions: v.game_versions,
                loaders: v.loaders,
                date: v.date_published,
            })
            .collect())
    }

    /// Pick a version of `project` for (mc, loader). `version_spec` = latest | version number | version id.
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
        let project = self.project(id).await?;
        let kind = kind_hint
            .or_else(|| kind_from_project_type(&project.project_type))
            .ok_or_else(|| {
                Error::Resolve(format!(
                    "'{}' is a {} project, which cannot be installed as content",
                    project.title, project.project_type
                ))
            })?;
        let loaders = loader_filter(kind, loader);
        let candidates = self.versions(&project.id, Some(mc), &loaders).await?;
        let chosen = if version_spec == "latest" || version_spec.is_empty() {
            pick_latest(&candidates, channel, warnings, &project.title).ok_or_else(|| {
                Error::Resolve(format!(
                    "no version of '{}' ({}) for Minecraft {mc} / {} ({})",
                    project.title,
                    project.slug.as_deref().unwrap_or(&project.id),
                    loader.as_str(),
                    kind.dir()
                ))
            })?
        } else if let Some(v) = candidates.iter().find(|v| v.version_number == version_spec || v.id == version_spec) {
            v.clone()
        } else {
            // Exact pin outside the filtered set: fetch it anyway, warn.
            let v = match self.version(version_spec).await {
                Ok(v) if v.project_id == project.id => v,
                _ => {
                    let all = self.versions(&project.id, None, &[]).await?;
                    all.into_iter()
                        .find(|v| v.version_number == version_spec)
                        .ok_or_else(|| Error::Resolve(format!("'{}' has no version '{version_spec}'", project.title)))?
                }
            };
            if !v.game_versions.iter().any(|g| g == mc) {
                warnings.push(format!(
                    "{} {} is pinned but does not list Minecraft {mc} (lists {:?})",
                    project.title, v.version_number, v.game_versions
                ));
            }
            v
        };
        Ok(to_resolved(&project, kind, &chosen))
    }

    /// Resolve a dependency reference (project and/or version id).
    pub async fn resolve_dep(
        &self,
        dep: &DepRef,
        mc: &str,
        loader: LoaderKind,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedVersion> {
        if let Some(vid) = &dep.version_id {
            let v = self.version(vid).await?;
            let project = self.project(&v.project_id).await?;
            let kind = kind_from_project_type(&project.project_type).unwrap_or(ModKind::Mod);
            if !v.game_versions.iter().any(|g| g == mc) {
                // Pinned dep version for another MC version: prefer latest compatible instead.
                if let Ok(r) = self.resolve(&project.id, "latest", Some(kind), None, mc, loader, warnings).await {
                    return Ok(r);
                }
            }
            return Ok(to_resolved(&project, kind, &v));
        }
        let pid = dep
            .project_id
            .as_deref()
            .ok_or_else(|| Error::Resolve("dependency without project or version id".into()))?;
        self.resolve(pid, "latest", None, None, mc, loader, warnings).await
    }
}

fn pick_latest(vs: &[Version], channel: Option<Channel>, warnings: &mut Vec<String>, title: &str) -> Option<Version> {
    let ch = channel.unwrap_or(Channel::Release);
    if let Some(v) = vs.iter().find(|v| ch.allows(&v.version_type)) {
        return Some(v.clone());
    }
    // Fallback: accept less stable channels, but say so.
    let v = vs.first()?.clone();
    warnings.push(format!(
        "{title}: no {:?} version available; using {} {} ({})",
        ch, title, v.version_number, v.version_type
    ));
    Some(v)
}

fn primary_file(v: &Version) -> Option<&VFile> {
    v.files.iter().find(|f| f.primary).or_else(|| v.files.first())
}

fn to_resolved(p: &Project, kind: ModKind, v: &Version) -> ResolvedVersion {
    let f = primary_file(v);
    let mut required = Vec::new();
    let mut optional = Vec::new();
    let mut incompatible = Vec::new();
    for d in &v.dependencies {
        let r = DepRef { project_id: d.project_id.clone(), version_id: d.version_id.clone() };
        match d.dependency_type.as_str() {
            "required" => required.push(r),
            "optional" => optional.push(r),
            "incompatible" => incompatible.push(r),
            _ => {}
        }
    }
    ResolvedVersion {
        source: Source::Modrinth,
        project_id: p.id.clone(),
        slug: p.slug.clone(),
        name: p.title.clone(),
        kind,
        version_id: v.id.clone(),
        version_number: v.version_number.clone(),
        version_type: v.version_type.clone(),
        filename: f.map(|f| f.filename.clone()).unwrap_or_else(|| format!("{}.jar", v.id)),
        url: f.map(|f| f.url.clone()),
        page_url: Some(format!(
            "https://modrinth.com/project/{}/version/{}",
            p.slug.as_deref().unwrap_or(&p.id),
            v.id
        )),
        sha1: f.and_then(|f| f.hashes.sha1.clone()),
        sha512: f.and_then(|f| f.hashes.sha512.clone()),
        size: f.and_then(|f| f.size),
        required,
        optional,
        incompatible,
    }
}

fn enc(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c.to_string() } else { format!("%{:02X}", c as u32) })
        .collect()
}
