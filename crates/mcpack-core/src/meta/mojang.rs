use std::collections::{BTreeMap, HashMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::Platform;
use crate::error::Result;
use crate::util;

pub const VERSION_MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const RESOURCES_URL: &str = "https://resources.download.minecraft.net";
pub const LIBRARIES_URL: &str = "https://libraries.minecraft.net/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionManifest {
    pub latest: Latest,
    pub versions: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub release_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherits_from: Option<String>,
    #[serde(default)]
    pub main_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Arguments>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_index: Option<AssetIndexRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<VersionDownloads>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_version: Option<JavaVersionRef>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Arg>,
    #[serde(default)]
    pub jvm: Vec<Arg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Arg {
    Plain(String),
    Ruled { rules: Vec<Rule>, value: ArgValue },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgValue {
    One(String),
    Many(Vec<String>),
}

impl ArgValue {
    pub fn values(&self) -> Vec<String> {
        match self {
            ArgValue::One(s) => vec![s.clone()],
            ArgValue::Many(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<OsRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<HashMap<String, bool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Library {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<LibDownloads>,
    /// Maven repository base (Fabric/Quilt style libraries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<Rule>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub natives: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LibDownloads {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    pub id: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionDownloads {
    pub client: Artifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersionRef {
    pub component: String,
    pub major_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetIndex {
    pub objects: BTreeMap<String, AssetObject>,
    #[serde(default)]
    pub map_to_resources: bool,
    #[serde(default, rename = "virtual")]
    pub is_virtual: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

/// Feature flags evaluated in argument rules.
#[derive(Debug, Clone, Default)]
pub struct Features(pub HashSet<String>);

impl Features {
    pub fn with(mut self, f: &str, on: bool) -> Self {
        if on {
            self.0.insert(f.to_string());
        }
        self
    }
}

impl Rule {
    fn matches(&self, p: &Platform, features: &Features) -> bool {
        if let Some(os) = &self.os {
            if let Some(name) = &os.name {
                if name != p.os {
                    return false;
                }
            }
            if let Some(arch) = &os.arch {
                let ok = match arch.as_str() {
                    "x86" => p.arch == "x86",
                    "x86_64" | "x64" => p.arch == "x86_64",
                    "arm64" | "aarch64" => p.arch == "aarch64",
                    other => other == p.arch,
                };
                if !ok {
                    return false;
                }
            }
            if let Some(ver) = &os.version {
                // Only matters for ancient OS X / Windows 10 tricks; best-effort.
                if let (Ok(re), Some(v)) = (Regex::new(ver), os_version()) {
                    if !re.is_match(&v) {
                        return false;
                    }
                }
            }
        }
        if let Some(fs) = &self.features {
            for (k, v) in fs {
                if features.0.contains(k) != *v {
                    return false;
                }
            }
        }
        true
    }
}

fn os_version() -> Option<String> {
    if cfg!(windows) {
        Some("10.0".into())
    } else {
        None
    }
}

/// Mojang rule semantics: start disallowed, last matching rule wins. No rules = allowed.
pub fn rules_allow(rules: Option<&[Rule]>, p: &Platform, features: &Features) -> bool {
    let rules = match rules {
        None => return true,
        Some(r) if r.is_empty() => return true,
        Some(r) => r,
    };
    let mut allowed = false;
    for r in rules {
        if r.matches(p, features) {
            allowed = r.action == "allow";
        }
    }
    allowed
}

/// A resolved library file to download / put on the classpath.
#[derive(Debug, Clone)]
pub struct LibFile {
    pub name: String,
    pub path: String,
    pub url: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    /// Old-style natives jar that must be extracted.
    pub extract_natives: bool,
}

impl Library {
    /// Files this library contributes on the given platform (main artifact and/or natives).
    pub fn files(&self, p: &Platform) -> Result<Vec<LibFile>> {
        let mut out = Vec::new();
        if !rules_allow(self.rules.as_deref(), p, &Features::default()) {
            return Ok(out);
        }
        // Main artifact.
        let artifact = self.downloads.as_ref().and_then(|d| d.artifact.as_ref());
        match artifact {
            Some(a) => {
                let path = match &a.path {
                    Some(p) => p.clone(),
                    None => util::maven_path(&self.name)?,
                };
                if !a.url.is_empty() {
                    out.push(LibFile {
                        name: self.name.clone(),
                        path,
                        url: a.url.clone(),
                        sha1: a.sha1.clone(),
                        size: a.size,
                        extract_natives: false,
                    });
                }
            }
            None if self.natives.is_none() => {
                let path = util::maven_path(&self.name)?;
                let base = self.url.clone().unwrap_or_else(|| LIBRARIES_URL.to_string());
                let base = if base.ends_with('/') { base } else { format!("{base}/") };
                out.push(LibFile {
                    name: self.name.clone(),
                    url: format!("{base}{path}"),
                    path,
                    sha1: self.sha1.clone(),
                    size: self.size,
                    extract_natives: false,
                });
            }
            None => {}
        }
        // Legacy natives classifier.
        if let Some(natives) = &self.natives {
            if let Some(classifier) = natives.get(p.os) {
                let classifier = classifier.replace("${arch}", if p.is_64bit() { "64" } else { "32" });
                if let Some(a) = self
                    .downloads
                    .as_ref()
                    .and_then(|d| d.classifiers.as_ref())
                    .and_then(|c| c.get(&classifier))
                {
                    let path = match &a.path {
                        Some(p) => p.clone(),
                        None => util::maven_path(&format!("{}:{classifier}", self.name))?,
                    };
                    out.push(LibFile {
                        name: format!("{}:{classifier}", self.name),
                        path,
                        url: a.url.clone(),
                        sha1: a.sha1.clone(),
                        size: a.size,
                        extract_natives: true,
                    });
                }
            }
        }
        Ok(out)
    }
}

/// Merge a child profile (e.g. Fabric) onto its parent vanilla version.
/// Child libraries win over parent libraries with the same `group:artifact[:classifier]`.
pub fn merge_versions(parent: &VersionJson, child: &VersionJson) -> VersionJson {
    let mut seen = HashSet::new();
    let mut libraries = Vec::new();
    for lib in child.libraries.iter().chain(parent.libraries.iter()) {
        let key = util::maven_key(&lib.name);
        // natives-style keys may legitimately repeat with different rules; keep those.
        let unique_key = if lib.rules.is_some() { format!("{key}#{:?}", lib.rules.as_ref().map(|r| r.len())) } else { key };
        if seen.insert(unique_key) {
            libraries.push(lib.clone());
        }
    }
    let arguments = match (&parent.arguments, &child.arguments) {
        (None, None) => None,
        (p, c) => {
            let mut a = p.clone().unwrap_or_default();
            if let Some(c) = c {
                a.game.extend(c.game.iter().cloned());
                a.jvm.extend(c.jvm.iter().cloned());
            }
            Some(a)
        }
    };
    VersionJson {
        id: child.id.clone(),
        inherits_from: None,
        main_class: child.main_class.clone().or_else(|| parent.main_class.clone()),
        arguments,
        minecraft_arguments: child.minecraft_arguments.clone().or_else(|| parent.minecraft_arguments.clone()),
        libraries,
        asset_index: child.asset_index.clone().or_else(|| parent.asset_index.clone()),
        assets: child.assets.clone().or_else(|| parent.assets.clone()),
        downloads: child.downloads.clone().or_else(|| parent.downloads.clone()),
        java_version: child.java_version.clone().or_else(|| parent.java_version.clone()),
        kind: parent.kind.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        let p = Platform { os: "windows", arch: "x86_64" };
        let f = Features::default();
        let only_osx = vec![Rule {
            action: "allow".into(),
            os: Some(OsRule { name: Some("osx".into()), arch: None, version: None }),
            features: None,
        }];
        assert!(!rules_allow(Some(&only_osx), &p, &f));
        let all_but_osx = vec![
            Rule { action: "allow".into(), os: None, features: None },
            Rule {
                action: "disallow".into(),
                os: Some(OsRule { name: Some("osx".into()), arch: None, version: None }),
                features: None,
            },
        ];
        assert!(rules_allow(Some(&all_but_osx), &p, &f));
        let feat = vec![Rule {
            action: "allow".into(),
            os: None,
            features: Some(HashMap::from([("has_custom_resolution".to_string(), true)])),
        }];
        assert!(!rules_allow(Some(&feat), &p, &f));
        assert!(rules_allow(Some(&feat), &p, &Features::default().with("has_custom_resolution", true)));
    }
}
