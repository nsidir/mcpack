//! Pre-launch checks: lock freshness, manual downloads, and jar-level metadata dependency
//! validation: fabric.mod.json / quilt.mod.json (Fabric, Quilt), META-INF/mods.toml (Forge,
//! NeoForge 1.20.1 – 1.20.4), META-INF/neoforge.mods.toml (NeoForge 1.20.5+), mcmod.info (old Forge).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::Path;

use serde::Serialize;

use crate::diagnose::Problem;
use crate::error::{IoContext, Result};
use crate::meta::loader::LoaderKind;

#[derive(Debug, Clone, Serialize, Default)]
pub struct ModMeta {
    pub id: String,
    pub version: String,
    pub name: Option<String>,
    pub file: String,
    pub provides: Vec<String>,
    pub depends: BTreeMap<String, Vec<String>>,
    pub breaks: BTreeMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nested: Vec<ModMeta>,
    /// Which metadata it was read from: fabric | quilt | forge | neoforge | legacyforge.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub platform: String,
    /// Mod platforms the jar has metadata for (multi-loader jars list several).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CheckReport {
    pub ok: bool,
    pub problems: Vec<Problem>,
    pub mods_scanned: usize,
    pub mods: Vec<ModSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModSummary {
    pub id: String,
    pub version: String,
    pub file: String,
}

/// Ids provided by the platform rather than by mods.
const BUILTIN: &[&str] = &[
    "minecraft", "java", "fabricloader", "fabric-loader", "quilt_loader", "quilt_base", "forge", "neoforge", "fml", "javafml",
    "lowcodefml", "mcp", "Forge", "FML",
];

fn values(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        _ => vec!["*".to_string()],
    }
}

fn parse_fabric(json: &serde_json::Value, file: &str) -> Option<ModMeta> {
    let id = json.get("id")?.as_str()?.to_string();
    let mut m = ModMeta {
        id,
        version: json.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        name: json.get("name").and_then(|v| v.as_str()).map(str::to_string),
        file: file.to_string(),
        platform: "fabric".into(),
        ..Default::default()
    };
    if let Some(p) = json.get("provides").and_then(|p| p.as_array()) {
        m.provides = p.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
    }
    for (field, target) in [("depends", &mut m.depends), ("breaks", &mut m.breaks)] {
        if let Some(obj) = json.get(field).and_then(|d| d.as_object()) {
            for (k, v) in obj {
                target.insert(k.clone(), values(v));
            }
        }
    }
    Some(m)
}

fn parse_quilt(json: &serde_json::Value, file: &str) -> Option<ModMeta> {
    let ql = json.get("quilt_loader")?;
    let mut m = ModMeta {
        id: ql.get("id")?.as_str()?.to_string(),
        version: ql.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        name: ql.get("metadata").and_then(|md| md.get("name")).and_then(|v| v.as_str()).map(str::to_string),
        file: file.to_string(),
        platform: "quilt".into(),
        ..Default::default()
    };
    if let Some(p) = ql.get("provides").and_then(|p| p.as_array()) {
        m.provides = p
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string).or_else(|| x.get("id").and_then(|i| i.as_str()).map(str::to_string)))
            .collect();
    }
    for (field, target) in [("depends", &mut m.depends), ("breaks", &mut m.breaks)] {
        if let Some(arr) = ql.get(field).and_then(|d| d.as_array()) {
            for d in arr {
                if let Some(s) = d.as_str() {
                    target.insert(s.to_string(), vec!["*".into()]);
                } else if let Some(id) = d.get("id").and_then(|i| i.as_str()) {
                    if d.get("optional").and_then(|o| o.as_bool()) == Some(true) {
                        continue;
                    }
                    let vers = d.get("versions").map(values).unwrap_or_else(|| vec!["*".into()]);
                    target.insert(id.to_string(), vers);
                }
            }
        }
    }
    Some(m)
}

