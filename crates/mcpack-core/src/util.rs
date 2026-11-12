use std::io::Read;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::error::{Error, IoContext, Result};

pub fn sha1_bytes(data: &[u8]) -> String {
    hex::encode(Sha1::digest(data))
}

pub fn sha1_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).ctx(path)?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).ctx(path)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha512_file(path: &Path) -> Result<String> {
    use sha2::Sha512;
    let mut f = std::fs::File::open(path).ctx(path)?;
    let mut h = Sha512::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).ctx(path)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Stable short hash of any serializable value (used for change detection fingerprints).
pub fn fingerprint<T: serde::Serialize>(v: &T) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    sha1_bytes(s.as_bytes())[..16].to_string()
}

pub fn ensure_dir(p: &Path) -> Result<()> {
    std::fs::create_dir_all(p).ctx(p)
}

pub fn ensure_parent(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        ensure_dir(parent)?;
    }
    Ok(())
}

/// Write file atomically-ish (temp + rename).
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    ensure_parent(path)?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    std::fs::write(&tmp, data).ctx(&tmp)?;
    replace_file(&tmp, path)
}

pub fn replace_file(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        // Windows rename fails if target exists.
        let _ = std::fs::remove_file(to);
    }
    std::fs::rename(from, to).ctx(to)
}

/// Copy a file (hard link when possible, falls back to copy).
pub fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    ensure_parent(to)?;
    if to.exists() {
        std::fs::remove_file(to).ctx(to)?;
    }
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).ctx(to)?;
    Ok(())
}

/// Maven coordinate `group:artifact:version[:classifier][@ext]` → relative path.
pub fn maven_path(name: &str) -> Result<String> {
    let (coord, ext) = match name.split_once('@') {
        Some((c, e)) => (c, e),
        None => (name, "jar"),
    };
    let parts: Vec<&str> = coord.split(':').collect();
    if parts.len() < 3 {
        return Err(Error::Invalid(format!("bad maven coordinate: {name}")));
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let file = match parts.get(3) {
        Some(classifier) => format!("{artifact}-{version}-{classifier}.{ext}"),
        None => format!("{artifact}-{version}.{ext}"),
    };
    Ok(format!("{group}/{artifact}/{version}/{file}"))
}

/// `group:artifact[:classifier]` – identity of a library ignoring version.
pub fn maven_key(name: &str) -> String {
    let coord = name.split('@').next().unwrap_or(name);
    let parts: Vec<&str> = coord.split(':').collect();
    match parts.len() {
        0..=2 => coord.to_string(),
        3 => format!("{}:{}", parts[0], parts[1]),
        _ => format!("{}:{}:{}", parts[0], parts[1], parts[3]),
    }
}

/// Offline-mode UUID (same algorithm as vanilla: UUID v3 of "OfflinePlayer:<name>").
pub fn offline_uuid(name: &str) -> String {
    use md5::{Digest as _, Md5};
    let mut b: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
    b[6] = (b[6] & 0x0f) | 0x30;
    b[8] = (b[8] & 0x3f) | 0x80;
    hex::encode(b)
}

pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sanitize to a safe single path component.
pub fn safe_component(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+') { c } else { '_' })
        .collect();
    let out = out.trim_matches('.').to_string();
    if out.is_empty() {
        "_".into()
    } else {
        out
    }
}

/// Resolve `p` relative to `base` unless absolute.
pub fn resolve_rel(base: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        pb
    } else {
        base.join(pb)
    }
}

/// Walk all files under `root`, returning paths relative to root with `/` separators.
pub fn walk_files(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ctx(&dir)? {
            let entry = entry.ctx(&dir)?;
            let p = entry.path();
            let ft = entry.file_type().ctx(&p)?;
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                if let Ok(rel) = p.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_paths() {
        assert_eq!(
            maven_path("net.fabricmc:fabric-loader:0.16.14").unwrap(),
            "net/fabricmc/fabric-loader/0.16.14/fabric-loader-0.16.14.jar"
        );
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.3:natives-windows").unwrap(),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar"
        );
        assert_eq!(maven_key("org.lwjgl:lwjgl:3.3.3:natives-windows"), "org.lwjgl:lwjgl:natives-windows");
        assert_eq!(maven_key("org.ow2.asm:asm:9.6"), "org.ow2.asm:asm");
    }

    #[test]
    fn offline_uuid_matches_vanilla() {
        // Known value for "Notch" offline: b50ad385-829d-3141-a216-7e7d7539ba7f
        assert_eq!(offline_uuid("Notch"), "b50ad385829d3141a2167e7d7539ba7f");
    }
}
