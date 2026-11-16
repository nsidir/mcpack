//! Forge / NeoForge installation from their official installer jars.
//!
//! Two installer generations exist:
//! - legacy (Forge 1.6 – 1.12.2 builds before 14.23.5.2851): `install_profile.json` holds the
//!   whole launcher profile in `versionInfo`, and the Forge jar ships inside the installer;
//! - modern (everything newer, including all NeoForge): `install_profile.json` names a
//!   `version.json` inside the installer plus *processors* (small Java tools) that build the
//!   patched client jar from the vanilla one. We run them the same way the installer does.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, IoContext, Result};
use crate::http::DownloadItem;
use crate::meta::loader::{self, LoaderKind};
use crate::meta::mojang::{Library, VersionJson};
use crate::util;
use crate::Ctx;

/// Where an installed Forge-like version keeps its installer facts.
pub const SIDECAR: &str = "mcpack-installer.json";

/// Facts about an installed Forge/NeoForge version that later steps need.
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
pub struct InstallerInfo {
    pub installer: PathBuf,
    pub installer_sha1: String,
    /// Modern installers: processors still have to run (`install_profile.json` kept beside).
    pub modern: bool,
    /// obf → runtime-name mappings produced by the installer (TSRG): `MERGED_MAPPINGS`
    /// (Mojang classes + SRG members, 1.17+) or else `MAPPINGS` (MCP classes + SRG members).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_mappings: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct Processor {
    jar: String,
    #[serde(default)]
    classpath: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    outputs: BTreeMap<String, String>,
    #[serde(default)]
    sides: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
struct SidedValue {
    #[serde(default)]
    client: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModernProfile {
    json: String,
    #[serde(default)]
    data: BTreeMap<String, SidedValue>,
    #[serde(default)]
    processors: Vec<Processor>,
    #[serde(default)]
    libraries: Vec<Library>,
}

pub fn version_id(kind: LoaderKind, mc: &str, loader_version: &str) -> String {
    format!("{}-{}-{}", kind.as_str(), loader_version, mc)
}

fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<Vec<u8>> {
    let mut f = zip
        .by_name(name.trim_start_matches('/'))
        .map_err(|_| Error::Launch(format!("installer has no {name}")))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| Error::io(name, e))?;
    Ok(buf)
}

/// Old Forge metadata points at http://files.minecraftforge.net/maven, which is gone.
fn fix_repo_url(url: &str) -> String {
    let u = url.trim();
    for old in ["http://files.minecraftforge.net/maven", "https://files.minecraftforge.net/maven", "http://maven.minecraftforge.net"] {
        if let Some(rest) = u.strip_prefix(old) {
            return format!("https://maven.minecraftforge.net{rest}");
        }
    }
    u.replace("http://repo1.maven.org", "https://repo1.maven.org").replace("http://repo.maven.apache.org", "https://repo.maven.apache.org")
}

fn normalize_libraries(libs: &mut Vec<Library>) {
    for l in libs.iter_mut() {
        if let Some(u) = &l.url {
            l.url = Some(fix_repo_url(u));
        }
        if let Some(a) = l.downloads.as_mut().and_then(|d| d.artifact.as_mut()) {
            a.url = fix_repo_url(&a.url);
        }
    }
}

/// Download the installer and produce the child launcher profile (inherits vanilla).
/// Caches `versions/<id>/<id>.json` plus [`SIDECAR`].
pub async fn loader_json(ctx: &Ctx, kind: LoaderKind, mc: &str, loader_version: &str) -> Result<VersionJson> {
    // FML before Minecraft 1.6 ignores the game directory and works in %APPDATA%\.minecraft:
    // it would read and write the player's own Minecraft folder instead of the instance.
    if kind == LoaderKind::Forge && crate::agent::mappings::mc_version_parts(mc).is_some_and(|v| v < vec![1, 6]) {
        return Err(Error::Invalid(format!(
            "Forge for Minecraft {mc} is not supported: Forge before Minecraft 1.6 cannot run from an instance folder (it always uses %APPDATA%/.minecraft). Forge works from 1.6 on; vanilla {mc} works"
        )));
    }
    let id = version_id(kind, mc, loader_version);
    let vdir = ctx.paths.versions().join(&id);
    let json_path = vdir.join(format!("{id}.json"));
    if json_path.exists() && vdir.join(SIDECAR).exists() {
        let text = std::fs::read_to_string(&json_path).ctx(&json_path)?;
        if let Ok(v) = serde_json::from_str(&text) {
            return Ok(v);
        }
    }
    let (coord, repo) = loader::forge_artifact(kind, mc, loader_version)?;
    let installer_rel = util::maven_path(&format!("{coord}:installer"))?;
    let installer = ctx.paths.libraries().join(&installer_rel);
    if let Err(e) = ctx
        .http
        .download(&DownloadItem { url: format!("{repo}{installer_rel}"), dest: installer.clone(), sha1: None, size: None })
        .await
    {
        return Err(match e {
            Error::HttpStatus { status, url, .. } => Error::Launch(format!(
                "could not download the {} installer ({url} answered HTTP {status}); the {} maven may be having problems, retry later",
                kind.display(),
                kind.display()
            )),
            e => e,
        });
    }
    let file = std::fs::File::open(&installer).ctx(&installer)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| Error::Launch(format!("bad installer {}: {e}", installer.display())))?;
    let profile: Value = serde_json::from_slice(&read_entry(&mut zip, "install_profile.json")?)?;

