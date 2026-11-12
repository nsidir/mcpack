//! Pre-launch checks: lock freshness, manual downloads, and jar-level metadata
//! (fabric.mod.json / quilt.mod.json) dependency validation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::Path;

use serde::Serialize;

use crate::diagnose::Problem;
use crate::error::{IoContext, Result};

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
const BUILTIN: &[&str] = &["minecraft", "java", "fabricloader", "fabric-loader", "quilt_loader", "quilt_base"];

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

/// Read mod metadata (with nested jar-in-jar mods) from jar bytes.
pub fn read_jar_bytes(bytes: &[u8], file: &str, depth: u32) -> Result<Option<ModMeta>> {
    let mut zip = match zip::ZipArchive::new(Cursor::new(bytes)) {
        Ok(z) => z,
        Err(_) => return Ok(None),
    };
    let mut meta: Option<ModMeta> = None;
    for name in ["fabric.mod.json", "quilt.mod.json"] {
        if meta.is_some() {
            break;
        }
        if let Ok(mut f) = zip.by_name(name) {
            let mut s = String::new();
            if f.read_to_string(&mut s).is_ok() {
                // Some mods have raw newlines inside strings; be lenient.
                let cleaned = s.replace(['\n', '\r', '\t'], " ");
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&cleaned) {
                    meta = if name == "fabric.mod.json" { parse_fabric(&json, file) } else { parse_quilt(&json, file) };
                }
            }
        }
    }
    let Some(mut meta) = meta else { return Ok(None) };
    if depth < 3 {
        let nested_names: Vec<String> = zip
            .file_names()
            .filter(|n| n.starts_with("META-INF/jars/") && n.ends_with(".jar"))
            .map(str::to_string)
            .collect();
        for n in nested_names {
            let mut buf = Vec::new();
            if let Ok(mut f) = zip.by_name(&n) {
                if f.read_to_end(&mut buf).is_err() {
                    continue;
                }
            }
            if let Ok(Some(m)) = read_jar_bytes(&buf, &format!("{file}!/{n}"), depth + 1) {
                meta.nested.push(m);
            }
        }
    }
    Ok(Some(meta))
}

pub fn scan_mods_dir(dir: &Path) -> Result<(Vec<ModMeta>, Vec<String>)> {
    let mut mods = Vec::new();
    let mut non_mods = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok((mods, non_mods)) };
    let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jar")).collect();
    paths.sort();
    for p in paths {
        let bytes = std::fs::read(&p).ctx(&p)?;
        let file = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match read_jar_bytes(&bytes, &file, 0)? {
            Some(m) => mods.push(m),
            None => non_mods.push(file),
        }
    }
    Ok((mods, non_mods))
}

fn collect_ids(m: &ModMeta, out: &mut BTreeMap<String, String>) {
    out.entry(m.id.clone()).or_insert_with(|| m.version.clone());
    for p in &m.provides {
        out.entry(p.clone()).or_insert_with(|| m.version.clone());
    }
    for n in &m.nested {
        collect_ids(n, out);
    }
}

/// Validate jar-level dependencies against Minecraft version and Java major.
pub fn check_mods(mods: &[ModMeta], minecraft: &str, java_major: Option<u32>, loader_version: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    for m in mods {
        collect_ids(m, &mut ids);
    }
    ids.insert("minecraft".into(), minecraft.into());
    if let Some(j) = java_major {
        ids.insert("java".into(), j.to_string());
    }
    if !loader_version.is_empty() {
        ids.insert("fabricloader".into(), loader_version.into());
        ids.insert("fabric-loader".into(), loader_version.into());
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
                Some(have) => {
                    if version_matches(have, preds) == Some(false) {
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
            if let Some(have) = ids.get(other) {
                if version_matches(have, preds) != Some(false) && other != &m.id {
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

fn single(have: &[u64], c: &str) -> Option<bool> {
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
    let o = cmp_ver(have, &want);
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
            let mut upper = vec![want.first().copied().unwrap_or(0) + 1];
            if want.first() == Some(&0) && want.len() > 1 {
                upper = vec![0, want[1] + 1];
            }
            o != Less && cmp_ver(have, &upper) == Less
        }
        _ => return None,
    })
}

/// `preds` are OR-ed; each may contain space-separated AND-ed constraints.
/// Returns None when versions cannot be interpreted.
pub fn version_matches(have: &str, preds: &[String]) -> Option<bool> {
    let hv = parse_ver(have)?;
    let mut any_parsed = false;
    for p in preds {
        let mut all = true;
        let mut parsed = true;
        for c in p.split_whitespace() {
            match single(&hv, c) {
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
        assert_eq!(version_matches("1.21.1", &s(&["1.21-alpha.24.1.a"])), Some(false));
    }

    #[test]
    fn missing_and_present_deps() {
        let a = ModMeta {
            id: "a".into(),
            version: "1.0".into(),
            depends: BTreeMap::from([("fabric-api".to_string(), s(&["*"])), ("minecraft".to_string(), s(&["~1.21"]))]),
            ..Default::default()
        };
        let p = check_mods(&[a.clone()], "1.21.1", Some(21), "0.16.14");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, "missing_dependency");
        let api = ModMeta {
            id: "fabric-api".into(),
            version: "0.100.0".into(),
            nested: vec![ModMeta { id: "fabric-api-base".into(), version: "0.4".into(), ..Default::default() }],
            ..Default::default()
        };
        assert!(check_mods(&[a.clone(), api.clone()], "1.21.1", Some(21), "").is_empty());
        let p = check_mods(&[a, api], "1.20.1", Some(17), "");
        assert!(p.iter().any(|x| x.kind == "wrong_dependency_version"));
    }
}
