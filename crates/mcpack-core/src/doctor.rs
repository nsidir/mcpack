//! `doctor`: detect pack problems (resolution, jar metadata, and optionally a real launch)
//! and fix what can be fixed automatically, iterating until clean.
//!
//! Automatic fixes:
//! - missing dependency (declared in a jar but not on Modrinth/CurseForge) → add the dependency
//! - mod built for another Minecraft version / dependency too old → re-resolve to latest compatible
//! - missing class owned by a known library → add that library
//! - out of memory → raise `jvm.memory`
//! - mixin failure / crash-suspected mod → update that mod once
//!
//! Not auto-fixed (reported with suggestions): incompatible mod pairs, duplicates of
//! unmanaged jars, manual CurseForge downloads, graphics driver issues.

use std::collections::BTreeSet;
use std::path::Path;
use serde::{Deserialize, Serialize};

use crate::check::CheckReport;
use crate::diagnose::Problem;
use crate::error::Result;
use crate::pack::lock::LockFile;
use crate::pack::schema::{ModSpec, Source};
use crate::pack::Pack;
use crate::resolve::UpdatePolicy;
use crate::source::modrinth::Modrinth;
use crate::source::SearchFilters;
use crate::testing::{SmokeOptions, SmokeResult};
use crate::Launcher;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoctorOptions {
    #[serde(default)]
    pub profile: Option<String>,
    /// Also launch the client (smoke test) to catch runtime errors (default false).
    #[serde(default)]
    pub smoke: bool,
    /// Apply fixes (default true). With false, only reports planned fixes.
    #[serde(default)]
    pub apply: Option<bool>,
    /// Maximum fix rounds (default 5).
    #[serde(default)]
    pub max_rounds: Option<u32>,
    /// Smoke test timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorAction {
    pub round: u32,
    /// add_dependency | update_mod | update_all | raise_memory
    pub kind: String,
    pub target: String,
    pub reason: String,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub rounds: u32,
    pub actions: Vec<DoctorAction>,
    /// Problems still present (errors and warnings) after fixing.
    pub remaining: Vec<Problem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smoke: Option<SmokeResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Fix {
    AddDependency { mod_id: String },
    UpdateMod { ident: String },
    UpdateAll,
    RaiseMemory,
}

/// Known Fabric mod ids → Modrinth slugs.
fn known_slug(mod_id: &str) -> Option<&'static str> {
    let id = mod_id.to_ascii_lowercase();
    if id == "fabric" || id == "fabric-api" || id.starts_with("fabric-") && (id.ends_with("-v0") || id.ends_with("-v1") || id.ends_with("-v2") || id.ends_with("-v3") || id.ends_with("-base") || id.contains("-api")) {
        return Some("fabric-api");
    }
    const MAP: &[(&str, &str)] = &[
        ("cloth-config", "cloth-config"),
        ("cloth-config2", "cloth-config"),
        ("cloth_config", "cloth-config"),
        ("architectury", "architectury-api"),
        ("modmenu", "modmenu"),
        ("yet_another_config_lib_v3", "yacl"),
        ("yet-another-config-lib", "yacl"),
        ("yacl", "yacl"),
        ("geckolib", "geckolib"),
        ("owo", "owo-lib"),
        ("owo-lib", "owo-lib"),
        ("fabric-language-kotlin", "fabric-language-kotlin"),
        ("fabric_language_kotlin", "fabric-language-kotlin"),
        ("trinkets", "trinkets"),
        ("cardinal-components", "cardinal-components-api"),
        ("cardinal-components-base", "cardinal-components-api"),
        ("cardinal-components-entity", "cardinal-components-api"),
        ("puzzleslib", "puzzles-lib"),
        ("forgeconfigapiport", "forge-config-api-port"),
        ("midnightlib", "midnightlib"),
        ("balm-fabric", "balm"),
        ("balm", "balm"),
        ("resourcefullib", "resourceful-lib"),
        ("resourcefulconfig", "resourceful-config"),
        ("kotlinforforge", "kotlin-for-forge"),
        ("bookshelf", "bookshelf-lib"),
        ("collective", "collective"),
        ("lithostitched", "lithostitched"),
        ("terrablender", "terrablender"),
        ("indium", "indium"),
        ("sodium", "sodium"),
        ("iris", "iris"),
        ("qsl", "qsl"),
        ("quilted_fabric_api", "qsl"),
        ("placeholder-api", "placeholder-api"),
        ("libz", "libz"),
        ("azurelib", "azurelib"),
        ("patchouli", "patchouli"),
        ("playeranimator", "playeranimator"),
        ("player-animator", "playeranimator"),
        ("creativecore", "creativecore"),
        ("cristellib", "cristel-lib"),
        ("supermartijn642corelib", "supermartijn642s-core-lib"),
        ("supermartijn642configlib", "supermartijn642s-config-lib"),
    ];
    MAP.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
}