    let libraries = ctx.paths.libraries();
    let mut info = InstallerInfo { installer: installer.clone(), installer_sha1: util::sha1_file(&installer)?, ..Default::default() };
    let mut child: VersionJson;
    if let Some(vi) = profile.get("versionInfo") {
        // Legacy: the profile itself, plus the universal jar inside the installer.
        child = serde_json::from_value(vi.clone())?;
        let install = profile.get("install").cloned().unwrap_or(Value::Null);
        if let (Some(path), Some(file_path)) =
            (install.get("path").and_then(|v| v.as_str()), install.get("filePath").and_then(|v| v.as_str()))
        {
            let dest = libraries.join(util::maven_path(path)?);
            if !dest.exists() {
                util::write_atomic(&dest, &read_entry(&mut zip, file_path)?)?;
            }
        }
    } else {
        let modern: ModernProfile = serde_json::from_value(profile.clone())?;
        child = serde_json::from_slice(&read_entry(&mut zip, &modern.json)?)?;
        // Jars bundled under maven/ (Forge itself for some versions, local-only tools).
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name().to_string();
            if entry.is_dir() {
                continue;
            }
            if let Some(rel) = name.strip_prefix("maven/") {
                let dest = libraries.join(rel);
                if !dest.exists() {
                    let mut buf = Vec::new();
                    entry.read_to_end(&mut buf).map_err(|e| Error::io(&name, e))?;
                    util::write_atomic(&dest, &buf)?;
                }
            }
        }
        info.modern = !modern.processors.is_empty();
        let data_path = |key: &str| -> Option<PathBuf> {
            let v = modern.data.get(key)?.client.as_deref()?;
            let a = v.strip_prefix('[')?.strip_suffix(']')?;
            Some(libraries.join(util::maven_path(a).ok()?))
        };
        info.runtime_mappings = data_path("MERGED_MAPPINGS").or_else(|| data_path("MAPPINGS"));
        util::write_atomic(&vdir.join("install_profile.json"), serde_json::to_string_pretty(&profile)?.as_bytes())?;
    }
    normalize_libraries(&mut child.libraries);
    child.inherits_from = Some(child.inherits_from.clone().unwrap_or_else(|| mc.to_string()));
    util::write_atomic(&json_path, serde_json::to_string_pretty(&child)?.as_bytes())?;
    util::write_atomic(&vdir.join(SIDECAR), serde_json::to_string_pretty(&info)?.as_bytes())?;
    Ok(child)
}

pub fn installer_info(ctx: &Ctx, kind: LoaderKind, mc: &str, loader_version: &str) -> Option<InstallerInfo> {
    let p = ctx.paths.versions().join(version_id(kind, mc, loader_version)).join(SIDECAR);
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn marker_path(ctx: &Ctx, kind: LoaderKind, mc: &str, lv: &str) -> PathBuf {
    ctx.paths.versions().join(version_id(kind, mc, lv)).join(".mcpack-processed")
}

fn load_modern(ctx: &Ctx, kind: LoaderKind, mc: &str, lv: &str) -> Result<Option<ModernProfile>> {
    let p = ctx.paths.versions().join(version_id(kind, mc, lv)).join("install_profile.json");
    if !p.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p).ctx(&p)?;
    Ok(Some(serde_json::from_str(&text)?))
}

