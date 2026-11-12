//! Instance sync: make `<pack>/run/<profile>` match the pack + lock.
//!
//! Sync is two-phase: [`sync`] with `dry_run = true` computes exactly what would change
//! (used by hot reload to decide restart vs live reload *before* touching files that a
//! running game may hold open); `dry_run = false` applies it.

pub mod configpatch;
pub mod options;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent;
use crate::error::{IoContext, Result};
use crate::http::DownloadItem;
use crate::meta::loader::LoaderKind;
use crate::pack::lock::{LockEntry, LockFile};
use crate::pack::schema::{ConfigReload, ModKind, Source};
use crate::pack::Pack;
use crate::paths::instance_dir;
use crate::resolve::ManualDownload;
use crate::util;
use crate::Ctx;

const STATE_FILE: &str = ".mcpack/state.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InstanceState {
    /// Managed content files: relative path → sha1.
    files: BTreeMap<String, String>,
    /// Override files: relative path → sha1 of the source.
    overrides: BTreeMap<String, String>,
    launch_fp: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LiveAction {
    ReloadResources,
    ReloadDatapacks,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncReport {
    pub profile: String,
    pub instance_dir: String,
    pub dry_run: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: usize,
    pub overrides_changed: Vec<String>,
    pub config_changed: Vec<String>,
    pub options_changed: bool,
    pub launch_changed: bool,
    pub agent: bool,
    pub missing_manual: Vec<ManualDownload>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    /// The running client must restart for these changes to take effect.
    pub restart_required: bool,
    /// Changes that can be applied to a running client without restarting.
    pub live_actions: Vec<LiveAction>,
    /// Resource packs (`file/<name>`) newly added that should be enabled live.
    pub enable_resourcepacks: Vec<String>,
    /// Human-readable reasons for `restart_required` (empty when no restart is needed).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restart_reasons: Vec<String>,
    /// Mod jars that changed: what hot-swap compares.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mod_changes: Vec<ModFileChange>,
    /// Mod changes left for the next launch (`hold_mods`), e.g. after a hot-swap.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModFileChange {
    /// `mods/<filename>`
    pub rel: String,
    pub label: String,
    /// added | updated | removed
    pub status: String,
    /// The file the instance should have (absent for `removed`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
}

impl SyncReport {
    pub fn has_changes(&self) -> bool {
        !(self.added.is_empty()
            && self.removed.is_empty()
            && self.updated.is_empty()
            && self.overrides_changed.is_empty()
            && self.config_changed.is_empty()
            && !self.options_changed
            && !self.launch_changed)
    }
}

/// Fingerprint of everything that requires a relaunch when changed.
pub fn launch_fingerprint(pack: &Pack, lock: &LockFile, profile: &str, agent: bool) -> Result<String> {
    let eff = pack.effective(profile)?;
    Ok(util::fingerprint(&(
        &eff.minecraft,
        lock.loader,
        &lock.loader_version,
        &eff.jvm,
        agent,
        &eff.username,
    )))
}

/// Should the agent be injected for this profile?
pub fn agent_enabled(pack: &Pack, profile: &str, override_: Option<bool>) -> Result<bool> {
    let eff = pack.effective(profile)?;
    Ok(override_.unwrap_or(eff.agent) && pack.file.pack.loader.kind != LoaderKind::Vanilla)
}

struct Desired {
    rel: String,
    src: PathBuf,
    sha1: String,
    kind: Option<ModKind>,
    label: String,
    /// A file of the user's (`source = "file"`): copied, never hard-linked, so rebuilding it
    /// does not change (or get blocked by) the copy a running game has open.
    local: bool,
}

pub async fn sync(
    ctx: &Ctx,
    pack: &Pack,
    lock: &LockFile,
    profile: &str,
    agent_override: Option<bool>,
    dry_run: bool,
) -> Result<SyncReport> {
    sync_opts(ctx, pack, lock, profile, agent_override, dry_run, false).await
}

/// [`sync`], optionally leaving mod jars untouched (`hold_mods`): a running game keeps its mod
/// jars open (on Windows they cannot be replaced), so after a hot-swap the files are left as
/// they are and recorded as still pending; the next sync (at launch) applies them.
pub async fn sync_opts(
    ctx: &Ctx,
    pack: &Pack,
    lock: &LockFile,
    profile: &str,
    agent_override: Option<bool>,
    dry_run: bool,
    hold_mods: bool,
) -> Result<SyncReport> {
    let eff = pack.effective(profile)?;
    let inst = instance_dir(&pack.dir, profile);
    let state_path = inst.join(STATE_FILE);
    let state: InstanceState = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut report = SyncReport {
        profile: profile.to_string(),
        instance_dir: inst.display().to_string(),
        dry_run,
        ..Default::default()
    };
    if !dry_run {
        for d in ["mods", "config", "resourcepacks", "shaderpacks", "saves"] {
            util::ensure_dir(&inst.join(d))?;
        }
    }

    // ---- 1. Content files ----
    let entries = lock.closure(&eff.top_fingerprints());
    let mut downloads = Vec::new();
    for e in &entries {
        if let (Some(url), Some(sha)) = (&e.url, &e.sha1) {
            let dest = ctx.paths.stored_file(sha, &e.filename);
            if !dest.exists() {
                downloads.push(DownloadItem { url: url.clone(), dest, sha1: Some(sha.clone()), size: e.size });
            }
        }
    }
    if !downloads.is_empty() {
        tracing::info!(count = downloads.len(), "downloading pack content");
        ctx.http.download_all(downloads).await?;
    }

    let worlds = list_worlds(&inst);
    let mut desired: Vec<Desired> = Vec::new();
    for e in &entries {
        match locate_entry(ctx, pack, e, &mut report)? {
            Some((src, sha1)) => {
                let rel = format!("{}/{}", e.kind.dir(), e.filename);
                let local = e.source == Source::File;
                desired.push(Desired { rel, src: src.clone(), sha1: sha1.clone(), kind: Some(e.kind), label: label(e), local });
                if e.kind == ModKind::Datapack {
                    for w in &worlds {
                        desired.push(Desired {
                            rel: format!("saves/{w}/datapacks/{}", e.filename),
                            src: src.clone(),
                            sha1: sha1.clone(),
                            kind: Some(ModKind::Datapack),
                            label: format!("{} (world {w})", label(e)),
                            local,
                        });
                    }
                }
            }
            None => continue,
        }
    }

    // ---- 2. Agent ----
    let agent_on = agent_enabled(pack, profile, agent_override)?;
    report.agent = false;
    if agent_on {
        match agent::jar_for(&ctx.paths.agent_dir(), &eff.minecraft)? {
            Some(jar) => {
                let sha1 = util::sha1_file(&jar)?;
                desired.push(Desired {
                    rel: format!("mods/{}", agent::AGENT_FILENAME),
                    src: jar,
                    sha1,
                    kind: Some(ModKind::Mod),
                    label: "mcpack-agent".into(),
                    local: false,
                });
                report.agent = true;
            }
            None => report.warnings.push(format!(
                "no mcpack agent for Minecraft {} (available: {}); in-game automation disabled",
                eff.minecraft,
                agent::supported_versions().join(", ")
            )),
        }
    }

    // ---- 3. Apply content ----
    let desired_rels: BTreeSet<&str> = desired.iter().map(|d| d.rel.as_str()).collect();
    let mut new_files: BTreeMap<String, String> = BTreeMap::new();
    let mut kinds_changed: BTreeSet<ModKind> = BTreeSet::new();
    for d in &desired {
        let target = inst.join(&d.rel);
        let same = state.files.get(&d.rel) == Some(&d.sha1) && target.exists();
        if same {
            report.unchanged += 1;
            new_files.insert(d.rel.clone(), d.sha1.clone());
            continue;
        }
        let existed = state.files.contains_key(&d.rel) || target.exists();
        if d.kind == Some(ModKind::Mod) {
            report.mod_changes.push(ModFileChange {
                rel: d.rel.clone(),
                label: d.label.clone(),
                status: if existed { "updated" } else { "added" }.into(),
                src: Some(d.src.display().to_string()),
                sha1: Some(d.sha1.clone()),
            });
            if hold_mods {
                // Keep the old record so the change is still pending next time.
                if let Some(old) = state.files.get(&d.rel) {
                    new_files.insert(d.rel.clone(), old.clone());
                }
                report.held.push(d.label.clone());
                kinds_changed.insert(ModKind::Mod);
                continue;
            }
        }
        if !dry_run {
            let placed = if d.local { util::replace_with_copy(&d.src, &target) } else { util::link_or_copy(&d.src, &target) };
            if let Err(e) = placed {
                report.errors.push(format!("{}: {e}", d.rel));
                continue;
            }
        }
        new_files.insert(d.rel.clone(), d.sha1.clone());
        if let Some(k) = d.kind {
            kinds_changed.insert(k);
        }
        if existed {
            report.updated.push(d.label.clone());
        } else {
            report.added.push(d.label.clone());
        }
        if d.kind == Some(ModKind::Resourcepack) && !existed {
            if let Some(name) = Path::new(&d.rel).file_name() {
                report.enable_resourcepacks.push(format!("file/{}", name.to_string_lossy()));
            }
        }
    }
    for (rel, _) in state.files.iter().filter(|(r, _)| !desired_rels.contains(r.as_str())) {
        let target = inst.join(rel);
        if let Some(k) = kind_of_rel(rel) {
            kinds_changed.insert(k);
        }
        if kind_of_rel(rel) == Some(ModKind::Mod) {
            report.mod_changes.push(ModFileChange { rel: rel.clone(), label: rel.clone(), status: "removed".into(), src: None, sha1: None });
            if hold_mods {
                new_files.insert(rel.clone(), state.files[rel].clone());
                report.held.push(rel.clone());
                continue;
            }
        }
        report.removed.push(rel.clone());
        if !dry_run && target.exists() {
            if let Err(e) = std::fs::remove_file(&target) {
                report.errors.push(format!("remove {rel}: {e}"));
            }
        }
    }

    // ---- 4. Overrides ----
    let mut override_srcs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut roots = vec![pack.dir.join("overrides")];
    for name in pack.profile_chain_names(profile)? {
        roots.push(pack.dir.join("profiles").join(&name).join("overrides"));
    }
    for root in roots {
        for rel in util::walk_files(&root)? {
            override_srcs.insert(rel.clone(), root.join(&rel));
        }
    }
    let mut new_overrides = BTreeMap::new();
    for (rel, src) in &override_srcs {
        let sha = util::sha1_file(src)?;
        let target = inst.join(rel);
        if state.overrides.get(rel) != Some(&sha) || !target.exists() {
            report.overrides_changed.push(rel.clone());
            if !dry_run {
                if let Err(e) = util::ensure_parent(&target).and_then(|_| {
                    std::fs::copy(src, &target).map(|_| ()).ctx(&target)
                }) {
                    report.errors.push(format!("override {rel}: {e}"));
                }
            }
        }
        new_overrides.insert(rel.clone(), sha);
    }
    for rel in state.overrides.keys().filter(|r| !override_srcs.contains_key(*r)) {
        report.overrides_changed.push(rel.clone());
        if !dry_run {
            let _ = std::fs::remove_file(inst.join(rel));
        }
    }

    // ---- 5. Config patches ----
    for (file, table) in &eff.config {
        let path = inst.join(file);
        match configpatch::patch(&path, table, dry_run) {
            Ok(true) => report.config_changed.push(file.clone()),
            Ok(false) => {}
            Err(e) => report.errors.push(format!("config {file}: {e}")),
        }
    }

    // ---- 6. options.txt ----
    let managed_packs: Vec<String> = new_files
        .keys()
        .filter(|r| r.starts_with("resourcepacks/"))
        .filter_map(|r| r.strip_prefix("resourcepacks/").map(|n| format!("file/{n}")))
        .collect();
    let removed_packs: Vec<String> = state
        .files
        .keys()
        .filter(|r| r.starts_with("resourcepacks/") && !new_files.contains_key(*r))
        .filter_map(|r| r.strip_prefix("resourcepacks/").map(|n| format!("file/{n}")))
        .collect();
    let automation_defaults = !pack.file.dev.no_automation_defaults;
    match options::apply(&inst.join("options.txt"), &eff.options, automation_defaults, &managed_packs, &removed_packs, dry_run) {
        Ok(changed) => report.options_changed = changed,
        Err(e) => report.errors.push(format!("options.txt: {e}")),
    }

    // ---- 7. Launch fingerprint ----
    let launch_fp = launch_fingerprint(pack, lock, profile, report.agent)?;
    report.launch_changed = !state.launch_fp.is_empty() && state.launch_fp != launch_fp;

    // ---- 8. Classify ----
    let reload = pack.file.dev.config_reload;
    let overrides_need_restart = report
        .overrides_changed
        .iter()
        .any(|r| !(r.starts_with("resourcepacks/") || r.contains("/datapacks/") || r.starts_with("datapacks/") || (r.starts_with("config/") && reload != ConfigReload::Restart)));
    let config_changed_any = !report.config_changed.is_empty() || report.overrides_changed.iter().any(|r| r.starts_with("config/"));
    let mods_changed = kinds_changed.contains(&ModKind::Mod) && !hold_mods;
    report.restart_required =
        mods_changed || report.launch_changed || report.options_changed || overrides_need_restart || (config_changed_any && reload == ConfigReload::Restart);
    if mods_changed {
        let names = |status: &str| report.mod_changes.iter().filter(|c| c.status == status).map(|c| c.label.clone()).collect::<Vec<_>>();
        for (status, verb) in [("added", "added"), ("updated", "changed"), ("removed", "removed")] {
            let n = names(status);
            if !n.is_empty() {
                report.restart_reasons.push(format!("mods {verb}: {}", list(&n)));
            }
        }
    }
    if report.launch_changed {
        report.restart_reasons.push("launch settings changed (Minecraft/loader version, JVM settings or username)".into());
    }
    if report.options_changed {
        report.restart_reasons.push("options.txt settings changed".into());
    }
    if overrides_need_restart {
        let n: Vec<String> = report
            .overrides_changed
            .iter()
            .filter(|r| !(r.starts_with("resourcepacks/") || r.contains("datapacks/") || (r.starts_with("config/") && reload != ConfigReload::Restart)))
            .cloned()
            .collect();
        report.restart_reasons.push(format!("override files changed: {}", list(&n)));
    }
    if config_changed_any && reload == ConfigReload::Restart {
        let mut n = report.config_changed.clone();
        n.extend(report.overrides_changed.iter().filter(|r| r.starts_with("config/")).cloned());
        report.restart_reasons.push(format!("configs changed: {} (most mods read configs at startup; set dev.config_reload = \"resources\" for mods that reload them)", list(&n)));
    }
    if kinds_changed.contains(&ModKind::Resourcepack)
        || kinds_changed.contains(&ModKind::Shaderpack)
        || report.overrides_changed.iter().any(|r| r.starts_with("resourcepacks/"))
        || (config_changed_any && reload == ConfigReload::Resources)
    {
        report.live_actions.push(LiveAction::ReloadResources);
    }
    if kinds_changed.contains(&ModKind::Datapack) || report.overrides_changed.iter().any(|r| r.contains("datapacks/")) {
        report.live_actions.push(LiveAction::ReloadDatapacks);
    }

    if !dry_run {
        let new_state = InstanceState { files: new_files, overrides: new_overrides, launch_fp };
        util::write_atomic(&state_path, serde_json::to_string_pretty(&new_state)?.as_bytes())?;
    }
    Ok(report)
}

/// "a, b, c" or "a, b, c and 4 more".
fn list(items: &[String]) -> String {
    const SHOW: usize = 4;
    if items.len() <= SHOW {
        items.join(", ")
    } else {
        format!("{} and {} more", items[..SHOW].join(", "), items.len() - SHOW)
    }
}

fn label(e: &LockEntry) -> String {
    if e.version_number.is_empty() {
        e.name.clone()
    } else {
        format!("{} {}", e.name, e.version_number)
    }
}

fn kind_of_rel(rel: &str) -> Option<ModKind> {
    let first = rel.split('/').next()?;
    match first {
        "mods" => Some(ModKind::Mod),
        "resourcepacks" => Some(ModKind::Resourcepack),
        "shaderpacks" => Some(ModKind::Shaderpack),
        "datapacks" | "saves" => Some(ModKind::Datapack),
        _ => None,
    }
}

pub fn list_worlds(inst: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(inst.join("saves")) {
        for e in rd.flatten() {
            if e.path().join("level.dat").exists() {
                out.push(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
}

/// Find (and stage into the file store) the file for a lock entry.
fn locate_entry(ctx: &Ctx, pack: &Pack, e: &LockEntry, report: &mut SyncReport) -> Result<Option<(PathBuf, String)>> {
    match e.source {
        Source::File => {
            let rel = e.path.clone().unwrap_or_default();
            let abs = util::resolve_rel(&pack.dir, &rel);
            if !abs.exists() {
                report.warnings.push(format!("local file {} not found; skipped", abs.display()));
                return Ok(None);
            }
            let sha = util::sha1_file(&abs)?;
            Ok(Some((abs, sha)))
        }
        _ => {
            if let Some(sha) = &e.sha1 {
                let stored = ctx.paths.stored_file(sha, &e.filename);
                if stored.exists() {
                    return Ok(Some((stored, sha.clone())));
                }
            }
            if e.manual {
                // Look for a manually downloaded copy.
                let mut candidates = vec![pack.dir.join("manual").join(&e.filename)];
                if let Some(dl) = dirs::download_dir() {
                    candidates.push(dl.join(&e.filename));
                }
                for c in candidates {
                    if c.exists() {
                        let sha = util::sha1_file(&c)?;
                        if e.sha1.as_deref().is_some_and(|s| !s.eq_ignore_ascii_case(&sha)) {
                            report.warnings.push(format!("{} has the wrong hash; ignoring", c.display()));
                            continue;
                        }
                        let stored = ctx.paths.stored_file(&sha, &e.filename);
                        util::ensure_parent(&stored)?;
                        std::fs::copy(&c, &stored).ctx(&stored)?;
                        return Ok(Some((stored, sha)));
                    }
                }
                report.missing_manual.push(ManualDownload {
                    name: e.name.clone(),
                    filename: e.filename.clone(),
                    page_url: e.page_url.clone(),
                    sha1: e.sha1.clone(),
                });
                return Ok(None);
            }
            report.errors.push(format!("{}: file not available in store", e.name));
            Ok(None)
        }
    }
}
