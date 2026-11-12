//! CurseForge modpack `.zip` (manifest.json) import / export, plus format detection.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::meta::loader::LoaderKind;
use crate::mrpack::{ExportReport, ImportReport};
use crate::pack::schema::{LoaderSpec, ModSpec, PackFile, PackMeta, Source};
use crate::pack::{Pack, PACK_FILE};
use crate::util;
use crate::Launcher;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfManifest {
    minecraft: CfMinecraft,
    #[serde(default = "manifest_type")]
    manifest_type: String,
    #[serde(default = "one")]
    manifest_version: u32,
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    files: Vec<CfFileRef>,
    #[serde(default = "overrides_dir")]
    overrides: String,
}

fn manifest_type() -> String {
    "minecraftModpack".into()
}
fn one() -> u32 {
    1
}
fn overrides_dir() -> String {
    "overrides".into()
}
fn yes() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfMinecraft {
    version: String,
    #[serde(default)]
    mod_loaders: Vec<CfLoader>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CfLoader {
    id: String,
    #[serde(default)]
    primary: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct CfFileRef {
    #[serde(rename = "projectID")]
    project_id: u64,
    #[serde(rename = "fileID")]
    file_id: u64,
    #[serde(default = "yes")]
    required: bool,
}

/// Modpack archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackFormat {
    Mrpack,
    Curseforge,
}

impl PackFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "mrpack" | "modrinth" => Some(Self::Mrpack),
            "curseforge" | "cf" | "zip" => Some(Self::Curseforge),
            _ => None,
        }
    }

    /// Detect by looking inside the archive.
    pub fn detect(file: &Path) -> Result<Self> {
        let f = std::fs::File::open(file).ctx(file)?;
        let zip = zip::ZipArchive::new(f)?;
        let names: Vec<&str> = zip.file_names().collect();
        if names.contains(&"modrinth.index.json") {
            Ok(Self::Mrpack)
        } else if names.contains(&"manifest.json") {
            Ok(Self::Curseforge)
        } else {
            Err(Error::Invalid(format!(
                "{} is neither a Modrinth .mrpack (modrinth.index.json) nor a CurseForge modpack zip (manifest.json)",
                file.display()
            )))
        }
    }
}

impl Launcher {
    /// Import a modpack archive (.mrpack or CurseForge .zip, auto-detected) into a new pack.
    pub async fn import_pack(&self, file: &Path, dest: &Path, resolve: bool) -> Result<ImportReport> {
        match PackFormat::detect(file)? {
            PackFormat::Mrpack => self.import_mrpack(file, dest, resolve).await,
            PackFormat::Curseforge => self.import_cfzip(file, dest, resolve).await,
        }
    }

    /// Export a profile as `.mrpack` or a CurseForge modpack `.zip`.
    pub async fn export_pack(&self, dir: &Path, profile: &str, format: PackFormat, out: Option<&Path>) -> Result<ExportReport> {
        match format {
            PackFormat::Mrpack => self.export_mrpack(dir, profile, out).await,
            PackFormat::Curseforge => self.export_cfzip(dir, profile, out).await,
        }
    }