/// Libraries the processors need (downloaded with the game files). Empty once processed.
pub fn processor_libraries(ctx: &Ctx, kind: LoaderKind, mc: &str, lv: &str) -> Result<Vec<Library>> {
    let Some(info) = installer_info(ctx, kind, mc, lv) else { return Ok(vec![]) };
    if !info.modern || processed(ctx, kind, mc, lv, &info) {
        return Ok(vec![]);
    }
    let mut libs = load_modern(ctx, kind, mc, lv)?.map(|m| m.libraries).unwrap_or_default();
    normalize_libraries(&mut libs);
    Ok(libs)
}

fn processed(ctx: &Ctx, kind: LoaderKind, mc: &str, lv: &str, info: &InstallerInfo) -> bool {
    std::fs::read_to_string(marker_path(ctx, kind, mc, lv)).ok().as_deref() == Some(info.installer_sha1.as_str())
}

fn jar_main_class(jar: &Path) -> Result<String> {
    let f = std::fs::File::open(jar).ctx(jar)?;
    let mut zip = zip::ZipArchive::new(f)?;
    let mut manifest = String::new();
    zip.by_name("META-INF/MANIFEST.MF")
        .map_err(|_| Error::Launch(format!("{} has no manifest", jar.display())))?
        .read_to_string(&mut manifest)
        .ctx(jar)?;
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("Main-Class:").map(|s| s.trim().to_string()))
        .ok_or_else(|| Error::Launch(format!("{} has no Main-Class", jar.display())))
}

/// Replace `{KEY}` tokens (Forge installer `Util.replaceTokens`, with `\` escapes).
fn replace_tokens(data: &BTreeMap<String, String>, s: &str) -> Result<String> {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '{' => {
                let mut key = String::new();
                for k in chars.by_ref() {
                    if k == '}' {
                        break;
                    }
                    key.push(k);
                }
                let v = data.get(&key).ok_or_else(|| Error::Launch(format!("installer processor uses unknown data key {{{key}}}")))?;
                out.push_str(v);
            }
            _ => out.push(c),
        }
    }
    Ok(out)
}

fn artifact_path(libraries: &Path, s: &str) -> Result<Option<PathBuf>> {
    match s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        Some(a) => Ok(Some(libraries.join(util::maven_path(a)?))),
        None => Ok(None),
    }
}

