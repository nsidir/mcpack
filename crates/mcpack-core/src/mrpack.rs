//! Modrinth `.mrpack` import / export.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::http::DownloadItem;
use crate::meta::loader::LoaderKind;
use crate::pack::schema::{LoaderSpec, ModKind, ModSpec, PackFile, PackMeta, Source};
use crate::pack::{Pack, PACK_FILE};
use crate::util;
use crate::Launcher;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    format_version: u32,
    game: String,
    version_id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    files: Vec<IndexFile>,
    dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    path: String,
    hashes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
    downloads: Vec<String>,
    file_size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportReport {
    pub path: String,
    pub files: usize,
    pub overrides: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub pack_dir: String,
    pub name: String,
    pub minecraft: String,
    pub loader: LoaderKind,
    pub mods: usize,
    pub overrides: usize,
    pub warnings: Vec<String>,
}

impl Launcher {
    /// Export a profile as a Modrinth `.mrpack`.
    pub async fn export_mrpack(&self, dir: &Path, profile: &str, out: Option<&Path>) -> Result<ExportReport> {
        let dir = std::path::absolute(crate::pack::pack_dir(dir)).unwrap_or_else(|_| dir.to_path_buf());
        let pack = Pack::load(&dir)?;
        let (lock, _) = self.ensure_lock(&pack).await?;
        // Sync so generated configs exist.
        let running = self.find_session(&dir, profile).is_some();
        crate::instance::sync(&self.ctx, &pack, &lock, profile, Some(false), running).await?;
        let eff = pack.effective(profile)?;
        let inst = crate::paths::instance_dir(&dir, profile);
        let mut warnings = Vec::new();
        let mut files = Vec::new();
        let mut extra_overrides: Vec<(String, PathBuf)> = Vec::new();

        for e in lock.closure(&eff.top_fingerprints()) {
            let rel = format!("{}/{}", e.kind.dir(), e.filename);
            match (&e.url, &e.sha1) {
                (Some(url), Some(sha1)) if e.source != Source::Curseforge => {
                    let stored = self.ctx.paths.stored_file(sha1, &e.filename);
                    if !stored.exists() {
                        self.ctx
                            .http
                            .download(&DownloadItem { url: url.clone(), dest: stored.clone(), sha1: Some(sha1.clone()), size: e.size })
                            .await?;
                    }
                    let sha512 = match &e.sha512 {
                        Some(s) => s.clone(),
                        None => util::sha512_file(&stored)?,
                    };
                    let size = std::fs::metadata(&stored).map(|m| m.len()).unwrap_or(0);
                    files.push(IndexFile {
                        path: rel,
                        hashes: BTreeMap::from([("sha1".into(), sha1.clone()), ("sha512".into(), sha512)]),
                        env: None,
                        downloads: vec![url.clone()],
                        file_size: size,
                    });
                }
                _ => {
                    // Local files are bundled; CurseForge files must not be redistributed.
                    if e.source == Source::File {
                        let p = util::resolve_rel(&dir, e.path.as_deref().unwrap_or(""));
                        if p.exists() {
                            extra_overrides.push((rel, p));
                            continue;
                        }
                    }
                    warnings.push(format!(
                        "{} ({}) not included: {}",
                        e.name,
                        e.source.as_str(),
                        if e.source == Source::Curseforge { "CurseForge files cannot be listed in .mrpack" } else { "file unavailable" }
                    ));
                }
            }
        }

        let mut deps = BTreeMap::from([("minecraft".to_string(), eff.minecraft.clone())]);
        match lock.loader {
            LoaderKind::Fabric => {
                deps.insert("fabric-loader".into(), lock.loader_version.clone());
            }
            LoaderKind::Quilt => {
                deps.insert("quilt-loader".into(), lock.loader_version.clone());
            }
            LoaderKind::Vanilla => {}
        }
        let index = Index {
            format_version: 1,
            game: "minecraft".into(),
            version_id: pack.file.pack.version.clone().unwrap_or_else(|| "1.0.0".into()),
            name: pack.file.pack.name.clone(),
            summary: pack.file.pack.description.clone(),
            files,
            dependencies: deps,
        };

        let out = out
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.join(format!("{}-{}.mrpack", util::safe_component(&pack.file.pack.name), profile)));
        util::ensure_parent(&out)?;
        let f = std::fs::File::create(&out).ctx(&out)?;
        let mut zip = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("modrinth.index.json", opts)?;
        zip.write_all(serde_json::to_string_pretty(&index)?.as_bytes()).ctx(&out)?;

