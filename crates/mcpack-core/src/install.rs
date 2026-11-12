//! Install a Minecraft version (+ loader): version JSON, client jar, libraries, assets, natives, Java.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, IoContext, Result};
use crate::http::{DownloadItem, DownloadStats};
use crate::meta::java;
use crate::meta::loader::{self, LoaderKind};
use crate::meta::mojang::{self, AssetIndex, VersionJson, VersionManifest};
use crate::util;
use crate::Ctx;

/// Everything needed to build a launch command.
#[derive(Debug, Clone)]
pub struct GameInstall {
    pub version: VersionJson,
    pub client_jar: PathBuf,
    pub libraries_dir: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub natives_dir: PathBuf,
    pub assets_root: PathBuf,
    pub assets_index: String,
    pub java: PathBuf,
    pub java_major: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallSummary {
    pub version_id: String,
    pub minecraft: String,
    pub loader: LoaderKind,
    pub loader_version: String,
    pub java: String,
    pub java_major: Option<u32>,
    pub libraries: usize,
    pub downloads: DownloadStats,
}

async fn version_manifest(ctx: &Ctx, refresh: bool) -> Result<VersionManifest> {
    let path = ctx.paths.meta().join("version_manifest_v2.json");
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < 6 * 3600);
    if path.exists() && fresh && !refresh {
        let text = std::fs::read_to_string(&path).ctx(&path)?;
        if let Ok(m) = serde_json::from_str(&text) {
            return Ok(m);
        }
    }
    match ctx.http.bytes(mojang::VERSION_MANIFEST_URL).await {
        Ok(bytes) => {
            util::write_atomic(&path, &bytes)?;
            Ok(serde_json::from_slice(&bytes)?)
        }
        Err(e) if path.exists() => {
            tracing::warn!("using cached version manifest: {e}");
            let text = std::fs::read_to_string(&path).ctx(&path)?;
            Ok(serde_json::from_str(&text)?)
        }
        Err(e) => Err(e),
    }
}

/// List Minecraft versions (`release` only unless `include_snapshots`).
pub async fn list_minecraft_versions(ctx: &Ctx, include_snapshots: bool) -> Result<Vec<String>> {
    let m = version_manifest(ctx, false).await?;
    Ok(m.versions
        .into_iter()
        .filter(|v| include_snapshots || v.kind == "release")
        .map(|v| v.id)
        .collect())
}

async fn vanilla_json(ctx: &Ctx, mc: &str) -> Result<VersionJson> {
    let path = ctx.paths.versions().join(mc).join(format!("{mc}.json"));
    if path.exists() {
        let text = std::fs::read_to_string(&path).ctx(&path)?;
        if let Ok(v) = serde_json::from_str(&text) {
            return Ok(v);
        }
    }
    let mut manifest = version_manifest(ctx, false).await?;
    if !manifest.versions.iter().any(|v| v.id == mc) {
        manifest = version_manifest(ctx, true).await?;
    }
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == mc)
        .ok_or_else(|| Error::NotFound(format!("Minecraft version {mc}")))?;
    ctx.http
        .download(&DownloadItem { url: entry.url.clone(), dest: path.clone(), sha1: entry.sha1.clone(), size: None })
        .await?;
    let text = std::fs::read_to_string(&path).ctx(&path)?;
    Ok(serde_json::from_str(&text)?)
}

async fn loader_json(ctx: &Ctx, kind: LoaderKind, mc: &str, loader_version: &str) -> Result<VersionJson> {
    let id = format!("{}-{}-{}", kind.as_str(), loader_version, mc);
    let path = ctx.paths.versions().join(&id).join(format!("{id}.json"));
    if path.exists() {
        let text = std::fs::read_to_string(&path).ctx(&path)?;
        if let Ok(v) = serde_json::from_str(&text) {
            return Ok(v);
        }
    }
    let v = loader::profile_json(&ctx.http, kind, mc, loader_version).await?;
    util::write_atomic(&path, serde_json::to_string_pretty(&v)?.as_bytes())?;
    Ok(v)
}