/// Maven version range (Forge `versionRange`) → predicates [`version_matches`] understands.
/// `[1.2,2)` → `>=1.2 <2`; `[1.0]` → `=1.0`; ranges joined by commas are OR-ed; a bare
/// version is a soft requirement (anything).
pub fn maven_range(range: &str) -> Vec<String> {
    let r = range.trim();
    if r.is_empty() || r == "*" || !(r.starts_with('[') || r.starts_with('(')) {
        return vec!["*".into()];
    }
    let mut out = Vec::new();
    let mut rest = r;
    while let Some(start) = rest.find(['[', '(']) {
        let Some(end_rel) = rest[start..].find([']', ')']) else { break };
        let end = start + end_rel;
        let (open, close) = (&rest[start..=start], &rest[end..=end]);
        let inner = &rest[start + 1..end];
        let pred = match inner.split_once(',') {
            None => format!("={}", inner.trim()),
            Some((lo, hi)) => {
                let mut parts = Vec::new();
                if !lo.trim().is_empty() {
                    parts.push(format!("{}{}", if open == "[" { ">=" } else { ">" }, lo.trim()));
                }
                if !hi.trim().is_empty() {
                    parts.push(format!("{}{}", if close == "]" { "<=" } else { "<" }, hi.trim()));
                }
                if parts.is_empty() {
                    "*".to_string()
                } else {
                    parts.join(" ")
                }
            }
        };
        out.push(pred);
        rest = &rest[end + 1..];
    }
    if out.is_empty() {
        vec!["*".into()]
    } else {
        out
    }
}

fn read_text(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn manifest_version(zip: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
    read_text(zip, "META-INF/MANIFEST.MF")?
        .lines()
        .find_map(|l| l.strip_prefix("Implementation-Version:").map(|v| v.trim().to_string()))
}

/// Forge / NeoForge `mods.toml`: every `[[mods]]` entry (the first is the jar's primary mod).
fn parse_mods_toml(text: &str, file: &str, platform: &str, jar_version: Option<&str>) -> Vec<ModMeta> {
    let Ok(doc) = text.parse::<toml::Table>() else { return vec![] };
    let deps = doc.get("dependencies").and_then(|d| d.as_table());
    let mut out = Vec::new();
    for m in doc.get("mods").and_then(|m| m.as_array()).into_iter().flatten() {
        let Some(id) = m.get("modId").and_then(|v| v.as_str()) else { continue };
        let mut version = m.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if version.contains("${file.jarVersion}") {
            version = jar_version.unwrap_or("").to_string();
        }
        let mut meta = ModMeta {
            id: id.to_string(),
            version,
            name: m.get("displayName").and_then(|v| v.as_str()).map(str::to_string),
            file: file.to_string(),
            platform: platform.to_string(),
            ..Default::default()
        };
        for d in deps.and_then(|t| t.get(id)).and_then(|v| v.as_array()).into_iter().flatten() {
            let Some(dep) = d.get("modId").and_then(|v| v.as_str()) else { continue };
            if d.get("side").and_then(|v| v.as_str()).is_some_and(|s| s.eq_ignore_ascii_case("server")) {
                continue;
            }
            let range = maven_range(d.get("versionRange").and_then(|v| v.as_str()).unwrap_or("*"));
            // Forge: mandatory = true|false; NeoForge: type = required|optional|incompatible|discouraged.
            let kind = d.get("type").and_then(|v| v.as_str()).map(str::to_ascii_lowercase);
            let mandatory = d.get("mandatory").and_then(|v| v.as_bool());
            match (kind.as_deref(), mandatory) {
                (Some("required"), _) | (None, Some(true)) => {
                    meta.depends.insert(dep.to_string(), range);
                }
                (Some("incompatible"), _) => {
                    meta.breaks.insert(dep.to_string(), range);
                }
                _ => {}
            }
        }
        out.push(meta);
    }
    out
}

/// Old Forge `mcmod.info` (a JSON list, or `{"modList": [...]}`).
fn parse_mcmod_info(text: &str, file: &str) -> Vec<ModMeta> {
    let cleaned = text.replace(['\n', '\r', '\t'], " ");
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&cleaned) else { return vec![] };
    let list = json
        .as_array()
        .cloned()
        .or_else(|| json.get("modList").and_then(|l| l.as_array()).cloned())
        .unwrap_or_default();
    list.iter()
        .filter_map(|m| {
            let id = m.get("modid").and_then(|v| v.as_str())?;
            let mut meta = ModMeta {
                id: id.to_string(),
                version: m.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                name: m.get("name").and_then(|v| v.as_str()).map(str::to_string),
                file: file.to_string(),
                platform: "legacyforge".into(),
                ..Default::default()
            };
            for d in m.get("requiredMods").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
                let (dep, range) = d.split_once('@').unwrap_or((d, "*"));
                meta.depends.insert(dep.to_string(), maven_range(range));
            }
            Some(meta)
        })
        .collect()
}