        // Overrides: pack overrides, profile overrides, patched configs, options.
        let mut overrides: BTreeMap<String, PathBuf> = BTreeMap::new();
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
        for (rel, p) in extra_overrides {
            overrides.insert(rel, p);
        }
        for (rel, p) in &overrides {
            zip.start_file(format!("overrides/{rel}"), opts)?;
            let bytes = std::fs::read(p).ctx(p)?;
            zip.write_all(&bytes).ctx(&out)?;
        }
        zip.finish()?;
        Ok(ExportReport { path: out.display().to_string(), files: index.files.len(), overrides: overrides.len(), warnings })
    }

    /// Import a `.mrpack` into a new pack directory (Modrinth files become pinned modrinth specs).
    pub async fn import_mrpack(&self, file: &Path, dest: &Path, resolve: bool) -> Result<ImportReport> {
        let dest = std::path::absolute(dest).unwrap_or_else(|_| dest.to_path_buf());
        if dest.join(PACK_FILE).exists() {
            return Err(Error::Invalid(format!("{} already contains a pack", dest.display())));
        }
        let f = std::fs::File::open(file).ctx(file)?;
        let mut zip = zip::ZipArchive::new(f)?;
        let index: Index = {
            let mut e = zip.by_name("modrinth.index.json")?;
            let mut s = String::new();
            e.read_to_string(&mut s).ctx(file)?;
            serde_json::from_str(&s)?
        };
        let mc = index
            .dependencies
            .get("minecraft")
            .cloned()
            .ok_or_else(|| Error::Invalid("mrpack has no minecraft dependency".into()))?;
        let (loader, lv) = if let Some(v) = index.dependencies.get("fabric-loader") {
            (LoaderKind::Fabric, v.clone())
        } else if let Some(v) = index.dependencies.get("quilt-loader") {
            (LoaderKind::Quilt, v.clone())
        } else if index.dependencies.keys().any(|k| k == "forge" || k == "neoforge") {
            return Err(Error::Invalid("Forge/NeoForge packs are not supported yet".into()));
        } else {
            (LoaderKind::Vanilla, "latest".into())
        };
        let re = regex::Regex::new(r"cdn\.modrinth\.com/data/([A-Za-z0-9]+)/versions/([A-Za-z0-9]+)/").expect("regex");
        let mut warnings = Vec::new();
        let mut mods = Vec::new();
        for f in &index.files {
            if f.env.as_ref().and_then(|e| e.get("client")).map(String::as_str) == Some("unsupported") {
                continue;
            }
            let kind = ModKind::parse(f.path.split('/').next().unwrap_or("mods")).unwrap_or_default();
            let filename = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
            let url = f.downloads.first().cloned().unwrap_or_default();
            let spec = match re.captures(&url) {
                Some(c) => ModSpec {
                    source: Source::Modrinth,
                    id: Some(c[1].to_string()),
                    version: Some(c[2].to_string()),
                    kind: Some(kind),
                    ..Default::default()
                },
                None => {
                    if url.is_empty() {
                        warnings.push(format!("{} has no download URL; skipped", f.path));
                        continue;
                    }
                    ModSpec {
                        source: Source::Url,
                        url: Some(url),
                        sha1: f.hashes.get("sha1").cloned(),
                        filename: Some(filename),
                        kind: Some(kind),
                        ..Default::default()
                    }
                }
            };
            mods.push(spec);
        }
        util::ensure_dir(&dest)?;
        let mut overrides = 0;
        for i in 0..zip.len() {
            let mut e = zip.by_index(i)?;
            let name = e.name().to_string();
            let rel = name.strip_prefix("overrides/").or_else(|| name.strip_prefix("client-overrides/"));
            let Some(rel) = rel else { continue };
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
        let file = PackFile {
            pack: PackMeta {
                name: index.name.clone(),
                version: Some(index.version_id.clone()),
                description: index.summary.clone(),
                minecraft: mc.clone(),
                loader: LoaderSpec { kind: loader, version: lv },
            },
            mods,
            ..Default::default()
        };
        let mut text = String::from("# Imported from .mrpack by mcpack.\n");
        text.push_str(&toml::to_string_pretty(&file)?);
        std::fs::write(dest.join(PACK_FILE), text).ctx(&dest)?;
        std::fs::write(dest.join(".gitignore"), "run/\n").ctx(&dest)?;
        let n = file.mods.len();
        if resolve {
            if let Err(e) = self.resolve(&dest, Default::default()).await {
                warnings.push(format!("resolve failed: {e}"));
            }
        }
        Ok(ImportReport {
            pack_dir: dest.display().to_string(),
            name: index.name,
            minecraft: mc,
            loader,
            mods: n,
            overrides,
            warnings,
        })
    }
}