/// Resolve the merged version JSON for (mc, loader).
pub async fn version_json(ctx: &Ctx, mc: &str, kind: LoaderKind, loader_version: &str) -> Result<VersionJson> {
    let vanilla = vanilla_json(ctx, mc).await?;
    if kind == LoaderKind::Vanilla {
        return Ok(vanilla);
    }
    let child = loader_json(ctx, kind, mc, loader_version).await?;
    Ok(mojang::merge_versions(&vanilla, &child))
}

/// Download everything and return launch inputs.
pub async fn install(
    ctx: &Ctx,
    mc: &str,
    kind: LoaderKind,
    loader_version: &str,
    java_override: Option<&str>,
    progress: &(dyn Fn(&str, Option<f64>, String) + Sync),
) -> Result<(GameInstall, InstallSummary)> {
    progress("download", Some(0.0), format!("Minecraft {mc} metadata"));
    let version = version_json(ctx, mc, kind, loader_version).await?;
    let p = &ctx.platform;
    let mut items = Vec::new();

    // Client jar.
    let client = version
        .downloads
        .as_ref()
        .map(|d| d.client.clone())
        .ok_or_else(|| Error::Launch(format!("version {mc} has no client download")))?;
    let client_jar = ctx.paths.versions().join(mc).join(format!("{mc}.jar"));
    items.push(DownloadItem { url: client.url.clone(), dest: client_jar.clone(), sha1: client.sha1.clone(), size: client.size });

    // Libraries.
    let mut classpath = Vec::new();
    let mut natives_jars = Vec::new();
    for lib in &version.libraries {
        for f in lib.files(p)? {
            let dest = ctx.paths.libraries().join(&f.path);
            items.push(DownloadItem { url: f.url.clone(), dest: dest.clone(), sha1: f.sha1.clone(), size: f.size });
            if f.extract_natives {
                natives_jars.push(dest);
            } else {
                if f.name.contains(":natives-") {
                    natives_jars.push(dest.clone());
                }
                if !classpath.contains(&dest) {
                    classpath.push(dest);
                }
            }
        }
    }
    classpath.push(client_jar.clone());

    // Asset index.
    let idx_ref = version
        .asset_index
        .clone()
        .ok_or_else(|| Error::Launch(format!("version {mc} has no asset index")))?;
    let idx_path = ctx.paths.assets().join("indexes").join(format!("{}.json", idx_ref.id));
    ctx.http
        .download(&DownloadItem { url: idx_ref.url.clone(), dest: idx_path.clone(), sha1: idx_ref.sha1.clone(), size: idx_ref.size })
        .await?;
    let idx_text = std::fs::read_to_string(&idx_path).ctx(&idx_path)?;
    let index: AssetIndex = serde_json::from_str(&idx_text)?;
    for obj in index.objects.values() {
        let prefix = &obj.hash[..2];
        items.push(DownloadItem {
            url: format!("{}/{prefix}/{}", mojang::RESOURCES_URL, obj.hash),
            dest: ctx.paths.assets().join("objects").join(prefix).join(&obj.hash),
            sha1: Some(obj.hash.clone()),
            size: Some(obj.size),
        });
    }

    tracing::info!(files = items.len(), "downloading game files");
    let step = (items.len() / 100).max(1);
    let stats = ctx
        .http
        .download_all_with(items, &|done, total| {
            if done % step == 0 || done == total {
                progress("download", Some(done as f64 / total.max(1) as f64), format!("game files {done}/{total}"));
            }
        })
        .await?;

    // Legacy virtual assets.
    if index.is_virtual || index.map_to_resources {
        let vroot = ctx.paths.assets().join("virtual").join(&idx_ref.id);
        for (name, obj) in &index.objects {
            let dest = vroot.join(name);
            if !dest.exists() {
                let src = ctx.paths.assets().join("objects").join(&obj.hash[..2]).join(&obj.hash);
                util::link_or_copy(&src, &dest)?;
            }
        }
    }

    // Natives.
    let natives_dir = ctx.paths.natives().join(&version.id);
    extract_natives(&natives_jars, &natives_dir)?;

    // Java.
    progress("java", None, "preparing Java runtime".into());
    let (java, java_major) = select_java(ctx, &version, java_override).await?;
    progress("java", Some(1.0), format!("Java {}", java_major.map(|m| m.to_string()).unwrap_or_else(|| "?".into())));

    let summary = InstallSummary {
        version_id: version.id.clone(),
        minecraft: mc.to_string(),
        loader: kind,
        loader_version: loader_version.to_string(),
        java: java.display().to_string(),
        java_major,
        libraries: classpath.len(),
        downloads: stats,
    };
    Ok((
        GameInstall {
            version,
            client_jar,
            libraries_dir: ctx.paths.libraries(),
            classpath,
            natives_dir,
            assets_root: ctx.paths.assets(),
            assets_index: idx_ref.id,
            java,
            java_major,
        },
        summary,
    ))
}

