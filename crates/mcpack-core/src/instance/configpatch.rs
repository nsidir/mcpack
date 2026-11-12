//! Apply structured key patches to config files of various formats.

use std::path::Path;

use crate::error::{Error, IoContext, Result};
use crate::pack::edit::json_to_value;
use crate::util;

/// Flatten a patch table into (path, value) pairs. Nested tables recurse; keys containing
/// dots are split (`"a.b" = 1` == `[a] b = 1`).
pub fn flatten(table: &toml::Table) -> Vec<(Vec<String>, toml::Value)> {
    let mut out = Vec::new();
    fn walk(prefix: &[String], t: &toml::Table, out: &mut Vec<(Vec<String>, toml::Value)>) {
        for (k, v) in t {
            let mut path = prefix.to_vec();
            path.extend(k.split('.').map(str::to_string));
            match v {
                toml::Value::Table(sub) => walk(&path, sub, out),
                other => out.push((path, other.clone())),
            }
        }
    }
    walk(&[], table, &mut out);
    out
}

pub fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

/// String form for line-based formats (options.txt, .properties).
pub fn scalar_string(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => serde_json::to_string(&toml_to_json(other)).unwrap_or_default(),
    }
}

/// Patch `path`; returns whether content changed. With `dry_run`, nothing is written.
pub fn patch(path: &Path, table: &toml::Table, dry_run: bool) -> Result<bool> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let existing = if path.exists() { Some(std::fs::read_to_string(path).ctx(path)?) } else { None };
    let pairs = flatten(table);
    let new_text = match ext.as_str() {
        "json" | "json5" | "jsonc" | "mcmeta" => {
            let before: serde_json::Value = match &existing {
                Some(t) if !t.trim().is_empty() => serde_json::from_str(&strip_json_comments(t))
                    .map_err(|e| Error::Invalid(format!("{}: cannot parse as JSON: {e}", path.display())))?,
                _ => serde_json::Value::Object(Default::default()),
            };
            let mut after = before.clone();
            for (p, v) in &pairs {
                set_json(&mut after, p, toml_to_json(v));
            }
            if after == before && existing.is_some() {
                return Ok(false);
            }
            serde_json::to_string_pretty(&after)? + "\n"
        }
        "toml" => {
            let mut doc: toml_edit::DocumentMut = existing.as_deref().unwrap_or("").parse()?;
            for (p, v) in &pairs {
                let val = json_to_value(&toml_to_json(v))?;
                crate::pack::edit::set_path(doc.as_item_mut(), p, toml_edit::Item::Value(val))?;
            }
            doc.to_string()
        }
        "properties" | "cfg" | "conf" => patch_lines(existing.as_deref().unwrap_or(""), &pairs, '='),
        "txt" => patch_lines(existing.as_deref().unwrap_or(""), &pairs, ':'),
        other => {
            return Err(Error::Invalid(format!(
                "unsupported config format '.{other}' for {} (supported: json, json5/jsonc, toml, properties, cfg, txt)",
                path.display()
            )))
        }
    };
    if existing.as_deref() == Some(new_text.as_str()) {
        return Ok(false);
    }
    if !dry_run {
        util::write_atomic(path, new_text.as_bytes())?;
    }
    Ok(true)
}

fn set_json(root: &mut serde_json::Value, path: &[String], v: serde_json::Value) {
    let mut cur = root;
    for (i, key) in path.iter().enumerate() {
        let last = i == path.len() - 1;
        if let Ok(idx) = key.parse::<usize>() {
            if cur.as_array().is_some_and(|a| idx < a.len()) {
                let arr = cur.as_array_mut().expect("array");
                if last {
                    arr[idx] = v;
                    return;
                }
                cur = &mut arr[idx];
                continue;
            }
        }
        if !cur.is_object() {
            *cur = serde_json::Value::Object(Default::default());
        }
        let obj = cur.as_object_mut().expect("object");
        if last {
            obj.insert(key.clone(), v);
            return;
        }
        cur = obj.entry(key.clone()).or_insert_with(|| serde_json::Value::Object(Default::default()));
    }
}

fn patch_lines(text: &str, pairs: &[(Vec<String>, toml::Value)], sep: char) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for (p, v) in pairs {
        let key = p.join(".");
        let val = scalar_string(v);
        let mut found = false;
        for line in lines.iter_mut() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.starts_with('!') {
                continue;
            }
            if let Some((k, _)) = line.split_once(sep) {
                if k.trim() == key {
                    *line = format!("{key}{sep}{val}");
                    found = true;
                    break;
                }
            }
        }
        if !found {
            lines.push(format!("{key}{sep}{val}"));
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Remove `//` and `/* */` comments outside strings (for json5/jsonc best-effort parsing).
pub fn strip_json_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(c);
            i += 1;
        }
    }
    // Trailing commas are common in json5: drop `,` before `}`/`]`.
    let re = regex::Regex::new(r",(\s*[}\]])").expect("regex");
    re.replace_all(&out, "$1").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> toml::Table {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn json_patch() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("c.json");
        std::fs::write(&f, "{\"quality\": {\"weather\": \"FANCY\", \"x\": 1}, // c\n \"keep\": true,}").unwrap();
        assert!(patch(&f, &t(r#""quality.weather" = "FAST""#), false).unwrap());
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["quality"]["weather"], "FAST");
        assert_eq!(v["quality"]["x"], 1);
        assert_eq!(v["keep"], true);
        // Idempotent.
        assert!(!patch(&f, &t(r#""quality.weather" = "FAST""#), false).unwrap());
    }

    #[test]
    fn toml_and_props_patch() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("c.toml");
        std::fs::write(&f, "# keep me\n[general]\nenabled = false\n").unwrap();
        assert!(patch(&f, &t("general = { enabled = true, level = 3 }"), false).unwrap());
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("# keep me"));
        let v: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(v["general"]["enabled"].as_bool(), Some(true));
        assert_eq!(v["general"]["level"].as_integer(), Some(3));

        let p = d.path().join("x.properties");
        std::fs::write(&p, "# c\na=1\nb=2\n").unwrap();
        assert!(patch(&p, &t("b = 5\nc = \"z\""), false).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# c\na=1\nb=5\nc=z\n");
    }

    #[test]
    fn dry_run_does_not_write() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("new.json");
        assert!(patch(&f, &t("a = 1"), true).unwrap());
        assert!(!f.exists());
    }
}
