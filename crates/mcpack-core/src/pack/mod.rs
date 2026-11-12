//! Packs: load, effective profiles, editing.

pub mod edit;
pub mod lock;
pub mod schema;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, IoContext, Result};
use schema::*;

pub const PACK_FILE: &str = "pack.toml";
pub const DEFAULT_PROFILE: &str = "default";

/// A loaded pack directory.
#[derive(Debug, Clone)]
pub struct Pack {
    pub dir: PathBuf,
    pub file: PackFile,
}

impl Pack {
    /// Load from a pack directory or a path to `pack.toml`.
    pub fn load(path: impl AsRef<Path>) -> Result<Pack> {
        let dir = pack_dir(path.as_ref());
        let file_path = dir.join(PACK_FILE);
        if !file_path.exists() {
            return Err(Error::NotFound(format!("no {PACK_FILE} in {}", dir.display())));
        }
        let text = std::fs::read_to_string(&file_path).ctx(&file_path)?;
        let file: PackFile = toml::from_str(&text)?;
        let pack = Pack { dir, file };
        pack.validate()?;
        Ok(pack)
    }

    pub fn validate(&self) -> Result<()> {
        let f = &self.file;
        if f.pack.minecraft.trim().is_empty() {
            return Err(Error::Invalid("pack.minecraft must be set".into()));
        }
        for (name, p) in &f.profiles {
            if let Some(parent) = &p.extends {
                if parent != DEFAULT_PROFILE && !f.profiles.contains_key(parent) {
                    return Err(Error::Invalid(format!("profile '{name}' extends unknown profile '{parent}'")));
                }
            }
        }
        // Cycle check.
        for name in f.profiles.keys() {
            self.profile_chain(name)?;
        }
        for m in f.mods.iter().chain(f.profiles.values().flat_map(|p| p.mods.iter())) {
            validate_spec(m)?;
        }
        Ok(())
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(PACK_FILE)
    }

    pub fn profile_names(&self) -> Vec<String> {
        let mut v = vec![DEFAULT_PROFILE.to_string()];
        v.extend(self.file.profiles.keys().filter(|k| *k != DEFAULT_PROFILE).cloned());
        v
    }

    /// Names of the profile chain from base-most to `name` (excluding the implicit base).
    pub fn profile_chain_names(&self, name: &str) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let mut cur = Some(name.to_string());
        while let Some(n) = cur {
            if n == DEFAULT_PROFILE && !self.file.profiles.contains_key(DEFAULT_PROFILE) {
                break;
            }
            let p = self.file.profiles.get(&n).ok_or_else(|| Error::NotFound(format!("profile '{n}'")))?;
            names.push(n.clone());
            cur = p.extends.clone().filter(|e| e != &n);
            if names.len() > 32 {
                return Err(Error::Invalid(format!("profile inheritance cycle involving '{name}'")));
            }
        }
        names.reverse();
        Ok(names)
    }

    /// Profile chain from base-most to `name` (excluding the implicit base).
    fn profile_chain(&self, name: &str) -> Result<Vec<&Profile>> {
        let mut chain = Vec::new();
        let mut cur = Some(name.to_string());
        let mut guard = 0;
        while let Some(n) = cur {
            if n == DEFAULT_PROFILE && !self.file.profiles.contains_key(DEFAULT_PROFILE) {
                break;
            }
            let p = self
                .file
                .profiles
                .get(&n)
                .ok_or_else(|| Error::NotFound(format!("profile '{n}' (available: {})", self.profile_names().join(", "))))?;
            chain.push(p);
            cur = p.extends.clone().filter(|e| e != &n);
            guard += 1;
            if guard > 32 {
                return Err(Error::Invalid(format!("profile inheritance cycle involving '{name}'")));
            }
        }
        chain.reverse();
        Ok(chain)
    }

    /// Fully merged settings for a profile.
    pub fn effective(&self, profile: &str) -> Result<EffectiveProfile> {
        let f = &self.file;
        let chain = self.profile_chain(profile)?;
        let mut jvm = f.jvm.clone();
        let mut options = f.options.clone();
        let mut config = f.config.clone();
        let mut mods: Vec<ModSpec> = f.mods.clone();
        let mut agent = None;
        let mut username = f.dev.username.clone();
        for p in chain {
            jvm = jvm.overlay(&p.jvm);
            for (k, v) in &p.options {
                options.insert(k.clone(), v.clone());
            }
            for (file, table) in &p.config {
                let entry = config.entry(file.clone()).or_default();
                deep_merge(entry, table);
            }
            mods.retain(|m| !p.disable.iter().any(|d| m.matches(d)));
            for m in &p.mods {
                // A profile spec for the same project replaces the base one.
                mods.retain(|b| !(b.source == m.source && b.ident().eq_ignore_ascii_case(&m.ident())));
                mods.push(m.clone());
            }
            if p.agent.is_some() {
                agent = p.agent;
            }
            if p.username.is_some() {
                username = p.username.clone();
            }
        }
        let loader = f.pack.loader.kind;
        Ok(EffectiveProfile {
            name: profile.to_string(),
            minecraft: f.pack.minecraft.clone(),
            loader: f.pack.loader.clone(),
            jvm,
            options,
            config,
            mods,
            agent: agent.unwrap_or(loader != LoaderKind::Vanilla),
            username,
        })
    }

    /// Every top-level spec across base + all profiles (deduped by fingerprint).
    pub fn all_specs(&self) -> Vec<ModSpec> {
        let mut out: Vec<ModSpec> = Vec::new();
        for m in self.file.mods.iter().chain(self.file.profiles.values().flat_map(|p| p.mods.iter())) {
            if !out.iter().any(|o| o == m) {
                out.push(m.clone());
            }
        }
        out
    }

    /// Fingerprint of everything that affects `pack.lock`.
    pub fn resolution_inputs(&self) -> String {
        #[derive(Serialize)]
        struct Inputs<'a> {
            mc: &'a str,
            loader: &'a LoaderSpec,
            specs: Vec<String>,
        }
        let mut specs: Vec<String> = self.all_specs().iter().map(|s| s.resolution_fingerprint()).collect();
        specs.sort();
        crate::util::fingerprint(&Inputs { mc: &self.file.pack.minecraft, loader: &self.file.pack.loader, specs })
    }
}