async fn select_java(ctx: &Ctx, version: &VersionJson, java_override: Option<&str>) -> Result<(PathBuf, Option<u32>)> {
    let required = version.java_version.as_ref().map(|j| j.major_version);
    if let Some(j) = java_override.map(str::to_string).or_else(|| ctx.cfg().java_path) {
        let path = PathBuf::from(&j);
        let major = java::detect_major(&path).await;
        if let (Some(r), Some(m)) = (required, major) {
            if m < r {
                return Err(Error::Launch(format!("configured java {j} is Java {m}, but this version needs Java {r}")));
            }
        }
        return Ok((path, major));
    }
    let component = version
        .java_version
        .as_ref()
        .map(|j| j.component.clone())
        .unwrap_or_else(|| "jre-legacy".to_string());
    match java::ensure_runtime(&ctx.http, &ctx.paths.java(), &component, &ctx.platform).await {
        Ok(exe) => Ok((exe, required)),
        Err(e) => {
            tracing::warn!("Mojang runtime {component} unavailable ({e}); falling back to system java");
            let sys = system_java().ok_or_else(|| Error::Launch(format!("no Java runtime available: {e}")))?;
            let major = java::detect_major(&sys).await;
            Ok((sys, major))
        }
    }
}

fn system_java() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "java.exe" } else { "java" };
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let p = PathBuf::from(home).join("bin").join(exe);
        if p.exists() {
            return Some(p);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|d| d.join(exe)).find(|p| p.exists())
    })
}

fn extract_natives(jars: &[PathBuf], dir: &Path) -> Result<()> {
    let marker = dir.join(".mcpack-extracted");
    let sig = util::fingerprint(&jars.iter().map(|p| p.display().to_string()).collect::<Vec<_>>());
    if marker.exists() && std::fs::read_to_string(&marker).ok().as_deref() == Some(sig.as_str()) {
        return Ok(());
    }
    util::ensure_dir(dir)?;
    for jar in jars {
        let f = std::fs::File::open(jar).ctx(jar)?;
        let mut zip = zip::ZipArchive::new(f)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name().to_string();
            if entry.is_dir() || name.starts_with("META-INF") {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if !(lower.ends_with(".dll") || lower.ends_with(".so") || lower.ends_with(".dylib") || lower.ends_with(".jnilib")) {
                continue;
            }
            let file_name = Path::new(&name).file_name().map(|n| n.to_os_string()).unwrap_or_default();
            let dest = dir.join(file_name);
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).ctx(jar)?;
            if std::fs::write(&dest, &buf).is_err() {
                // In use by a running game: fine, it's the same file.
                continue;
            }
        }
    }
    std::fs::write(&marker, sig).ctx(&marker)?;
    Ok(())
}