/// Metadata platforms in the order to prefer for a pack's loader (multi-loader jars carry several).
fn platform_order(loader: LoaderKind) -> &'static [&'static str] {
    match loader {
        LoaderKind::Quilt => &["quilt", "fabric", "neoforge", "forge", "legacyforge"],
        LoaderKind::Forge => &["forge", "legacyforge", "neoforge", "fabric", "quilt"],
        LoaderKind::NeoForge => &["neoforge", "forge", "fabric", "quilt", "legacyforge"],
        _ => &["fabric", "quilt", "neoforge", "forge", "legacyforge"],
    }
}

/// Read mod metadata (with nested jar-in-jar mods) from jar bytes, preferring the metadata
/// of `loader` when a jar supports several loaders.
pub fn read_jar_bytes(bytes: &[u8], file: &str, depth: u32, loader: LoaderKind) -> Result<Option<ModMeta>> {
    let mut zip = match zip::ZipArchive::new(Cursor::new(bytes)) {
        Ok(z) => z,
        Err(_) => return Ok(None),
    };
    let jar_version = manifest_version(&mut zip);
    let mut found: Vec<Vec<ModMeta>> = Vec::new();
    for (name, platform) in [
        ("fabric.mod.json", "fabric"),
        ("quilt.mod.json", "quilt"),
        ("META-INF/neoforge.mods.toml", "neoforge"),
        ("META-INF/mods.toml", "forge"),
        ("mcmod.info", "legacyforge"),
    ] {
        let Some(text) = read_text(&mut zip, name) else { continue };
        let metas: Vec<ModMeta> = match platform {
            "fabric" | "quilt" => {
                // Some mods have raw newlines inside strings; be lenient.
                let cleaned = text.replace(['\n', '\r', '\t'], " ");
                serde_json::from_str::<serde_json::Value>(&cleaned)
                    .ok()
                    .and_then(|json| if platform == "fabric" { parse_fabric(&json, file) } else { parse_quilt(&json, file) })
                    .into_iter()
                    .collect()
            }
            "legacyforge" => parse_mcmod_info(&text, file),
            _ => {
                let mut v = parse_mods_toml(&text, file, platform, jar_version.as_deref());
                // NeoForge 1.20.2 – 1.20.4 mods still use META-INF/mods.toml.
                if platform == "forge" && v.iter().any(|m| m.depends.contains_key("neoforge")) {
                    for m in &mut v {
                        m.platform = "neoforge".into();
                    }
                }
                v
            }
        };
        if !metas.is_empty() {
            found.push(metas);
        }
    }
    let mut platforms: Vec<String> = Vec::new();
    for f in &found {
        if !platforms.contains(&f[0].platform) {
            platforms.push(f[0].platform.clone());
        }
    }
    let order = platform_order(loader);
    found.sort_by_key(|f| order.iter().position(|o| *o == f[0].platform).unwrap_or(order.len()));
    let Some(mut metas) = found.into_iter().next() else { return Ok(None) };
    let mut meta = metas.remove(0);
    meta.platforms = platforms;
    // Further mods declared by the same jar.
    meta.nested.extend(metas);
    if depth < 3 {
        // Fabric/Quilt jar-in-jar (META-INF/jars) and Forge/NeoForge JarJar (META-INF/jarjar).
        let nested_names: Vec<String> = zip
            .file_names()
            .filter(|n| (n.starts_with("META-INF/jars/") || n.starts_with("META-INF/jarjar/")) && n.ends_with(".jar"))
            .map(str::to_string)
            .collect();
        for n in nested_names {
            let mut buf = Vec::new();
            if let Ok(mut f) = zip.by_name(&n) {
                if f.read_to_end(&mut buf).is_err() {
                    continue;
                }
            }
            if let Ok(Some(mut m)) = read_jar_bytes(&buf, &format!("{file}!/{n}"), depth + 1, loader) {
                // Nested jars are libraries of the outer mod: their loader is the outer one's.
                m.platforms.clear();
                meta.nested.push(m);
            }
        }
    }
    Ok(Some(meta))
}

