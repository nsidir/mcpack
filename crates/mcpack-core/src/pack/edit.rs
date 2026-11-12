//! Format-preserving edits to `pack.toml` (comments and layout survive).

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

use super::schema::{LoaderKind, ModSpec, PackFile};
use super::{pack_dir, Pack, PACK_FILE};
use crate::error::{Error, IoContext, Result};
use crate::util;

/// Create a new pack directory with a starter `pack.toml`.
pub fn init(dir: &Path, name: &str, minecraft: &str, loader: LoaderKind, loader_version: &str) -> Result<Pack> {
    let file = dir.join(PACK_FILE);
    if file.exists() {
        return Err(Error::Invalid(format!("{} already exists", file.display())));
    }
    util::ensure_dir(dir)?;
    util::ensure_dir(&dir.join("overrides"))?;
    let text = format!(
        r#"# mcpack pack definition — the source of truth for this modpack.
# Edit by hand or through mcpack tools, then `sync` (or `watch_start`) to apply.

[pack]
name = {name}
minecraft = {mc}
loader = {{ kind = "{kind}", version = {lv} }}

[jvm]
memory = "4G"

# options.txt values, e.g.:
# [options]
# renderDistance = 12

# Structured config patches (json/toml/properties/txt), e.g.:
# [config."config/sodium-options.json"]
# "quality.weather_quality" = "FAST"

# Content (source = modrinth | curseforge | url | file), e.g.:
# [[mods]]
# id = "sodium"

# Automated-testing profile: agent injected, light settings for fast boots.
[profiles.test]
agent = true
options = {{ renderDistance = 4, simulationDistance = 5, maxFps = 60 }}
"#,
        name = toml_str(name),
        mc = toml_str(minecraft),
        kind = loader.as_str(),
        lv = toml_str(if loader_version.is_empty() { "latest" } else { loader_version }),
    );
    std::fs::write(&file, text).ctx(&file)?;
    let gi = dir.join(".gitignore");
    if !gi.exists() {
        std::fs::write(&gi, "run/\n").ctx(&gi)?;
    }
    Pack::load(dir)
}

fn toml_str(s: &str) -> String {
    Value::from(s).to_string().trim().to_string()
}

/// Parse a path like `profiles.test.jvm.memory`, `config."sodium-options.json".a`, `mods[0].version`.
pub fn parse_path(s: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let quote = c;
                let mut closed = false;
                for c2 in chars.by_ref() {
                    if c2 == quote {
                        closed = true;
                        break;
                    }
                    cur.push(c2);
                }
                if !closed {
                    return Err(Error::Invalid(format!("unterminated quote in path: {s}")));
                }
            }
            '.' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            '[' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut idx = String::new();
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                    idx.push(c2);
                }
                out.push(idx.trim().trim_matches('"').to_string());
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        return Err(Error::Invalid("empty path".into()));
    }
    Ok(out)
}

pub fn json_to_value(v: &serde_json::Value) -> Result<Value> {
    Ok(match v {
        serde_json::Value::Null => return Err(Error::Invalid("null is not a TOML value (use unset)".into())),
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else {
                Value::from(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(json_to_value(x)?);
            }
            Value::Array(arr)
        }
        serde_json::Value::Object(o) => {
            let mut t = InlineTable::new();
            for (k, x) in o {
                if x.is_null() {
                    continue;
                }
                t.insert(k, json_to_value(x)?);
            }
            Value::InlineTable(t)
        }
    })
}

pub struct PackEditor {
    dir: PathBuf,
    doc: DocumentMut,
}