    /// Import a CurseForge modpack zip. Files become pinned CurseForge specs
    /// (resolving them needs a CurseForge API key).
    pub async fn import_cfzip(&self, file: &Path, dest: &Path, resolve: bool) -> Result<ImportReport> {
        let dest = std::path::absolute(dest).unwrap_or_else(|_| dest.to_path_buf());
        if dest.join(PACK_FILE).exists() {
            return Err(Error::Invalid(format!("{} already contains a pack", dest.display())));
        }
        let f = std::fs::File::open(file).ctx(file)?;
        let mut zip = zip::ZipArchive::new(f)?;
        let manifest: CfManifest = {
            let mut e = zip.by_name("manifest.json")?;
            let mut s = String::new();
            e.read_to_string(&mut s).ctx(file)?;
            serde_json::from_str(&s)?
        };
        let mut warnings = Vec::new();
        let loader_id = manifest
            .minecraft
            .mod_loaders
            .iter()
            .find(|l| l.primary)
            .or_else(|| manifest.minecraft.mod_loaders.first())
            .map(|l| l.id.clone())
            .unwrap_or_default();
        let (loader, lv) = match loader_id.split_once('-') {
            Some(("fabric", v)) => (LoaderKind::Fabric, v.to_string()),
            Some(("quilt", v)) => (LoaderKind::Quilt, v.to_string()),
            Some(("forge", _)) | Some(("neoforge", _)) => {
                return Err(Error::Invalid("Forge/NeoForge packs are not supported yet".into()))
            }
            _ if loader_id.is_empty() => (LoaderKind::Vanilla, "latest".to_string()),
            _ => {
                warnings.push(format!("unknown loader '{loader_id}', using fabric latest"));
                (LoaderKind::Fabric, "latest".to_string())
            }
        };
        let mut mods = Vec::new();
        for f in &manifest.files {
            if !f.required {
                warnings.push(format!("optional CurseForge file {}/{} skipped", f.project_id, f.file_id));
                continue;
            }
            mods.push(ModSpec {
                source: Source::Curseforge,
                id: Some(f.project_id.to_string()),
                version: Some(f.file_id.to_string()),
                ..Default::default()
            });
        }
        util::ensure_dir(&dest)?;
        let prefix = format!("{}/", manifest.overrides.trim_end_matches('/'));
        let mut overrides = 0;
        for i in 0..zip.len() {
            let mut e = zip.by_index(i)?;
            let name = e.name().to_string();
            let Some(rel) = name.strip_prefix(&prefix) else { continue };
            if e.is_dir() || rel.is_empty() || rel.contains("..") {
                continue;
            }
            let target = dest.join("overrides").join(rel);
            util::ensure_parent(&target)?;
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).ctx(&target)?;
            std::fs::write(&target, buf).ctx(&target)?;
            overrides += 1;
        }
        let pack_file = PackFile {
            pack: PackMeta {
                name: manifest.name.clone(),
                version: Some(manifest.version.clone()).filter(|v| !v.is_empty()),
                description: Some(format!("by {}", manifest.author)).filter(|_| !manifest.author.is_empty()),
                minecraft: manifest.minecraft.version.clone(),
                loader: LoaderSpec { kind: loader, version: lv },
            },
            mods,
            ..Default::default()
        };
        let mut text = String::from("# Imported from a CurseForge modpack by mcpack.\n");
        text.push_str(&toml::to_string_pretty(&pack_file)?);
        std::fs::write(dest.join(PACK_FILE), text).ctx(&dest)?;
        std::fs::write(dest.join(".gitignore"), "run/\n").ctx(&dest)?;
        let n = pack_file.mods.len();
        if resolve {
            if let Err(e) = self.resolve(&dest, Default::default()).await {
                warnings.push(format!("resolve failed: {e}"));
            }
        }
        Ok(ImportReport {
            pack_dir: dest.display().to_string(),
            name: manifest.name,
            minecraft: manifest.minecraft.version,
            loader,
            mods: n,
            overrides,
            warnings,
        })
    }

    /// Export a CurseForge modpack zip. CurseForge entries are listed in manifest.json; local
    /// files are bundled in overrides; content from other sources is reported (CurseForge
    /// manifests can only reference CurseForge files — use .mrpack for Modrinth content).
    pub async fn export_cfzip(&self, dir: &Path, profile: &str, out: Option<&Path>) -> Result<ExportReport> {
        let dir = std::path::absolute(crate::pack::pack_dir(dir)).unwrap_or_else(|_| dir.to_path_buf());
        let pack = Pack::load(&dir)?;
        let (lock, _) = self.ensure_lock(&pack).await?;
        let running = self.find_session(&dir, profile).is_some();
        crate::instance::sync(&self.ctx, &pack, &lock, profile, Some(false), running).await?;
        let eff = pack.effective(profile)?;
        let inst = crate::paths::instance_dir(&dir, profile);
        let mut warnings = Vec::new();
        let mut files = Vec::new();
        let mut overrides: BTreeMap<String, PathBuf> = BTreeMap::new();
        for e in lock.closure(&eff.top_fingerprints()) {
            match e.source {
                Source::Curseforge => match (e.project_id.parse::<u64>(), e.version_id.parse::<u64>()) {
                    (Ok(p), Ok(f)) => files.push(CfFileRef { project_id: p, file_id: f, required: true }),
                    _ => warnings.push(format!("{}: bad CurseForge ids", e.name)),
                },
                Source::File => {
                    let p = util::resolve_rel(&dir, e.path.as_deref().unwrap_or(""));
                    if p.exists() {
                        overrides.insert(format!("{}/{}", e.kind.dir(), e.filename), p);
                    }
                }
                _ => warnings.push(format!(
                    "{} ({}) not included: CurseForge manifests can only reference CurseForge files (export as mrpack for Modrinth content)",
                    e.name,
                    e.source.as_str()
                )),
            }
        }
        let mut roots = vec![dir.join("overrides")];
        for n in pack.profile_chain_names(profile)? {
            roots.push(dir.join("profiles").join(n).join("overrides"));
        }
        for r in roots {
            for rel in util::walk_files(&r)? {
                overrides.insert(rel.clone(), r.join(&rel));
            }
        }
        for file in eff.config.keys() {
            let p = inst.join(file);
            if p.exists() {
                overrides.insert(file.replace('\\', "/"), p);
            }
        }
        if !eff.options.is_empty() && inst.join("options.txt").exists() {
            overrides.insert("options.txt".into(), inst.join("options.txt"));
        }
        let loader_id = match lock.loader {
            LoaderKind::Fabric => Some(format!("fabric-{}", lock.loader_version)),
            LoaderKind::Quilt => Some(format!("quilt-{}", lock.loader_version)),
            LoaderKind::Vanilla => None,
        };
        let manifest = CfManifest {
            minecraft: CfMinecraft {
                version: eff.minecraft.clone(),
                mod_loaders: loader_id.into_iter().map(|id| CfLoader { id, primary: true }).collect(),
            },
            manifest_type: manifest_type(),
            manifest_version: 1,
            name: pack.file.pack.name.clone(),
            version: pack.file.pack.version.clone().unwrap_or_else(|| "1.0.0".into()),
            author: String::new(),
            files,
            overrides: overrides_dir(),
        };
        let out = out
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.join(format!("{}-{}-curseforge.zip", util::safe_component(&pack.file.pack.name), profile)));
        util::ensure_parent(&out)?;
        let f = std::fs::File::create(&out).ctx(&out)?;
        let mut zip = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("manifest.json", opts)?;
        zip.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes()).ctx(&out)?;
        for (rel, p) in &overrides {
            zip.start_file(format!("overrides/{rel}"), opts)?;
            let bytes = std::fs::read(p).ctx(p)?;
            zip.write_all(&bytes).ctx(&out)?;
        }
        zip.finish()?;
        Ok(ExportReport { path: out.display().to_string(), files: manifest.files.len(), overrides: overrides.len(), warnings })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let m: CfManifest = serde_json::from_str(
            r#"{"minecraft":{"version":"1.20.1","modLoaders":[{"id":"fabric-0.16.14","primary":true}]},
                "manifestType":"minecraftModpack","manifestVersion":1,"name":"X","version":"1","author":"a",
                "files":[{"projectID":238222,"fileID":4712866,"required":true}],"overrides":"overrides"}"#,
        )
        .unwrap();
        assert_eq!(m.files[0].project_id, 238222);
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"projectID\":238222") && s.contains("\"modLoaders\""));
    }
}