pub fn scan_mods_dir(dir: &Path, loader: LoaderKind) -> Result<(Vec<ModMeta>, Vec<String>)> {
    let mut mods = Vec::new();
    let mut non_mods = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok((mods, non_mods)) };
    let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jar")).collect();
    paths.sort();
    for p in paths {
        let bytes = std::fs::read(&p).ctx(&p)?;
        let file = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match read_jar_bytes(&bytes, &file, 0, loader)? {
            Some(m) => mods.push(m),
            None => non_mods.push(file),
        }
    }
    Ok((mods, non_mods))
}

/// Does a mod with metadata for `platforms` load on `loader` (Minecraft `mc`)?
pub fn platform_supported(platforms: &[String], loader: LoaderKind, mc: &str) -> bool {
    let has = |p: &str| platforms.iter().any(|x| x == p);
    match loader {
        LoaderKind::Fabric => has("fabric"),
        LoaderKind::Quilt => has("quilt") || has("fabric"),
        LoaderKind::Forge => has("forge") || has("legacyforge"),
        // NeoForge for 1.20.1 is a Forge fork that still loads Forge mods.
        LoaderKind::NeoForge => has("neoforge") || (mc == "1.20.1" && has("forge")),
        LoaderKind::Vanilla => false,
    }
}

/// Every version of each mod id present (top-level jars and jar-in-jar copies). Fabric picks
/// one candidate per id while satisfying dependencies, so any of them may be the one loaded.
fn collect_ids(m: &ModMeta, out: &mut BTreeMap<String, Vec<String>>) {
    out.entry(m.id.clone()).or_default().push(m.version.clone());
    for p in &m.provides {
        out.entry(p.clone()).or_default().push(m.version.clone());
    }
    for n in &m.nested {
        collect_ids(n, out);
    }
}