/// Run the installer processors (modern installers) once per installer build.
pub async fn run_processors(
    ctx: &Ctx,
    kind: LoaderKind,
    mc: &str,
    lv: &str,
    java: &Path,
    client_jar: &Path,
    progress: &(dyn Fn(&str, Option<f64>, String) + Sync),
) -> Result<()> {
    let Some(info) = installer_info(ctx, kind, mc, lv) else { return Ok(()) };
    if !info.modern || processed(ctx, kind, mc, lv, &info) {
        return Ok(());
    }
    let Some(profile) = load_modern(ctx, kind, mc, lv)? else { return Ok(()) };
    let libraries = ctx.paths.libraries();
    let vdir = ctx.paths.versions().join(version_id(kind, mc, lv));
    let work = vdir.join("installer-data");
    util::ensure_dir(&work)?;

    // Data values: [artifact] → library path, 'literal' → literal, /file → extracted from installer.
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    {
        let f = std::fs::File::open(&info.installer).ctx(&info.installer)?;
        let mut zip = zip::ZipArchive::new(f)?;
        for (k, v) in &profile.data {
            let Some(v) = v.client.as_deref() else { continue };
            let resolved = if let Some(p) = artifact_path(&libraries, v)? {
                p.display().to_string()
            } else if let Some(lit) = v.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
                lit.to_string()
            } else if v.starts_with('/') {
                let dest = work.join(v.trim_start_matches('/'));
                util::write_atomic(&dest, &read_entry(&mut zip, v)?)?;
                dest.display().to_string()
            } else {
                v.to_string()
            };
            data.insert(k.clone(), resolved);
        }
    }
    let root = ctx.paths.versions().parent().map(Path::to_path_buf).unwrap_or_else(|| libraries.clone());
    data.insert("SIDE".into(), "client".into());
    data.insert("MINECRAFT_JAR".into(), client_jar.display().to_string());
    data.insert("MINECRAFT_VERSION".into(), mc.to_string());
    data.insert("ROOT".into(), root.display().to_string());
    data.insert("INSTALLER".into(), info.installer.display().to_string());
    data.insert("LIBRARY_DIR".into(), libraries.display().to_string());

    let procs: Vec<&Processor> = profile
        .processors
        .iter()
        .filter(|p| p.sides.as_ref().is_none_or(|s| s.iter().any(|x| x == "client")))
        .collect();
    let sep = ctx.platform.classpath_separator();
    for (i, p) in procs.iter().enumerate() {
        progress("install", Some(i as f64 / procs.len().max(1) as f64), format!("{} installer step {}/{}", kind.display(), i + 1, procs.len()));
        // Skip processors whose outputs already exist with the right hashes.
        if !p.outputs.is_empty() {
            let mut all_ok = true;
            for (k, v) in &p.outputs {
                let path = replace_tokens(&data, k)?;
                let path = artifact_path(&libraries, &path)?.map(|p| p.display().to_string()).unwrap_or(path);
                let sha = replace_tokens(&data, v)?;
                let sha = sha.trim_matches('\'');
                if !crate::http::file_is_valid(Path::new(&path), Some(sha), None)? {
                    all_ok = false;
                    break;
                }
            }
            if all_ok {
                continue;
            }
        }
        let jar = libraries.join(util::maven_path(&p.jar)?);
        let main = jar_main_class(&jar)?;
        let mut cp = vec![jar.display().to_string()];
        for c in &p.classpath {
            cp.push(libraries.join(util::maven_path(c)?).display().to_string());
        }
        let mut args = Vec::new();
        for a in &p.args {
            match artifact_path(&libraries, a)? {
                Some(path) => args.push(path.display().to_string()),
                None => args.push(replace_tokens(&data, a)?),
            }
        }
        tracing::info!(processor = %p.jar, ?args, "running installer processor");
        let mut cmd = tokio::process::Command::new(java);
        cmd.arg("-cp").arg(cp.join(sep)).arg(&main).args(&args).current_dir(&work);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd
            .output()
            .await
            .map_err(|e| Error::Launch(format!("failed to run installer step {} with {}: {e}", p.jar, java.display())))?;
        if !out.status.success() {
            let tail = |b: &[u8]| {
                let s = String::from_utf8_lossy(b);
                let lines: Vec<&str> = s.lines().collect();
                lines[lines.len().saturating_sub(15)..].join("\n")
            };
            return Err(Error::Launch(format!(
                "{} installer step {} failed ({}):\n{}\n{}",
                kind.display(),
                p.jar,
                out.status,
                tail(&out.stdout),
                tail(&out.stderr)
            )));
        }
        for (k, v) in &p.outputs {
            let path = replace_tokens(&data, k)?;
            let path = artifact_path(&libraries, &path)?.map(|p| p.display().to_string()).unwrap_or(path);
            let sha = replace_tokens(&data, v)?;
            let sha = sha.trim_matches('\'');
            if !Path::new(&path).exists() {
                return Err(Error::Launch(format!("{} installer step {} did not produce {path}", kind.display(), p.jar)));
            }
            // Old installers recorded hashes of jars Mojang has since re-released (1.13.2), so a
            // mismatch is not fatal (other launchers do not check this either).
            if !crate::http::file_is_valid(Path::new(&path), Some(sha), None)? {
                tracing::warn!("{} installer step {} produced {path} with an unexpected hash", kind.display(), p.jar);
            }
        }
    }
    progress("install", Some(1.0), format!("{} installed", kind.display()));
    let _ = std::fs::remove_dir_all(&work);
    util::write_atomic(&marker_path(ctx, kind, mc, lv), info.installer_sha1.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let data = BTreeMap::from([("A".to_string(), "x".to_string()), ("B".to_string(), "y".to_string())]);
        assert_eq!(replace_tokens(&data, "{A}/{B}.jar").unwrap(), "x/y.jar");
        assert_eq!(replace_tokens(&data, "\\{A}").unwrap(), "{A}");
        assert!(replace_tokens(&data, "{C}").is_err());
    }

    #[test]
    fn repo_urls() {
        assert_eq!(fix_repo_url("http://files.minecraftforge.net/maven/"), "https://maven.minecraftforge.net/");
        assert_eq!(fix_repo_url("https://libraries.minecraft.net/"), "https://libraries.minecraft.net/");
    }
}
