//! `pack.toml` schema: the declarative source of truth for a modpack.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::meta::loader::LoaderKind;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackFile {
    pub pack: PackMeta,
    #[serde(default, skip_serializing_if = "JvmSettings::is_empty")]
    pub jvm: JvmSettings,
    /// `options.txt` keys (e.g. `renderDistance = 8`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, toml::Value>,
    /// Structured config patches: file path (relative to the instance) → keys to set.
    /// Nested tables and dotted keys (`"a.b" = 1`) both address nested fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, toml::Table>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<ModSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default, skip_serializing_if = "DevSettings::is_default")]
    pub dev: DevSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub minecraft: String,
    #[serde(default)]
    pub loader: LoaderSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoaderSpec {
    #[serde(default)]
    pub kind: LoaderKind,
    /// `latest` or an exact loader version.
    #[serde(default = "latest")]
    pub version: String,
}

impl Default for LoaderSpec {
    fn default() -> Self {
        Self { kind: LoaderKind::Vanilla, version: latest() }
    }
}

fn latest() -> String {
    "latest".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct JvmSettings {
    /// Max heap, e.g. `4G`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// Min heap, e.g. `1G`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_memory: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub game_args: Vec<String>,
    /// Explicit java executable (otherwise Mojang runtime for the MC version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

impl JvmSettings {
    pub fn is_empty(&self) -> bool {
        *self == JvmSettings::default()
    }

    /// Overlay `other` on top of self (other wins; args are appended).
    pub fn overlay(&self, other: &JvmSettings) -> JvmSettings {
        let mut args = self.args.clone();
        args.extend(other.args.iter().cloned());
        let mut game_args = self.game_args.clone();
        game_args.extend(other.game_args.iter().cloned());
        JvmSettings {
            memory: other.memory.clone().or_else(|| self.memory.clone()),
            min_memory: other.min_memory.clone().or_else(|| self.min_memory.clone()),
            args,
            game_args,
            java: other.java.clone().or_else(|| self.java.clone()),
            width: other.width.or(self.width),
            height: other.height.or(self.height),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    #[default]
    Modrinth,
    Curseforge,
    Url,
    File,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Modrinth => "modrinth",
            Source::Curseforge => "curseforge",
            Source::Url => "url",
            Source::File => "file",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "modrinth" | "mr" => Some(Source::Modrinth),
            "curseforge" | "cf" => Some(Source::Curseforge),
            "url" => Some(Source::Url),
            "file" | "local" => Some(Source::File),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ModKind {
    #[default]
    Mod,
    Resourcepack,
    Shaderpack,
    Datapack,
}

impl ModKind {
    /// Directory inside the instance.
    pub fn dir(&self) -> &'static str {
        match self {
            ModKind::Mod => "mods",
            ModKind::Resourcepack => "resourcepacks",
            ModKind::Shaderpack => "shaderpacks",
            ModKind::Datapack => "datapacks",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "mod" | "mods" => Some(ModKind::Mod),
            "resourcepack" | "resourcepacks" | "resource_pack" => Some(ModKind::Resourcepack),
            "shaderpack" | "shader" | "shaders" | "shaderpacks" => Some(ModKind::Shaderpack),
            "datapack" | "datapacks" | "data_pack" => Some(ModKind::Datapack),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Release,
    Beta,
    Alpha,
}

impl Channel {
    pub fn allows(&self, version_type: &str) -> bool {
        let rank = match version_type {
            "release" => 0,
            "beta" => 1,
            _ => 2,
        };
        let max = match self {
            Channel::Release => 0,
            Channel::Beta => 1,
            Channel::Alpha => 2,
        };
        rank <= max
    }
}

/// One requested content entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ModSpec {
    #[serde(default)]
    pub source: Source,
    /// Modrinth slug/project id, CurseForge slug/numeric id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `latest` (default) or an exact version number / version id / file id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Content kind; inferred from the project type when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ModKind>,
    /// Lowest accepted release channel (default: release, falls back with a warning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    /// For `source = "url"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// For `source = "file"`: path relative to the pack directory (or absolute).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Also install optional dependencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional_deps: Option<bool>,
}

impl ModSpec {
    /// Human/stable identifier for matching (`remove`, `disable`).
    pub fn ident(&self) -> String {
        match self.source {
            Source::Modrinth | Source::Curseforge => self.id.clone().unwrap_or_default(),
            Source::Url => self.filename.clone().or_else(|| self.url.clone()).unwrap_or_default(),
            Source::File => self.path.clone().unwrap_or_default(),
        }
    }

    pub fn matches(&self, ident: &str) -> bool {
        let ident = ident.trim();
        let (src, id) = match ident.split_once(':') {
            Some((s, i)) if Source::parse(s).is_some() => (Source::parse(s), i),
            _ => (None, ident),
        };
        if let Some(s) = src {
            if s != self.source {
                return false;
            }
        }
        self.ident().eq_ignore_ascii_case(id)
            || self.filename.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(id))
    }

    /// Fingerprint of everything that affects resolution of this spec.
    pub fn resolution_fingerprint(&self) -> String {
        crate::util::fingerprint(self)
    }

    pub fn version_spec(&self) -> &str {
        self.version.as_deref().unwrap_or("latest")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    /// Another profile to inherit from (default: the base pack settings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    #[serde(default, skip_serializing_if = "JvmSettings::is_empty")]
    pub jvm: JvmSettings,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, toml::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, toml::Table>,
    /// Extra content only in this profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<ModSpec>,
    /// Base mods to leave out in this profile (by id/slug/filename).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disable: Vec<String>,
    /// Inject the mcpack automation agent (default: true for fabric/quilt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConfigReload {
    /// Restart the client when config files change (safe default: most mods read config at startup).
    #[default]
    Restart,
    /// Trigger an in-game resource reload (works for mods that reload config with resources).
    Resources,
    /// Just sync files; do nothing to the running client.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DevSettings {
    #[serde(default)]
    pub config_reload: ConfigReload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Do not apply automation-friendly options.txt defaults.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_automation_defaults: bool,
}

impl DevSettings {
    pub fn is_default(&self) -> bool {
        *self == DevSettings::default()
    }
}