/// Validate jar-level dependencies against Minecraft version and Java major.
pub fn check_mods(mods: &[ModMeta], minecraft: &str, java_major: Option<u32>, loader: LoaderKind, loader_version: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut ids: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in mods {
        collect_ids(m, &mut ids);
    }
    ids.insert("minecraft".into(), vec![minecraft.into()]);
    if let Some(j) = java_major {
        ids.insert("java".into(), vec![j.to_string()]);
    }
    if !loader_version.is_empty() {
        // Old Forge builds carry the Minecraft version as a suffix ("10.13.4.1614-1.7.10").
        let lv = loader_version.strip_suffix(&format!("-{minecraft}")).unwrap_or(loader_version).to_string();
        for id in loader.builtin_mod_ids() {
            ids.insert((*id).to_string(), vec![lv.clone()]);
        }
        if loader == LoaderKind::NeoForge && minecraft == "1.20.1" {
            ids.insert("forge".into(), vec![lv.clone()]);
        }
        if loader.is_forge_like() {
            // Language providers are versioned with the loader's major version.
            let major = lv.split('.').next().unwrap_or("").to_string();
            ids.insert("javafml".into(), vec![major.clone()]);
            ids.insert("lowcodefml".into(), vec![major]);
        }
    }

    // Jars built for another loader do not load at all.
    for m in mods {
        if !m.platforms.is_empty() && loader != LoaderKind::Vanilla && !platform_supported(&m.platforms, loader, minecraft) {
            problems.push(
                Problem::error(
                    "wrong_loader",
                    format!(
                        "{} ({}) is a {} mod; this pack uses {}",
                        m.name.as_deref().unwrap_or(&m.id),
                        m.file,
                        m.platforms.join("/"),
                        loader.display()
                    ),
                )
                .mods(vec![m.id.clone()])
                .files(vec![m.file.clone()])
                .suggest(format!("replace it with the {} version of the mod (pack_add finds it by slug)", loader.display())),
            );
        }
    }

    // Duplicates among top-level jars.
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for m in mods {
        if let Some(prev) = seen.insert(&m.id, &m.file) {
            problems.push(
                Problem::error("duplicate_mod", format!("mod '{}' is in both {prev} and {}", m.id, m.file))
                    .mods(vec![m.id.clone()])
                    .suggest("remove one of the jars"),
            );
        }
    }

    let mut all: Vec<&ModMeta> = Vec::new();
    fn flatten<'a>(m: &'a ModMeta, out: &mut Vec<&'a ModMeta>) {
        out.push(m);
        for n in &m.nested {
            flatten(n, out);
        }
    }
    for m in mods {
        flatten(m, &mut all);
    }

    let mut reported: BTreeSet<(String, String)> = BTreeSet::new();
    for m in &all {
        for (dep, preds) in &m.depends {
            match ids.get(dep) {
                None if BUILTIN.contains(&dep.as_str()) => {}
                None => {
                    if reported.insert((m.id.clone(), dep.clone())) {
                        problems.push(
                            Problem::error("missing_dependency", format!("{} ({}) requires '{dep}' {}, which is missing", m.name.as_deref().unwrap_or(&m.id), m.id, preds.join(" || ")))
                                .mods(vec![m.id.clone(), dep.clone()])
                                .files(vec![m.file.clone()])
                                .suggest(format!("pack_add id=\"{dep}\" (use search_mods if the Modrinth slug differs)")),
                        );
                    }
                }
                Some(haves) => {
                    let results: Vec<Option<bool>> = haves.iter().map(|h| version_matches(h, preds)).collect();
                    if results.iter().all(|r| *r == Some(false)) {
                        let have = best_version(haves);
                        let kind = if dep == "java" { "wrong_java" } else { "wrong_dependency_version" };
                        let pr = Problem::error(
                            kind,
                            format!("{} ({}) requires {dep} {} but {have} is present", m.name.as_deref().unwrap_or(&m.id), m.id, preds.join(" || ")),
                        )
                        .mods(vec![m.id.clone(), dep.clone()])
                        .files(vec![m.file.clone()])
                        .suggest(if dep == "minecraft" {
                            format!("'{}' is not built for Minecraft {minecraft}; pin a compatible version or remove it", m.id)
                        } else {
                            format!("change versions of '{}' or '{dep}'", m.id)
                        });
                        problems.push(pr);
                    }
                }
            }
        }
        for (other, preds) in &m.breaks {
            if let Some(haves) = ids.get(other) {
                // Unavoidable only when every candidate version is in the broken range.
                if haves.iter().all(|h| version_matches(h, preds) != Some(false)) && other != &m.id {
                    let have = best_version(haves);
                    problems.push(
                        Problem::error("incompatible_mods", format!("{} declares it breaks with '{other}' {} ({have} present)", m.id, preds.join(" || ")))
                            .mods(vec![m.id.clone(), other.clone()])
                            .suggest(format!("remove '{}' or '{other}'", m.id)),
                    );
                }
            }
        }
    }
    problems
}

/// The highest parseable version (for messages).
fn best_version(vs: &[String]) -> String {
    vs.iter()
        .max_by(|a, b| match (parse_ver(a), parse_ver(b)) {
            (Some(x), Some(y)) => cmp_ver(&x, &y),
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => std::cmp::Ordering::Equal,
        })
        .cloned()
        .unwrap_or_default()
}

// ---------- version predicates (Fabric semver-ish) ----------

fn parse_ver(v: &str) -> Option<Vec<u64>> {
    let core = v.split('+').next()?.split('-').next()?;
    let parts: Vec<u64> = core.split('.').map(|p| p.parse::<u64>()).collect::<std::result::Result<_, _>>().ok()?;
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

fn cmp_ver(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            o => return o,
        }
    }
    std::cmp::Ordering::Equal
}

/// The pre-release part (`1.20-3.1.44` -> `3.1.44`), ignoring build metadata after `+`.
fn pre_release(v: &str) -> Option<&str> {
    v.split('+').next()?.split_once('-').map(|(_, p)| p).filter(|p| !p.is_empty())
}

/// SemVer precedence for pre-release parts: a version with one sorts before the same version
/// without; dot-separated identifiers compare numerically when both are numbers.
fn cmp_pre(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (None, None) => Equal,
        (None, Some(_)) => Greater,
        (Some(_), None) => Less,
        (Some(a), Some(b)) => {
            let (mut x, mut y) = (a.split('.'), b.split('.'));
            loop {
                match (x.next(), y.next()) {
                    (None, None) => return Equal,
                    (None, Some(_)) => return Less,
                    (Some(_), None) => return Greater,
                    (Some(p), Some(q)) => {
                        let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                            (Ok(m), Ok(n)) => m.cmp(&n),
                            (Ok(_), Err(_)) => Less,
                            (Err(_), Ok(_)) => Greater,
                            _ => p.cmp(q),
                        };
                        if o != Equal {
                            return o;
                        }
                    }
                }
            }
        }
    }
}

