//! `options.txt` management (key:value lines).

use std::collections::BTreeMap;
use std::path::Path;

use super::configpatch::scalar_string;
use crate::error::{IoContext, Result};
use crate::util;

/// Defaults that make automated launches smooth (only applied when the pack doesn't set them).
pub const AUTOMATION_DEFAULTS: &[(&str, &str)] = &[
    ("onboardAccessibility", "false"),
    ("skipMultiplayerWarning", "true"),
    ("tutorialStep", "none"),
    ("pauseOnLostFocus", "false"),
    ("joinedFirstServer", "true"),
];

pub fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

pub fn read(path: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path).map(|t| parse(&t)).unwrap_or_default()
}

/// Apply pack options; returns whether the file changed.
pub fn apply(
    path: &Path,
    options: &BTreeMap<String, toml::Value>,
    automation_defaults: bool,
    managed_packs: &[String],
    removed_packs: &[String],
    dry_run: bool,
) -> Result<bool> {
    let original = if path.exists() { std::fs::read_to_string(path).ctx(path)? } else { String::new() };
    let mut lines = parse(&original);
    let set = |lines: &mut Vec<(String, String)>, k: &str, v: String| {
        if let Some(l) = lines.iter_mut().find(|(lk, _)| lk == k) {
            l.1 = v;
        } else {
            lines.push((k.to_string(), v));
        }
    };
    if automation_defaults {
        for (k, v) in AUTOMATION_DEFAULTS {
            if !options.contains_key(*k) && !lines.iter().any(|(lk, _)| lk == k) {
                set(&mut lines, k, v.to_string());
            }
        }
    }
    for (k, v) in options {
        set(&mut lines, k, scalar_string(v));
    }
    // Auto-enable managed resource packs unless the pack sets resourcePacks explicitly.
    if !options.contains_key("resourcePacks") && (!managed_packs.is_empty() || !removed_packs.is_empty()) {
        let current = lines.iter().find(|(k, _)| k == "resourcePacks").map(|(_, v)| v.clone());
        let mut list: Vec<String> = current
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_else(|| vec!["vanilla".to_string()]);
        list.retain(|p| !removed_packs.contains(p));
        for p in managed_packs {
            if !list.contains(p) {
                list.push(p.clone());
            }
        }
        set(&mut lines, "resourcePacks", serde_json::to_string(&list).unwrap_or_default());
    }
    let mut text: String = lines.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    if text.is_empty() {
        text = String::new();
    }
    if text == original {
        return Ok(false);
    }
    if !dry_run {
        util::write_atomic(path, text.as_bytes())?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_options_and_packs() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("options.txt");
        std::fs::write(&f, "version:3955\nrenderDistance:12\nresourcePacks:[\"vanilla\",\"file/old.zip\"]\n").unwrap();
        let mut opts = BTreeMap::new();
        opts.insert("renderDistance".to_string(), toml::Value::Integer(4));
        let changed = apply(&f, &opts, true, &["file/new.zip".into()], &["file/old.zip".into()], false).unwrap();
        assert!(changed);
        let lines = read(&f);
        let get = |k: &str| lines.iter().find(|(lk, _)| lk == k).map(|(_, v)| v.clone());
        assert_eq!(get("renderDistance").as_deref(), Some("4"));
        assert_eq!(get("version").as_deref(), Some("3955"));
        assert_eq!(get("onboardAccessibility").as_deref(), Some("false"));
        assert_eq!(get("resourcePacks").as_deref(), Some("[\"vanilla\",\"file/new.zip\"]"));
        assert!(!apply(&f, &opts, true, &["file/new.zip".into()], &[], false).unwrap());
    }
}
