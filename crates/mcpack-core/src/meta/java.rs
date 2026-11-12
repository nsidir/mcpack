//! Mojang-provided Java runtimes (same ones the official launcher uses).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::Platform;
use crate::error::{Error, IoContext, Result};
use crate::http::{DownloadItem, Http};
use crate::util;

pub const JAVA_ALL_URL: &str =
    "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

#[derive(Debug, Deserialize)]
struct RuntimeEntry {
    manifest: RuntimeManifestRef,
    version: RuntimeVersion,
}

#[derive(Debug, Deserialize)]
struct RuntimeManifestRef {
    url: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeVersion {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeManifest {
    files: BTreeMap<String, RuntimeFile>,
}

#[derive(Debug, Deserialize)]
struct RuntimeFile {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    executable: bool,
    #[serde(default)]
    downloads: Option<RuntimeDownloads>,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RuntimeDownloads {
    raw: RawDownload,
}

#[derive(Debug, Deserialize)]
struct RawDownload {
    sha1: String,
    size: u64,
    url: String,
}

/// Java executable path inside an installed runtime directory.
pub fn java_exe(runtime_dir: &Path, p: &Platform) -> PathBuf {
    match p.os {
        "windows" => runtime_dir.join("bin").join("java.exe"),
        "osx" => runtime_dir.join("jre.bundle/Contents/Home/bin/java"),
        _ => runtime_dir.join("bin").join("java"),
    }
}

/// Ensure Mojang runtime `component` (e.g. `java-runtime-delta`) is installed; returns java executable.
pub async fn ensure_runtime(http: &Http, java_root: &Path, component: &str, p: &Platform) -> Result<PathBuf> {
    let dir = java_root.join(component);
    let marker = dir.join(".mcpack-complete");
    let exe = java_exe(&dir, p);
    if marker.exists() && exe.exists() {
        return Ok(exe);
    }
    let all: BTreeMap<String, BTreeMap<String, Vec<RuntimeEntry>>> = http.json(JAVA_ALL_URL).await?;
    let key = p.java_runtime_key();
    let entry = all
        .get(key)
        .and_then(|m| m.get(component))
        .and_then(|v| v.first())
        .ok_or_else(|| Error::NotFound(format!("Mojang java runtime {component} for {key}")))?;
    let manifest: RuntimeManifest = http.json(&entry.manifest.url).await?;
    let mut items = Vec::new();
    let mut executables = Vec::new();
    let mut links = Vec::new();
    for (path, f) in &manifest.files {
        let dest = dir.join(path);
        match f.kind.as_str() {
            "directory" => util::ensure_dir(&dest)?,
            "file" => {
                if let Some(d) = &f.downloads {
                    items.push(DownloadItem {
                        url: d.raw.url.clone(),
                        dest: dest.clone(),
                        sha1: Some(d.raw.sha1.clone()),
                        size: Some(d.raw.size),
                    });
                    if f.executable {
                        executables.push(dest);
                    }
                }
            }
            "link" => {
                if let Some(t) = &f.target {
                    links.push((dest, t.clone()));
                }
            }
            _ => {}
        }
    }
    tracing::info!(component, files = items.len(), "installing java runtime");
    http.download_all(items).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for e in &executables {
            let _ = std::fs::set_permissions(e, std::fs::Permissions::from_mode(0o755));
        }
        for (dest, target) in &links {
            let _ = std::fs::remove_file(dest);
            let _ = std::os::unix::fs::symlink(target, dest);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (&executables, &links);
    }
    std::fs::write(&marker, &entry.version.name).ctx(&marker)?;
    Ok(exe)
}

/// Best-effort detection of a Java executable's major version (`java -version`).
pub async fn detect_major(java: &Path) -> Option<u32> {
    let out = tokio::process::Command::new(java).arg("-version").output().await.ok()?;
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    let re = regex::Regex::new(r#"version "(\d+)(?:\.(\d+))?"#).ok()?;
    let caps = re.captures(&text)?;
    let first: u32 = caps.get(1)?.as_str().parse().ok()?;
    if first == 1 {
        caps.get(2)?.as_str().parse().ok()
    } else {
        Some(first)
    }
}