fn single(have: &[u64], have_pre: Option<&str>, c: &str) -> Option<bool> {
    use std::cmp::Ordering::*;
    let c = c.trim();
    if c.is_empty() || c == "*" {
        return Some(true);
    }
    let (op, rest) = if let Some(r) = c.strip_prefix(">=") {
        (">=", r)
    } else if let Some(r) = c.strip_prefix("<=") {
        ("<=", r)
    } else if let Some(r) = c.strip_prefix('>') {
        (">", r)
    } else if let Some(r) = c.strip_prefix('<') {
        ("<", r)
    } else if let Some(r) = c.strip_prefix('=') {
        ("=", r)
    } else if let Some(r) = c.strip_prefix('~') {
        ("~", r)
    } else if let Some(r) = c.strip_prefix('^') {
        ("^", r)
    } else {
        ("=", c)
    };
    let rest = rest.trim();
    // Wildcards like 1.21.x / 1.21.*
    if rest.ends_with(".x") || rest.ends_with(".X") || rest.ends_with(".*") {
        let prefix = parse_ver(&rest[..rest.len() - 2])?;
        return Some(have.len() >= prefix.len() && have[..prefix.len()] == prefix[..]);
    }
    let want = parse_ver(rest)?;
    // A trailing `-` (`>=2.2.4-`) is Fabric's idiom for "including pre-releases": an empty
    // pre-release that sorts below every other one, so keep it rather than reading a release.
    let want_pre = pre_release(rest).or_else(|| rest.split('+').next().filter(|v| v.ends_with('-')).map(|_| ""));
    let o = cmp_ver(have, &want).then_with(|| cmp_pre(have_pre, want_pre));
    Some(match op {
        ">=" => o != Less,
        "<=" => o != Greater,
        ">" => o == Greater,
        "<" => o == Less,
        "=" => o == Equal,
        "~" => {
            // >= want, < next minor
            let mut upper = want.clone();
            if upper.len() < 2 {
                upper.resize(2, 0);
            }
            upper[1] += 1;
            upper.truncate(2);
            o != Less && cmp_ver(have, &upper) == Less
        }
        "^" => {
            // Fabric: same major version (no npm-style special case for 0.x).
            let upper = vec![want.first().copied().unwrap_or(0) + 1];
            o != Less && cmp_ver(have, &upper) == Less
        }
        _ => return None,
    })
}