impl PackEditor {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let dir = pack_dir(path.as_ref());
        let file = dir.join(PACK_FILE);
        let text = std::fs::read_to_string(&file).ctx(&file)?;
        Ok(Self { dir, doc: text.parse::<DocumentMut>()? })
    }

    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// Validate and write. Nothing is written if the result would be an invalid pack.
    pub fn save(&self) -> Result<Pack> {
        let text = self.doc.to_string();
        let file: PackFile = toml::from_str(&text)?;
        let pack = Pack { dir: self.dir.clone(), file };
        pack.validate()?;
        util::write_atomic(&self.dir.join(PACK_FILE), text.as_bytes())?;
        Ok(pack)
    }

    /// Set any value by path. Objects become tables, arrays/scalars become values.
    pub fn set(&mut self, path: &[String], value: &serde_json::Value) -> Result<()> {
        if path.is_empty() {
            return Err(Error::Invalid("empty path".into()));
        }
        let item = match json_to_value(value)? {
            Value::InlineTable(t) if path.len() <= 3 => Item::Table(t.into_table()),
            other => Item::Value(other),
        };
        set_in(self.doc.as_item_mut(), path, item)
    }

    /// Remove a value by path. Returns whether something was removed.
    pub fn unset(&mut self, path: &[String]) -> Result<bool> {
        if path.is_empty() {
            return Err(Error::Invalid("empty path".into()));
        }
        unset_in(self.doc.as_item_mut(), path)
    }

    fn mods_array(&mut self, profile: Option<&str>) -> Result<&mut toml_edit::ArrayOfTables> {
        let root = self.doc.as_table_mut();
        let parent: &mut Table = match profile {
            None => root,
            Some(p) if p == super::DEFAULT_PROFILE => root,
            Some(p) => {
                let profiles = root.entry("profiles").or_insert_with(|| {
                    let mut t = Table::new();
                    t.set_implicit(true);
                    Item::Table(t)
                });
                let profiles = profiles
                    .as_table_mut()
                    .ok_or_else(|| Error::Invalid("`profiles` is not a table".into()))?;
                let prof = profiles.entry(p).or_insert_with(|| Item::Table(Table::new()));
                if let Some(it) = prof.as_inline_table().cloned() {
                    *prof = Item::Table(it.into_table());
                }
                prof.as_table_mut().ok_or_else(|| Error::Invalid(format!("profile `{p}` is not a table")))?
            }
        };
        let mods = parent
            .entry("mods")
            .or_insert_with(|| Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
        if mods.is_array() {
            // Convert inline array of tables into [[mods]] form.
            let arr = mods.as_array().cloned().unwrap_or_default();
            let mut aot = toml_edit::ArrayOfTables::new();
            for v in arr.iter() {
                if let Some(t) = v.as_inline_table() {
                    aot.push(t.clone().into_table());
                }
            }
            *mods = Item::ArrayOfTables(aot);
        }
        mods.as_array_of_tables_mut()
            .ok_or_else(|| Error::Invalid("`mods` must be an array of tables".into()))
    }

    /// Add (or replace an existing entry for the same project). Returns true if replaced.
    pub fn add_mod(&mut self, profile: Option<&str>, spec: &ModSpec) -> Result<bool> {
        let doc = toml_edit::ser::to_document(spec).map_err(|e| Error::Other(format!("serialize mod spec: {e}")))?;
        let table = doc.as_table().clone();
        let aot = self.mods_array(profile)?;
        for i in 0..aot.len() {
            let existing = aot.get(i).map(|t| t.to_string()).unwrap_or_default();
            if let Ok(e) = toml::from_str::<ModSpec>(&existing) {
                if e.source == spec.source && e.ident().eq_ignore_ascii_case(&spec.ident()) {
                    if let Some(t) = aot.get_mut(i) {
                        *t = table;
                    }
                    return Ok(true);
                }
            }
        }
        aot.push(table);
        Ok(false)
    }

    /// Remove matching entries. Returns how many were removed.
    pub fn remove_mod(&mut self, profile: Option<&str>, ident: &str) -> Result<usize> {
        let aot = self.mods_array(profile)?;
        let mut removed = 0;
        let mut i = 0;
        while i < aot.len() {
            let text = aot.get(i).map(|t| t.to_string()).unwrap_or_default();
            let hit = toml::from_str::<ModSpec>(&text).map(|m| m.matches(ident)).unwrap_or(false);
            if hit {
                aot.remove(i);
                removed += 1;
            } else {
                i += 1;
            }
        }
        Ok(removed)
    }
}

/// Set `new` at `path` inside any TOML item (used for config-file patching too).
pub fn set_path(item: &mut Item, path: &[String], new: Item) -> Result<()> {
    if path.is_empty() {
        return Err(Error::Invalid("empty path".into()));
    }
    set_in(item, path, new)
}

fn set_in(item: &mut Item, path: &[String], new: Item) -> Result<()> {
    let (head, rest) = path.split_first().expect("non-empty path");
    if let Ok(idx) = head.parse::<usize>() {
        if let Some(aot) = item.as_array_of_tables_mut() {
            let len = aot.len();
            let t = aot
                .get_mut(idx)
                .ok_or_else(|| Error::Invalid(format!("index {idx} out of range (len {len})")))?;
            if rest.is_empty() {
                *t = match new {
                    Item::Table(nt) => nt,
                    Item::Value(Value::InlineTable(it)) => it.into_table(),
                    _ => return Err(Error::Invalid("array-of-tables element must be an object".into())),
                };
                return Ok(());
            }
            return set_in_tl(t, rest, new, false);
        }
        if let Some(arr) = item.as_array_mut() {
            if rest.is_empty() {
                let v = new.into_value().map_err(|_| Error::Invalid("cannot put a table in an array".into()))?;
                if idx == arr.len() {
                    arr.push(v);
                } else if idx < arr.len() {
                    arr.replace(idx, v);
                } else {
                    return Err(Error::Invalid(format!("index {idx} out of range (len {})", arr.len())));
                }
                return Ok(());
            }
            let el = arr
                .get_mut(idx)
                .and_then(|v| v.as_inline_table_mut())
                .ok_or_else(|| Error::Invalid(format!("element {idx} is not a table")))?;
            return set_in_tl(el, rest, new, true);
        }
    }
    let inline = item.is_inline_table();
    let tl = item
        .as_table_like_mut()
        .ok_or_else(|| Error::Invalid(format!("cannot descend into non-table at `{head}`")))?;
    set_in_tl(tl, path, new, inline)
}