fn normalize(s: &str) -> String {
    s.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

impl Launcher {
    /// Find a Modrinth project id/slug for a Fabric mod id.
    pub async fn find_project_for_mod_id(&self, mod_id: &str, minecraft: &str, loader: crate::LoaderKind) -> Option<String> {
        if let Some(s) = known_slug(mod_id) {
            return Some(s.to_string());
        }
        let mr = Modrinth::new(self.ctx.http.clone());
        for cand in [mod_id.to_string(), mod_id.replace('_', "-")] {
            if let Ok(p) = mr.project(&cand).await {
                if p.project_type == "mod" {
                    return Some(p.slug.unwrap_or(p.id));
                }
            }
        }
        let hits = mr
            .search(
                &mod_id.replace(['_', '-'], " "),
                &SearchFilters { minecraft: Some(minecraft.into()), loader: Some(loader), kind: None, limit: 5 },
            )
            .await
            .ok()?;
        let want = normalize(mod_id);
        hits.iter()
            .find(|h| h.slug.as_deref().map(normalize).as_deref() == Some(want.as_str()) || normalize(&h.title) == want)
            .map(|h| h.slug.clone().unwrap_or_else(|| h.id.clone()))
    }

    /// Detect and auto-fix pack problems. See module docs for what is fixed.
    pub async fn doctor(&self, dir: &Path, opts: DoctorOptions) -> Result<DoctorReport> {
        let dir = std::path::absolute(crate::pack::pack_dir(dir)).unwrap_or_else(|_| dir.to_path_buf());
        let profile = opts.profile.clone().unwrap_or_else(|| crate::pack::DEFAULT_PROFILE.to_string());
        let apply = opts.apply.unwrap_or(true);
        let max_rounds = opts.max_rounds.unwrap_or(5).max(1);
        let mut actions: Vec<DoctorAction> = Vec::new();
        let mut tried: BTreeSet<Fix> = BTreeSet::new();
        let mut smoke: Option<SmokeResult> = None;
        let mut remaining: Vec<Problem>;
        let mut round = 0;

        loop {
            round += 1;
            // Resolution errors surface here as Err; report them as problems.
            let check = match self.check(&dir, &profile).await {
                Ok(c) => c,
                Err(e) => CheckReport {
                    ok: false,
                    problems: vec![Problem::error("resolve_failed", e.to_string())
                        .suggest("fix or remove the entry named in the message (mod_versions lists versions for this Minecraft/loader)")],
                    ..Default::default()
                },
            };
            let mut problems: Vec<Problem> = check.problems.clone();
            if opts.smoke && check.ok {
                let r = self
                    .smoke_test(
                        &dir,
                        SmokeOptions { profile: Some(profile.clone()), timeout_secs: opts.timeout_secs, ..Default::default() },
                    )
                    .await?;
                if !r.passed {
                    if let Some(d) = &r.diagnosis {
                        problems.extend(d.problems.iter().cloned());
                    } else {
                        problems.push(Problem::error("smoke_failed", r.error.clone().unwrap_or_else(|| r.outcome.clone())));
                    }
                }
                smoke = Some(r);
            }
            remaining = problems.clone();
            let errors: Vec<&Problem> = problems.iter().filter(|p| p.severity == "error").collect();
            if errors.is_empty() || round > max_rounds {
                break;
            }

            let pack = Pack::load(&dir)?;
            let lock = LockFile::load(&dir)?.unwrap_or_default();
            let mut fixes: Vec<(Fix, String)> = Vec::new();
            for p in &errors {
                if let Some(f) = self.plan_fix(p, &pack, &lock, &check).await {
                    if !tried.contains(&f) && !fixes.iter().any(|(x, _)| *x == f) {
                        fixes.push((f, p.message.clone()));
                    }
                }
            }
            if fixes.is_empty() {
                break;
            }
            for (fix, reason) in fixes {
                tried.insert(fix.clone());
                let (kind, target) = match &fix {
                    Fix::AddDependency { mod_id } => ("add_dependency", mod_id.clone()),
                    Fix::UpdateMod { ident } => ("update_mod", ident.clone()),
                    Fix::UpdateAll => ("update_all", "*".to_string()),
                    Fix::RaiseMemory => ("raise_memory", "jvm.memory".to_string()),
                };
                let mut action = DoctorAction { round, kind: kind.into(), target: target.clone(), reason, applied: false, error: None };
                if apply {
                    let res = self.apply_fix(&dir, &pack, &fix).await;
                    match res {
                        Ok(t) => {
                            action.applied = true;
                            action.target = t;
                        }
                        Err(e) => action.error = Some(e.to_string()),
                    }
                }
                actions.push(action);
            }
            if !apply {
                break;
            }
        }
        let ok = !remaining.iter().any(|p| p.severity == "error");
        Ok(DoctorReport { ok, rounds: round, actions, remaining, smoke })
    }

    async fn plan_fix(&self, p: &Problem, pack: &Pack, lock: &LockFile, check: &CheckReport) -> Option<Fix> {
        let mc = &pack.file.pack.minecraft;
        let loader = pack.file.pack.loader.kind;
        // Map a jar mod id to the pack entry identifier that provides it.
        let ident_for_mod_id = |mod_id: &str| -> Option<String> {
            let file = check.mods.iter().find(|m| m.id == mod_id).map(|m| m.file.split('!').next().unwrap_or(&m.file).to_string())?;
            let e = lock.entries.iter().find(|e| e.filename == file)?;
            Some(e.slug.clone().unwrap_or_else(|| e.project_id.clone()))
        };
        let is_top = |ident: &str| pack.all_specs().iter().any(|s| s.matches(ident));
        match p.kind.as_str() {
            "missing_dependency" | "missing_class" => {
                let dep = if p.kind == "missing_class" { p.mods.first()? } else { p.mods.get(1)? };
                let slug = self.find_project_for_mod_id(dep, mc, loader).await?;
                Some(Fix::AddDependency { mod_id: slug })
            }
            "wrong_dependency_version" | "wrong_java" => {
                let modid = p.mods.first()?;
                let dep = p.mods.get(1).map(String::as_str).unwrap_or("");
                // Mod built for another MC/Java: update the mod itself. Otherwise update the dependency.
                let target = if matches!(dep, "minecraft" | "java" | "fabricloader" | "") { modid.as_str() } else { dep };
                match ident_for_mod_id(target) {
                    Some(ident) if is_top(&ident) => Some(Fix::UpdateMod { ident }),
                    Some(_) => Some(Fix::UpdateAll),
                    None => None,
                }
            }
            "out_of_memory" => Some(Fix::RaiseMemory),
            "mixin_failure" | "suspected_mods" => {
                let modid = p.mods.first()?;
                let ident = ident_for_mod_id(modid).or_else(|| lock.entries.iter().find(|e| e.matches_ident(modid)).map(|e| e.slug.clone().unwrap_or(e.project_id.clone())))?;
                if is_top(&ident) {
                    Some(Fix::UpdateMod { ident })
                } else {
                    Some(Fix::UpdateAll)
                }
            }
            _ => None,
        }
    }

    async fn apply_fix(&self, dir: &Path, pack: &Pack, fix: &Fix) -> Result<String> {
        match fix {
            Fix::AddDependency { mod_id } => {
                let spec = ModSpec { source: Source::Modrinth, id: Some(mod_id.clone()), ..Default::default() };
                self.pack_add(dir, None, spec, true).await?;
                Ok(mod_id.clone())
            }
            Fix::UpdateMod { ident } => {
                // Unpin an explicit version so the newest compatible one is chosen.
                if let Some(spec) = pack.all_specs().into_iter().find(|s| s.matches(ident)) {
                    if spec.version.is_some() && spec.source != Source::File && spec.source != Source::Url {
                        let mut ed = crate::pack::edit::PackEditor::open(dir)?;
                        let mut s = spec.clone();
                        s.version = None;
                        ed.add_mod(None, &s)?;
                        ed.save()?;
                    }
                }
                self.resolve(dir, UpdatePolicy { all: false, only: vec![ident.clone()] }).await?;
                Ok(ident.clone())
            }
            Fix::UpdateAll => {
                self.resolve(dir, UpdatePolicy { all: true, only: vec![] }).await?;
                Ok("*".into())
            }
            Fix::RaiseMemory => {
                let cur = pack.file.jvm.memory.clone().unwrap_or_else(|| "2G".into());
                let gb = parse_gb(&cur).unwrap_or(2.0);
                let next = ((gb + 2.0).min(12.0)).ceil() as u32;
                self.pack_set(dir, "jvm.memory", &serde_json::json!(format!("{next}G"))).await?;
                Ok(format!("jvm.memory = {next}G"))
            }
        }
    }
}

fn parse_gb(m: &str) -> Option<f64> {
    let t = m.trim().to_ascii_uppercase();
    let t = t.trim_end_matches('B');
    if let Some(g) = t.strip_suffix('G') {
        g.parse().ok()
    } else if let Some(mb) = t.strip_suffix('M') {
        mb.parse::<f64>().ok().map(|v| v / 1024.0)
    } else {
        t.parse::<f64>().ok().map(|v| v / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(known_slug("fabric-api-base"), Some("fabric-api"));
        assert_eq!(known_slug("fabric-rendering-v1"), Some("fabric-api"));
        assert_eq!(known_slug("cloth-config2"), Some("cloth-config"));
        assert_eq!(known_slug("sodium"), Some("sodium"));
        assert_eq!(known_slug("something-else"), None);
        assert_eq!(parse_gb("4G"), Some(4.0));
        assert_eq!(parse_gb("2048M"), Some(2.0));
    }
}