fn validate_spec(m: &ModSpec) -> Result<()> {
    match m.source {
        Source::Modrinth | Source::Curseforge if m.id.as_deref().unwrap_or("").is_empty() => {
            Err(Error::Invalid(format!("{} mod entry needs `id`", m.source.as_str())))
        }
        Source::Url if m.url.as_deref().unwrap_or("").is_empty() => Err(Error::Invalid("url entry needs `url`".into())),
        Source::File if m.path.as_deref().unwrap_or("").is_empty() => {
            Err(Error::Invalid("file entry needs `path`".into()))
        }
        _ => Ok(()),
    }
}

/// Merged view of a profile.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveProfile {
    pub name: String,
    pub minecraft: String,
    pub loader: LoaderSpec,
    pub jvm: JvmSettings,
    pub options: BTreeMap<String, toml::Value>,
    pub config: BTreeMap<String, toml::Table>,
    pub mods: Vec<ModSpec>,
    pub agent: bool,
    pub username: Option<String>,
}

impl EffectiveProfile {
    pub fn top_fingerprints(&self) -> Vec<String> {
        self.mods.iter().map(|m| m.resolution_fingerprint()).collect()
    }
}

pub fn deep_merge(into: &mut toml::Table, from: &toml::Table) {
    for (k, v) in from {
        match (into.get_mut(k), v) {
            (Some(toml::Value::Table(a)), toml::Value::Table(b)) => deep_merge(a, b),
            _ => {
                into.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Accept either a directory or a `pack.toml` path.
pub fn pack_dir(p: &Path) -> PathBuf {
    if p.file_name().is_some_and(|n| n == PACK_FILE) || p.extension().is_some_and(|e| e == "toml") {
        p.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    } else {
        p.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[pack]
name = "t"
minecraft = "1.21.1"
loader = { kind = "fabric" }

[jvm]
memory = "4G"

[options]
renderDistance = 12

[config."sodium-options.json"]
"quality.weather_quality" = "FAST"

[[mods]]
id = "sodium"

[[mods]]
id = "iris"

[profiles.lowend]
disable = ["iris"]
jvm = { memory = "2G" }
options = { renderDistance = 4 }

[[profiles.lowend.mods]]
id = "ferrite-core"

[profiles.test]
extends = "lowend"
agent = true
"#;

    fn load() -> Pack {
        let file: PackFile = toml::from_str(SAMPLE).unwrap();
        let p = Pack { dir: PathBuf::from("."), file };
        p.validate().unwrap();
        p
    }

    #[test]
    fn effective_profiles() {
        let p = load();
        let base = p.effective("default").unwrap();
        assert_eq!(base.mods.len(), 2);
        assert_eq!(base.jvm.memory.as_deref(), Some("4G"));
        assert!(base.agent);
        let low = p.effective("lowend").unwrap();
        let ids: Vec<_> = low.mods.iter().map(|m| m.ident()).collect();
        assert_eq!(ids, vec!["sodium", "ferrite-core"]);
        assert_eq!(low.jvm.memory.as_deref(), Some("2G"));
        assert_eq!(low.options["renderDistance"].as_integer(), Some(4));
        let test = p.effective("test").unwrap();
        assert_eq!(test.mods.len(), 2);
        assert!(p.effective("nope").is_err());
    }

    #[test]
    fn inputs_change_with_specs() {
        let mut p = load();
        let a = p.resolution_inputs();
        p.file.mods[0].version = Some("0.5.0".into());
        assert_ne!(a, p.resolution_inputs());
    }
}
