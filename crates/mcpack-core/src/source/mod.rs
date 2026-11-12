//! Content sources (Modrinth, CurseForge) behind a common model.

pub mod curseforge;
pub mod modrinth;

use serde::Serialize;

use crate::pack::schema::{ModKind, Source};

/// A dependency reference as reported by a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepRef {
    pub project_id: Option<String>,
    pub version_id: Option<String>,
}

/// A concrete file version chosen for a project.
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    pub source: Source,
    pub project_id: String,
    pub slug: Option<String>,
    pub name: String,
    pub kind: ModKind,
    pub version_id: String,
    pub version_number: String,
    pub version_type: String,
    pub filename: String,
    pub url: Option<String>,
    pub page_url: Option<String>,
    pub sha1: Option<String>,
    pub sha512: Option<String>,
    pub size: Option<u64>,
    pub required: Vec<DepRef>,
    pub optional: Vec<DepRef>,
    pub incompatible: Vec<DepRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub source: Source,
    pub id: String,
    pub slug: Option<String>,
    pub title: String,
    pub description: String,
    pub kind: Option<ModKind>,
    pub downloads: u64,
    pub author: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionSummary {
    pub id: String,
    pub version_number: String,
    pub version_type: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub date: String,
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub minecraft: Option<String>,
    pub loader: Option<crate::meta::loader::LoaderKind>,
    pub kind: Option<ModKind>,
    pub limit: u32,
}