fn set_in_tl(tl: &mut dyn TableLike, path: &[String], new: Item, inline: bool) -> Result<()> {
    let (head, rest) = path.split_first().expect("non-empty path");
    if rest.is_empty() {
        let new = if inline {
            Item::Value(new.into_value().map_err(|_| Error::Invalid("invalid inline value".into()))?)
        } else {
            new
        };
        tl.insert(head, new);
        return Ok(());
    }
    if tl.get(head).is_none() {
        let child = if inline {
            Item::Value(Value::InlineTable(InlineTable::new()))
        } else {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        };
        tl.insert(head, child);
    }
    let child = tl.get_mut(head).expect("just inserted");
    set_in(child, rest, new)
}

fn unset_in(item: &mut Item, path: &[String]) -> Result<bool> {
    let (head, rest) = path.split_first().expect("non-empty path");
    if let Ok(idx) = head.parse::<usize>() {
        if let Some(aot) = item.as_array_of_tables_mut() {
            if rest.is_empty() {
                if idx < aot.len() {
                    aot.remove(idx);
                    return Ok(true);
                }
                return Ok(false);
            }
            return match aot.get_mut(idx) {
                Some(t) => unset_tl(t, rest),
                None => Ok(false),
            };
        }
        if let Some(arr) = item.as_array_mut() {
            if rest.is_empty() {
                if idx < arr.len() {
                    arr.remove(idx);
                    return Ok(true);
                }
                return Ok(false);
            }
            return match arr.get_mut(idx).and_then(|v| v.as_inline_table_mut()) {
                Some(t) => unset_tl(t, rest),
                None => Ok(false),
            };
        }
    }
    match item.as_table_like_mut() {
        Some(tl) => unset_tl(tl, path),
        None => Ok(false),
    }
}

fn unset_tl(tl: &mut dyn TableLike, path: &[String]) -> Result<bool> {
    let (head, rest) = path.split_first().expect("non-empty path");
    if rest.is_empty() {
        return Ok(tl.remove(head).is_some());
    }
    match tl.get_mut(head) {
        Some(i) => unset_in(i, rest),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::schema::Source;

    #[test]
    fn paths() {
        assert_eq!(parse_path("a.b.c").unwrap(), vec!["a", "b", "c"]);
        assert_eq!(
            parse_path(r#"config."sodium-options.json".quality"#).unwrap(),
            vec!["config", "sodium-options.json", "quality"]
        );
        assert_eq!(parse_path("mods[0].version").unwrap(), vec!["mods", "0", "version"]);
    }

    #[test]
    fn edit_roundtrip_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path(), "demo", "1.21.1", LoaderKind::Fabric, "latest").unwrap();
        let mut ed = PackEditor::open(dir.path()).unwrap();
        ed.set(&parse_path("jvm.memory").unwrap(), &serde_json::json!("6G")).unwrap();
        ed.set(&parse_path("profiles.test.options.renderDistance").unwrap(), &serde_json::json!(2)).unwrap();
        ed.set(&parse_path(r#"config."config/x.json".a.b"#).unwrap(), &serde_json::json!(true)).unwrap();
        ed.add_mod(None, &ModSpec { id: Some("sodium".into()), ..Default::default() }).unwrap();
        ed.add_mod(Some("test"), &ModSpec { id: Some("modmenu".into()), ..Default::default() }).unwrap();
        let replaced = ed
            .add_mod(None, &ModSpec { id: Some("sodium".into()), version: Some("0.6.0".into()), ..Default::default() })
            .unwrap();
        assert!(replaced);
        let pack = ed.save().unwrap();
        let text = std::fs::read_to_string(dir.path().join(PACK_FILE)).unwrap();
        assert!(text.contains("# mcpack pack definition"));
        assert_eq!(pack.file.jvm.memory.as_deref(), Some("6G"));
        assert_eq!(pack.file.mods.len(), 1);
        assert_eq!(pack.file.mods[0].version.as_deref(), Some("0.6.0"));
        assert_eq!(pack.file.mods[0].source, Source::Modrinth);
        let test = pack.effective("test").unwrap();
        assert_eq!(test.options["renderDistance"].as_integer(), Some(2));
        assert_eq!(test.mods.len(), 2);
        assert!(pack.file.config["config/x.json"]["a"]["b"].as_bool().unwrap());

        let mut ed = PackEditor::open(dir.path()).unwrap();
        assert_eq!(ed.remove_mod(None, "sodium").unwrap(), 1);
        assert!(ed.unset(&parse_path("jvm.memory").unwrap()).unwrap());
        let pack = ed.save().unwrap();
        assert!(pack.file.mods.is_empty());
        assert!(pack.file.jvm.memory.is_none());
    }

    #[test]
    fn invalid_edit_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path(), "demo", "1.21.1", LoaderKind::Fabric, "latest").unwrap();
        let mut ed = PackEditor::open(dir.path()).unwrap();
        ed.set(&parse_path("pack.minecraft").unwrap(), &serde_json::json!(5)).unwrap();
        assert!(ed.save().is_err());
        // File unchanged.
        assert!(Pack::load(dir.path()).is_ok());
    }
}