/// `preds` are OR-ed; each may contain space-separated AND-ed constraints.
/// Returns None when versions cannot be interpreted.
pub fn version_matches(have: &str, preds: &[String]) -> Option<bool> {
    let hv = parse_ver(have)?;
    let hpre = pre_release(have);
    let mut any_parsed = false;
    for p in preds {
        let mut all = true;
        let mut parsed = true;
        for c in p.split_whitespace() {
            match single(&hv, hpre, c) {
                Some(true) => {}
                Some(false) => all = false,
                None => parsed = false,
            }
        }
        if p.trim().is_empty() || p.trim() == "*" {
            return Some(true);
        }
        if parsed {
            any_parsed = true;
            if all {
                return Some(true);
            }
        }
    }
    if any_parsed {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn predicates() {
        assert_eq!(version_matches("1.21.1", &s(&["~1.21"])), Some(true));
        assert_eq!(version_matches("1.20.1", &s(&["~1.21"])), Some(false));
        assert_eq!(version_matches("1.21.1", &s(&[">=1.21 <1.21.2"])), Some(true));
        assert_eq!(version_matches("1.21.3", &s(&[">=1.21 <1.21.2"])), Some(false));
        assert_eq!(version_matches("1.20.1", &s(&["1.20", "1.20.1"])), Some(true));
        assert_eq!(version_matches("1.21.1", &s(&["1.21.x"])), Some(true));
        assert_eq!(version_matches("21", &s(&[">=17"])), Some(true));
        assert_eq!(version_matches("0.16.14", &s(&[">=0.15.0"])), Some(true));
        assert_eq!(version_matches("0.92.2+1.20.1", &s(&["*"])), Some(true));
        assert_eq!(version_matches("0.6.3", &s(&["^0.4.3"])), Some(true));
        assert_eq!(version_matches("1.20-3.1.44", &s(&["<=1.20-2.7.32"])), Some(false));
        assert_eq!(version_matches("1.20-2.7.30", &s(&["<=1.20-2.7.32"])), Some(true));
        assert_eq!(version_matches("1.0.0-beta.2", &s(&[">=1.0.0-beta.10"])), Some(false));
        assert_eq!(version_matches("1.0.0", &s(&["^0.4.3"])), Some(false));
        assert_eq!(version_matches("1.21.1", &s(&["1.21-alpha.24.1.a"])), Some(false));
        // Trailing `-` admits pre-releases of that version (Wilder Wild -> frozenlib >=2.2.4-).
        assert_eq!(version_matches("2.2.4-mc1.21.1", &s(&[">=2.2.4-"])), Some(true));
        assert_eq!(version_matches("2.2.4", &s(&[">=2.2.4-"])), Some(true));
        assert_eq!(version_matches("2.2.3-mc1.21.1", &s(&[">=2.2.4-"])), Some(false));
        assert_eq!(version_matches("2.2.4-mc1.21.1", &s(&[">=2.2.4"])), Some(false));
    }

    #[test]
    fn missing_and_present_deps() {
        let a = ModMeta {
            id: "a".into(),
            version: "1.0".into(),
            depends: BTreeMap::from([("fabric-api".to_string(), s(&["*"])), ("minecraft".to_string(), s(&["~1.21"]))]),
            ..Default::default()
        };
        let p = check_mods(&[a.clone()], "1.21.1", Some(21), LoaderKind::Fabric, "0.16.14");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, "missing_dependency");
        let api = ModMeta {
            id: "fabric-api".into(),
            version: "0.100.0".into(),
            nested: vec![ModMeta { id: "fabric-api-base".into(), version: "0.4".into(), ..Default::default() }],
            ..Default::default()
        };
        assert!(check_mods(&[a.clone(), api.clone()], "1.21.1", Some(21), LoaderKind::Fabric, "").is_empty());
        let p = check_mods(&[a, api], "1.20.1", Some(17), LoaderKind::Fabric, "");
        assert!(p.iter().any(|x| x.kind == "wrong_dependency_version"));
    }

    #[test]
    fn older_nested_copy_does_not_hide_newer_top_level() {
        let user = ModMeta {
            id: "wands".into(),
            version: "2.6.9".into(),
            depends: BTreeMap::from([("cloth-config".to_string(), s(&[">=11.1.106"]))]),
            nested: vec![ModMeta { id: "cloth-config".into(), version: "11.0.99".into(), ..Default::default() }],
            ..Default::default()
        };
        let cloth = ModMeta { id: "cloth-config".into(), version: "11.1.136".into(), ..Default::default() };
        assert!(check_mods(&[user.clone(), cloth], "1.20.1", Some(17), LoaderKind::Fabric, "").is_empty());
        let p = check_mods(&[user], "1.20.1", Some(17), LoaderKind::Fabric, "");
        assert!(p[0].message.contains("11.0.99"));
    }

    fn jar(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            for (name, text) in files {
                z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
                std::io::Write::write_all(&mut z, text.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn maven_ranges() {
        assert_eq!(maven_range("[47,)"), s(&[">=47"]));
        assert_eq!(maven_range("[1.20.1,1.21)"), s(&[">=1.20.1 <1.21"]));
        assert_eq!(maven_range("(,2.0]"), s(&["<=2.0"]));
        assert_eq!(maven_range("[1.0]"), s(&["=1.0"]));
        assert_eq!(maven_range("[1,2),[3,4)"), s(&[">=1 <2", ">=3 <4"]));
        assert_eq!(maven_range("1.0"), s(&["*"]));
        assert_eq!(version_matches("47.4.28", &maven_range("[47,)")), Some(true));
        assert_eq!(version_matches("1.20.1", &maven_range("[1.20,1.21)")), Some(true));
        assert_eq!(version_matches("1.21.1", &maven_range("[1.20,1.21)")), Some(false));
    }

    #[test]
    fn forge_mods_toml() {
        let toml = r#"
modLoader="javafml"
loaderVersion="[47,)"
[[mods]]
modId="jei"
version="${file.jarVersion}"
displayName="Just Enough Items"
[[dependencies.jei]]
modId="forge"
mandatory=true
versionRange="[47.1,)"
side="BOTH"
[[dependencies.jei]]
modId="minecraft"
mandatory=true
versionRange="[1.20.1,1.20.2)"
[[dependencies.jei]]
modId="optionalthing"
mandatory=false
[[dependencies.jei]]
modId="serveronly"
mandatory=true
side="SERVER"
"#;
        let bytes = jar(&[("META-INF/mods.toml", toml), ("META-INF/MANIFEST.MF", "Manifest-Version: 1.0
Implementation-Version: 15.2.0.27
")]);
        let m = read_jar_bytes(&bytes, "jei.jar", 0, LoaderKind::Forge).unwrap().unwrap();
        assert_eq!(m.id, "jei");
        assert_eq!(m.version, "15.2.0.27");
        assert_eq!(m.platform, "forge");
        assert_eq!(m.depends.keys().cloned().collect::<Vec<_>>(), vec!["forge", "minecraft"]);
        assert!(check_mods(&[m.clone()], "1.20.1", Some(17), LoaderKind::Forge, "47.4.28").is_empty());
        let p = check_mods(&[m.clone()], "1.20.1", Some(17), LoaderKind::Forge, "47.0.1");
        assert_eq!(p[0].kind, "wrong_dependency_version");
        // A Forge mod in a Fabric pack does not load.
        let p = check_mods(&[m.clone()], "1.20.1", Some(17), LoaderKind::Fabric, "0.16.14");
        assert!(p.iter().any(|x| x.kind == "wrong_loader"));
        // NeoForge for 1.20.1 loads Forge mods; later NeoForge does not.
        assert!(check_mods(&[m.clone()], "1.20.1", Some(17), LoaderKind::NeoForge, "47.1.106").is_empty());
        assert!(check_mods(&[m], "1.21.1", Some(21), LoaderKind::NeoForge, "21.1.77").iter().any(|x| x.kind == "wrong_loader"));
    }

    #[test]
    fn neoforge_and_multiloader_jars() {
        let neo = r#"
[[mods]]
modId="sodium"
version="0.6.0"
[[dependencies.sodium]]
modId="neoforge"
type="required"
versionRange="[21.1,)"
[[dependencies.sodium]]
modId="embeddium"
type="incompatible"
"#;
        let fabric = r#"{"schemaVersion":1,"id":"sodium","version":"0.6.0+fabric","depends":{"fabricloader":">=0.16"}}"#;
        let bytes = jar(&[("META-INF/neoforge.mods.toml", neo), ("fabric.mod.json", fabric)]);
        let on_neo = read_jar_bytes(&bytes, "sodium.jar", 0, LoaderKind::NeoForge).unwrap().unwrap();
        assert_eq!(on_neo.platform, "neoforge");
        assert!(on_neo.breaks.contains_key("embeddium"));
        assert!(check_mods(&[on_neo], "1.21.1", Some(21), LoaderKind::NeoForge, "21.1.77").is_empty());
        let on_fabric = read_jar_bytes(&bytes, "sodium.jar", 0, LoaderKind::Fabric).unwrap().unwrap();
        assert_eq!(on_fabric.platform, "fabric");
        assert!(check_mods(&[on_fabric], "1.21.1", Some(21), LoaderKind::Fabric, "0.16.14").is_empty());
    }

    #[test]
    fn legacy_mcmod_info() {
        let info = r#"[{"modid":"journeymap","name":"JourneyMap","version":"5.7.1","requiredMods":["Forge@[14.23,)"]}]"#;
        let bytes = jar(&[("mcmod.info", info)]);
        let m = read_jar_bytes(&bytes, "jm.jar", 0, LoaderKind::Forge).unwrap().unwrap();
        assert_eq!((m.id.as_str(), m.platform.as_str()), ("journeymap", "legacyforge"));
        assert!(check_mods(&[m], "1.12.2", Some(8), LoaderKind::Forge, "14.23.5.2864").is_empty());
    }
}
