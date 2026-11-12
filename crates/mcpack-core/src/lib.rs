//! # mcpack-core
//!
//! Automation-first Minecraft launcher library. Every operation is a plain async call on
//! [`Launcher`] returning serializable data, so it can be driven by scripts, a UI, or an AI
//! agent (see the `mcpack` MCP server).
//!
//! Model:
//! - A **pack** is a directory with `pack.toml` (declarative source of truth) and `pack.lock`
//!   (resolved, pinned content). Profiles in `pack.toml` overlay settings/mods.
//! - An **instance** is `<pack>/run/<profile>`, kept in sync with the pack by [`Launcher::sync`].
//! - A **session** is a running game process with captured logs and (for Fabric/Quilt) an
//!   in-game automation agent for commands, screenshots, world control and live reloads.

pub mod agent;
pub mod cfpack;
pub mod check;
pub mod config;
pub mod diagnose;
pub mod doctor;
pub mod events;
pub mod guide;
pub mod hotswap;
pub mod registry;
pub mod tools;
pub mod error;
pub mod http;
pub mod install;
pub mod instance;
pub mod launch;
pub mod meta;
pub mod mrpack;
pub mod pack;
pub mod paths;
pub mod resolve;
pub mod source;
pub mod testing;
pub mod util;
pub mod watch;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

pub use config::GlobalConfig;
pub use error::{Error, Result};
pub use launch::{LaunchOptions, Session, SessionStatus};
pub use pack::schema::{LoaderKind, ModKind, ModSpec, Source};
pub use pack::Pack;

use crate::agent::AgentEndpoint;
use crate::check::CheckReport;
use crate::diagnose::{Diagnosis, Problem};
use crate::install::InstallSummary;
use crate::instance::SyncReport;
use crate::pack::edit::{self, PackEditor};
use crate::pack::lock::LockFile;
use crate::paths::Paths;
use crate::resolve::{ResolveReport, UpdatePolicy};
use crate::source::curseforge::CurseForge;
use crate::source::modrinth::Modrinth;
use crate::source::{SearchFilters, SearchHit, VersionSummary};

/// Shared low-level context.
pub struct Ctx {
    pub paths: Paths,
    pub config: std::sync::RwLock<GlobalConfig>,
    pub http: http::Http,
    pub platform: meta::Platform,
    pub events: events::EventBus,
}

impl Ctx {
    /// Snapshot of the current global config.
    pub fn cfg(&self) -> GlobalConfig {
        self.config.read().expect("config lock").clone()
    }
}

/// The launcher. Cheap to clone; all state is shared.
#[derive(Clone)]
pub struct Launcher {
    pub(crate) ctx: Arc<Ctx>,
    pub(crate) sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    pub(crate) watches: Arc<Mutex<HashMap<String, watch::WatchHandle>>>,
    pack_locks: Arc<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LockSummaryEntry {
    pub key: String,
    /// Slug (Modrinth/CurseForge) or project id / filename.
    pub slug: String,
    pub name: String,
    pub version: String,
    pub kind: ModKind,
    /// modrinth | curseforge | local
    pub source: String,
    /// Client/server side (not tracked yet; always "both").
    pub side: String,
    /// Pulled in as a dependency rather than declared in pack.toml.
    pub dependency: bool,
    pub size: u64,
    pub filename: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    /// Requested directly in pack.toml (vs. pulled in as a dependency).
    pub top_level: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackInfo {
    pub dir: String,
    pub name: String,
    pub minecraft: String,
    pub loader: LoaderKind,
    pub loader_version_spec: String,
    pub loader_version: Option<String>,
    pub profiles: Vec<String>,
    pub pack: serde_json::Value,
    pub lock_stale: bool,
    pub content: Vec<LockSummaryEntry>,
    pub running_sessions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditResult {
    pub pack: PackInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve: Option<ResolveReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LaunchResult {
    pub session: SessionStatus,
    pub sync: SyncReport,
    pub install: InstallSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogsResult {
    pub session: String,
    pub running: bool,
    pub lines: Vec<launch::LogLine>,
    /// Pass as `since` to get only newer lines next time.
    pub next_seq: u64,
}

impl Launcher {
    /// Use `$MCPACK_HOME` (or the platform data dir) for caches and config.
    pub fn new() -> Result<Self> {
        Self::with_paths(Paths::from_env())
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Result<Self> {
        Self::with_paths(Paths::with_home(home))
    }

    fn with_paths(paths: Paths) -> Result<Self> {
        let config = GlobalConfig::load(&paths)?;
        let http = http::Http::new(config.download_concurrency)?;
        Ok(Self {
            ctx: Arc::new(Ctx { paths, config: std::sync::RwLock::new(config), http, platform: meta::Platform::current(), events: events::EventBus::default() }),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            watches: Arc::new(Mutex::new(HashMap::new())),
            pack_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    pub fn home(&self) -> &Path {
        &self.ctx.paths.home
    }

    fn norm(dir: &Path) -> PathBuf {
        let d = pack::pack_dir(dir);
        std::path::absolute(&d).unwrap_or(d)
    }

    async fn lock_pack(&self, dir: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let m = {
            let mut locks = self.pack_locks.lock().expect("pack locks");
            locks.entry(dir.to_path_buf()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
        };
        m.lock_owned().await
    }

    // ------------------------------------------------------------------ meta

    pub async fn minecraft_versions(&self, include_snapshots: bool) -> Result<Vec<String>> {
        install::list_minecraft_versions(&self.ctx, include_snapshots).await
    }

    pub async fn loader_versions(&self, loader: LoaderKind, minecraft: &str) -> Result<Vec<serde_json::Value>> {
        let v = meta::loader::loader_versions(&self.ctx.http, loader, minecraft).await?;
        Ok(v.into_iter().map(|(v, s)| serde_json::json!({"version": v, "stable": s})).collect())
    }

    pub fn agent_versions(&self) -> Vec<&'static str> {
        agent::supported_versions()
    }

    // ------------------------------------------------------------------ packs

    pub fn pack_init(&self, dir: &Path, name: &str, minecraft: &str, loader: LoaderKind, loader_version: Option<&str>) -> Result<PackInfo> {
        let dir = Self::norm(dir);
        edit::init(&dir, name, minecraft, loader, loader_version.unwrap_or("latest"))?;
        self.pack_show(&dir)
    }

    pub fn pack_show(&self, dir: &Path) -> Result<PackInfo> {
        let dir = Self::norm(dir);
        let pack = Pack::load(&dir)?;
        let lock = LockFile::load(&dir)?;
        let all_fps: Vec<String> = pack.all_specs().iter().map(|s| s.resolution_fingerprint()).collect();
        let content = lock
            .as_ref()
            .map(|l| {
                l.entries
                    .iter()
                    .map(|e| {
                        let top = e.requested_by.iter().any(|f| all_fps.contains(f));
                        LockSummaryEntry {
                            key: e.key.clone(),
                            slug: e.slug.clone().unwrap_or_else(|| match e.source {
                                Source::Url | Source::File => e.filename.clone(),
                                _ => e.project_id.clone(),
                            }),
                            name: e.name.clone(),
                            version: e.version_number.clone(),
                            kind: e.kind,
                            source: match e.source {
                                Source::Modrinth => "modrinth",
                                Source::Curseforge => "curseforge",
                                Source::Url | Source::File => "local",
                            }
                            .into(),
                            side: "both".into(),
                            dependency: !top,
                            size: e.size.unwrap_or(0),
                            filename: e.filename.clone(),
                            deps: e.deps.clone(),
                            manual: e.manual,
                            top_level: top,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let running = self
            .sessions
            .lock()
            .expect("sessions")
            .values()
            .filter(|s| s.pack_dir == dir && s.running())
            .map(|s| s.id.clone())
            .collect();
        Ok(PackInfo {
            dir: dir.display().to_string(),
            name: pack.file.pack.name.clone(),
            minecraft: pack.file.pack.minecraft.clone(),
            loader: pack.file.pack.loader.kind,
            loader_version_spec: pack.file.pack.loader.version.clone(),
            loader_version: lock.as_ref().map(|l| l.loader_version.clone()).filter(|v| !v.is_empty()),
            profiles: pack.profile_names(),
            pack: serde_json::to_value(&pack.file)?,
            lock_stale: lock.as_ref().is_none_or(|l| l.inputs != pack.resolution_inputs()),
            content,
            running_sessions: running,
        })
    }

    /// Effective (merged) settings of a profile.
    pub fn profile_show(&self, dir: &Path, profile: &str) -> Result<pack::EffectiveProfile> {
        Pack::load(Self::norm(dir))?.effective(profile)
    }

    /// Read any value from pack.toml by path (`jvm.memory`, `profiles.test`, `mods[0]`).
    pub fn pack_get(&self, dir: &Path, path: &str) -> Result<serde_json::Value> {
        let pack = Pack::load(Self::norm(dir))?;
        let mut v = serde_json::to_value(&pack.file)?;
        if path.trim().is_empty() {
            return Ok(v);
        }
        for seg in edit::parse_path(path)? {
            v = match (v, seg.parse::<usize>()) {
                (serde_json::Value::Array(mut a), Ok(i)) if i < a.len() => a.swap_remove(i),
                (serde_json::Value::Object(mut o), _) => o.remove(&seg).unwrap_or(serde_json::Value::Null),
                _ => serde_json::Value::Null,
            };
        }
        Ok(v)
    }

    /// Set a pack.toml value by path. Comments and layout are preserved; invalid results are rejected.
    pub async fn pack_set(&self, dir: &Path, path: &str, value: &serde_json::Value) -> Result<EditResult> {
        let dir = Self::norm(dir);
        let _g = self.lock_pack(&dir).await;
        let mut ed = PackEditor::open(&dir)?;
        ed.set(&edit::parse_path(path)?, value)?;
        ed.save()?;
        Ok(EditResult { pack: self.pack_show(&dir)?, resolve: None, replaced: None, removed: None })
    }

    pub async fn pack_unset(&self, dir: &Path, path: &str) -> Result<EditResult> {
        let dir = Self::norm(dir);
        let _g = self.lock_pack(&dir).await;
        let mut ed = PackEditor::open(&dir)?;
        let removed = ed.unset(&edit::parse_path(path)?)?;
        ed.save()?;
        Ok(EditResult { pack: self.pack_show(&dir)?, resolve: None, replaced: None, removed: Some(removed as usize) })
    }

    /// Add content. With `resolve`, the lock is updated immediately and the edit is rolled back
    /// if resolution fails (so the pack never ends up unresolvable).
    pub async fn pack_add(&self, dir: &Path, profile: Option<&str>, spec: ModSpec, resolve: bool) -> Result<EditResult> {
        let dir = Self::norm(dir);
        let _g = self.lock_pack(&dir).await;
        let backup = std::fs::read_to_string(dir.join(pack::PACK_FILE)).map_err(|e| Error::io(&dir, e))?;
        let mut ed = PackEditor::open(&dir)?;
        let replaced = ed.add_mod(profile, &spec)?;
        ed.save()?;
        let report = if resolve {
            match self.resolve_inner(&dir, UpdatePolicy { all: false, only: vec![spec.ident()] }).await {
                Ok(r) => Some(r),
                Err(e) => {
                    util::write_atomic(&dir.join(pack::PACK_FILE), backup.as_bytes())?;
                    return Err(e);
                }
            }
        } else {
            None
        };
        Ok(EditResult { pack: self.pack_show(&dir)?, resolve: report, replaced: Some(replaced), removed: None })
    }

    pub async fn pack_remove(&self, dir: &Path, profile: Option<&str>, ident: &str, resolve: bool) -> Result<EditResult> {
        let dir = Self::norm(dir);
        let _g = self.lock_pack(&dir).await;
        let mut ed = PackEditor::open(&dir)?;
        let removed = ed.remove_mod(profile, ident)?;
        if removed == 0 {
            return Err(Error::NotFound(format!("no mod matching '{ident}' in {}", profile.unwrap_or("base pack"))));
        }
        ed.save()?;
        let report = if resolve { Some(self.resolve_inner(&dir, UpdatePolicy::default()).await?) } else { None };
        Ok(EditResult { pack: self.pack_show(&dir)?, resolve: report, replaced: None, removed: Some(removed) })
    }

    // ------------------------------------------------------------------ resolve / sync

    pub async fn resolve(&self, dir: &Path, policy: UpdatePolicy) -> Result<ResolveReport> {
        let dir = Self::norm(dir);
        let _g = self.lock_pack(&dir).await;
        self.resolve_inner(&dir, policy).await
    }

    async fn resolve_inner(&self, dir: &Path, policy: UpdatePolicy) -> Result<ResolveReport> {
        let pack = Pack::load(dir)?;
        let old = LockFile::load(dir)?;
        let (lock, mut report) = resolve::resolve(&self.ctx, &pack, old.as_ref(), policy).await?;
        lock.save(dir)?;
        report.lock_written = true;
        Ok(report)
    }

    /// Which entries have newer compatible versions (nothing is written).
    pub async fn check_updates(&self, dir: &Path) -> Result<Vec<resolve::EntryChange>> {
        let dir = Self::norm(dir);
        let pack = Pack::load(&dir)?;
        let old = LockFile::load(&dir)?;
        let (_, report) = resolve::resolve(&self.ctx, &pack, old.as_ref(), UpdatePolicy { all: true, only: vec![] }).await?;
        Ok(report.changed)
    }

    /// Load the lock, re-resolving if pack.toml changed since it was written.
    pub async fn ensure_lock(&self, pack: &Pack) -> Result<(LockFile, Option<ResolveReport>)> {
        let old = LockFile::load(&pack.dir)?;
        if let Some(l) = &old {
            if l.inputs == pack.resolution_inputs() {
                return Ok((l.clone(), None));
            }
        }
        let (lock, mut report) = resolve::resolve(&self.ctx, pack, old.as_ref(), UpdatePolicy::default()).await?;
        lock.save(&pack.dir)?;
        report.lock_written = true;
        Ok((lock, Some(report)))
    }

    /// Make the instance for `profile` match the pack. `dry_run` reports without writing.
    pub async fn sync(&self, dir: &Path, profile: &str, agent: Option<bool>, dry_run: bool) -> Result<SyncReport> {
        let dir = Self::norm(dir);
        let _g = self.lock_pack(&dir).await;
        let pack = Pack::load(&dir)?;
        let (lock, _) = self.ensure_lock(&pack).await?;
        let mut report = instance::sync(&self.ctx, &pack, &lock, profile, agent, dry_run).await?;
        if !dry_run && self.find_session(&dir, profile).is_some() && report.restart_required {
            report.warnings.push("a session for this profile is running; restart it to apply all changes".into());
        }
        Ok(report)
    }

    /// Download the game, loader, libraries, assets and Java for a pack.
    pub async fn install(&self, dir: &Path, profile: &str) -> Result<InstallSummary> {
        let dir = Self::norm(dir);
        let pack = Pack::load(&dir)?;
        let (lock, _) = self.ensure_lock(&pack).await?;
        let eff = pack.effective(profile)?;
        let ev = self.ctx.events.clone();
        let pack_s = dir.display().to_string();
        let (_, summary) = install::install(&self.ctx, &eff.minecraft, lock.loader, &lock.loader_version, eff.jvm.java.as_deref(), &|phase, f, d| {
            ev.progress_profile(Some(&pack_s), Some(profile), None, phase, f, d)
        })
        .await?;
        Ok(summary)
    }

    /// Pre-launch validation: syncs the instance, then checks jars, deps, Java and manual downloads.
    pub async fn check(&self, dir: &Path, profile: &str) -> Result<CheckReport> {
        let dir = Self::norm(dir);
        let _g = self.lock_pack(&dir).await;
        let pack = Pack::load(&dir)?;
        let (lock, _) = self.ensure_lock(&pack).await?;
        let running = self.find_session(&dir, profile).is_some();
        let sync = instance::sync(&self.ctx, &pack, &lock, profile, Some(false), running).await?;
        let eff = pack.effective(profile)?;
        let mut problems: Vec<Problem> = Vec::new();
        for m in &sync.missing_manual {
            problems.push(
                Problem::error("manual_download", format!("{} ({}) must be downloaded manually", m.name, m.filename))
                    .suggest(format!(
                        "download from {} into <pack>/manual/ or your Downloads folder",
                        m.page_url.as_deref().unwrap_or("CurseForge")
                    )),
            );
        }
        for e in &sync.errors {
            problems.push(Problem::error("sync_error", e.clone()));
        }
        for w in &sync.warnings {
            problems.push(Problem::warning("sync_warning", w.clone()));
        }
        let keys: std::collections::BTreeSet<&str> = lock.entries.iter().map(|e| e.key.as_str()).collect();
        for e in &lock.entries {
            for inc in &e.incompatible {
                if keys.contains(inc.as_str()) {
                    problems.push(
                        Problem::error("incompatible_mods", format!("{} declares incompatibility with {inc}", e.name))
                            .mods(vec![e.key.clone(), inc.clone()]),
                    );
                }
            }
        }
        let inst = paths::instance_dir(&dir, profile);
        let (mods, non_mods) = check::scan_mods_dir(&inst.join("mods"))?;
        let java_major = install::version_json(&self.ctx, &eff.minecraft, lock.loader, &lock.loader_version)
            .await
            .ok()
            .and_then(|v| v.java_version.map(|j| j.major_version))
            .or(Some(17));
        if lock.loader != LoaderKind::Vanilla {
            problems.extend(check::check_mods(&mods, &eff.minecraft, java_major, &lock.loader_version));
        } else if !mods.is_empty() {
            problems.push(Problem::warning("vanilla_with_mods", "pack is vanilla but mods/ contains mod jars; they will not load"));
        }
        for f in non_mods {
            problems.push(Problem::warning("not_a_mod", format!("mods/{f} has no fabric.mod.json/quilt.mod.json")));
        }
        let ok = !problems.iter().any(|p| p.severity == "error");
        Ok(CheckReport {
            ok,
            mods_scanned: mods.len(),
            mods: mods
                .iter()
                .map(|m| check::ModSummary { id: m.id.clone(), version: m.version.clone(), file: m.file.clone() })
                .collect(),
            problems,
        })
    }

    // ------------------------------------------------------------------ sessions

    /// All sessions known to any mcpack process: in-memory ones plus the on-disk registry
    /// (sessions launched by another process, e.g. an MCP server, show up here too).
    pub fn sessions(&self) -> Vec<SessionStatus> {
        let mut v: Vec<SessionStatus> = self.sessions.lock().expect("sessions").values().map(|s| s.status()).collect();
        let known: std::collections::HashSet<String> = v.iter().map(|s| s.id.clone()).collect();
        for rec in registry::list_sessions(&self.ctx.paths) {
            if !known.contains(&rec.id) {
                v.push(status_from_record(&rec));
            }
        }
        v.sort_by_key(|s| s.started_ms);
        v
    }

    /// Status of any session (local or from another process).
    pub fn session_status(&self, id: &str) -> Result<SessionStatus> {
        match self.session_ref(id)? {
            SessionRef::Local(s) => Ok(s.status()),
            SessionRef::Remote(r) => Ok(status_from_record(&r)),
        }
    }

    pub(crate) fn session_ref(&self, id: &str) -> Result<SessionRef> {
        if let Some(s) = self.sessions.lock().expect("sessions").get(id).cloned() {
            return Ok(SessionRef::Local(s));
        }
        registry::read_session(&self.ctx.paths, id)
            .map(SessionRef::Remote)
            .ok_or_else(|| Error::NotFound(format!("session '{id}'")))
    }

    pub fn session(&self, id: &str) -> Result<Arc<Session>> {
        self.sessions
            .lock()
            .expect("sessions")
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("session '{id}'")))
    }

    /// Running session for a pack profile.
    pub fn find_session(&self, dir: &Path, profile: &str) -> Option<Arc<Session>> {
        let dir = Self::norm(dir);
        self.sessions
            .lock()
            .expect("sessions")
            .values()
            .filter(|s| s.pack_dir == dir && s.profile == profile && s.running())
            .max_by_key(|s| s.started_ms)
            .cloned()
    }

    /// Resolve a session id by id, or by (pack, profile), or the newest running session.
    /// Considers sessions owned by other mcpack processes too.
    pub fn pick_session(&self, id: Option<&str>, pack: Option<&Path>, profile: Option<&str>) -> Result<String> {
        if let Some(id) = id {
            self.session_ref(id)?;
            return Ok(id.to_string());
        }
        let running: Vec<SessionStatus> = self.sessions().into_iter().filter(|s| s.running).collect();
        let candidates: Vec<&SessionStatus> = match pack {
            Some(p) => {
                let dir = Self::norm(p).display().to_string();
                // Without an explicit profile, any running profile of the pack matches.
                running.iter().filter(|s| s.pack_dir == dir && profile.is_none_or(|pr| s.profile == pr)).collect()
            }
            None => running.iter().collect(),
        };
        candidates.iter().max_by_key(|s| s.started_ms).map(|s| s.id.clone()).ok_or_else(|| {
            Error::NotFound(match pack {
                Some(p) => format!("no running session for {} ({})", p.display(), profile.unwrap_or("default")),
                None => "no running session".into(),
            })
        })
    }

    pub async fn launch(&self, dir: &Path, opts: LaunchOptions) -> Result<LaunchResult> {
        let dir = Self::norm(dir);
        let _g = self.lock_pack(&dir).await;
        self.launch_inner(&dir, opts).await
    }

    pub(crate) async fn launch_inner(&self, dir: &Path, opts: LaunchOptions) -> Result<LaunchResult> {
        let profile = opts.profile.clone().unwrap_or_else(|| pack::DEFAULT_PROFILE.to_string());
        if let Some(s) = self.find_session(dir, &profile) {
            return Err(Error::Launch(format!(
                "profile '{profile}' is already running as session {} (stop or restart it)",
                s.id
            )));
        }
        let ev = self.ctx.events.clone();
        let pack_s = dir.display().to_string();
        let prog = |phase: &str, f: Option<f64>, d: String| ev.progress_profile(Some(&pack_s), Some(&profile), None, phase, f, d);
        prog("resolve", Some(0.0), "resolving pack".into());
        let pack = Pack::load(dir)?;
        let (lock, _) = self.ensure_lock(&pack).await?;
        prog("resolve", Some(1.0), format!("{} entries locked", lock.entries.len()));
        prog("download", None, "syncing pack content".into());
        let sync = instance::sync(&self.ctx, &pack, &lock, &profile, opts.agent, false).await?;
        if !sync.errors.is_empty() {
            return Err(Error::Launch(format!("sync failed: {}", sync.errors.join("; "))));
        }
        let eff = pack.effective(&profile)?;
        let (inst, summary) =
            install::install(&self.ctx, &eff.minecraft, lock.loader, &lock.loader_version, eff.jvm.java.as_deref(), &prog).await?;
        let username = opts
            .username
            .clone()
            .or_else(|| eff.username.clone())
            .unwrap_or_else(|| self.ctx.cfg().username);
        let params = launch::args::LaunchParams {
            uuid: util::offline_uuid(&username),
            username,
            game_dir: paths::instance_dir(dir, &profile),
            max_memory: eff.jvm.memory.clone(),
            min_memory: eff.jvm.min_memory.clone(),
            extra_jvm: eff.jvm.args.iter().chain(opts.extra_jvm_args.iter()).cloned().collect(),
            extra_game: eff.jvm.game_args.iter().chain(opts.extra_game_args.iter()).cloned().collect(),
            width: eff.jvm.width,
            height: eff.jvm.height,
            quick_play_world: opts.world.clone(),
        };
        let agent = if sync.agent { Some(AgentEndpoint::new()?) } else { None };
        let mut sync = sync;
        let hotswap = if pack.file.dev.hotswap {
            match hotswap::endpoint_for(&inst.java) {
                Ok(h) => Some(h),
                Err(why) => {
                    sync.warnings.push(format!("hot-swap unavailable: {why}"));
                    None
                }
            }
        } else {
            None
        };
        prog("launch", None, "starting the game".into());
        let session = launch::spawn(
            &inst,
            &params,
            &self.ctx.platform,
            dir,
            &profile,
            agent,
            opts,
            &eff.minecraft,
            self.ctx.events.clone(),
            self.ctx.paths.clone(),
            hotswap,
        )?;
        self.sessions.lock().expect("sessions").insert(session.id.clone(), session.clone());
        if session.agent.is_some() {
            ev.progress_profile(Some(&pack_s), Some(&profile), Some(&session.id), "agent", None, "waiting for the in-game agent");
        }
        Ok(LaunchResult { session: session.status(), sync, install: summary })
    }

    /// Stop a session: graceful (agent `exit`) first, then kill after `grace`.
    /// Works for sessions owned by other mcpack processes too.
    pub async fn stop(&self, id: &str, grace: Duration) -> Result<SessionStatus> {
        let s = match self.session_ref(id)? {
            SessionRef::Local(s) => s,
            SessionRef::Remote(rec) => return self.stop_remote(rec, grace).await,
        };
        if !s.running() {
            return Ok(s.status());
        }
        s.mark_stop_requested();
        let mut graceful = false;
        if let Some(a) = &s.agent {
            graceful = a.call("exit", serde_json::json!({}), Duration::from_secs(5)).await.is_ok();
        }
        if !graceful || s.wait_exit(grace).await.is_none() {
            s.kill();
            s.wait_exit(Duration::from_secs(15)).await;
        }
        Ok(s.status())
    }

    async fn stop_remote(&self, mut rec: registry::SessionRecord, grace: Duration) -> Result<SessionStatus> {
        if rec.exit.is_none() && registry::pid_alive(rec.pid) {
            let mut graceful = false;
            if let Some(a) = &rec.agent {
                graceful = a.call("exit", serde_json::json!({}), Duration::from_secs(5)).await.is_ok();
            }
            let deadline = std::time::Instant::now() + if graceful { grace } else { Duration::ZERO };
            while std::time::Instant::now() < deadline && registry::pid_alive(rec.pid) {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if let Some(pid) = rec.pid.filter(|_| registry::pid_alive(rec.pid)) {
                registry::kill_pid(pid);
            }
        }
        // The owning process normally records the exit; do it ourselves if it is gone.
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Some(r) = registry::read_session(&self.ctx.paths, &rec.id) {
            rec = r;
        }
        if rec.exit.is_none() && !registry::pid_alive(Some(rec.owner_pid)) {
            rec.exit = Some(launch::ExitInfo { code: None, at_ms: util::now_millis(), crashed: false, crash_report: None, requested: true });
            registry::write_session(&self.ctx.paths, &rec)?;
        }
        Ok(status_from_record(&rec))
    }

    /// Stop and relaunch with the same options (re-syncing first). Rejoins the current world if any.
    pub async fn restart(&self, id: &str) -> Result<LaunchResult> {
        match self.session_ref(id)? {
            SessionRef::Local(s) => {
                let dir = s.pack_dir.clone();
                let _g = self.lock_pack(&dir).await;
                self.restart_inner(&s).await
            }
            SessionRef::Remote(rec) => {
                let mut opts = rec.options.clone();
                if let Some(a) = &rec.agent {
                    if let Ok(st) = a.call("status", serde_json::json!({}), Duration::from_secs(3)).await {
                        if st.get("in_world").and_then(|v| v.as_bool()) == Some(true) {
                            opts.world = st.get("level_id").and_then(|v| v.as_str()).map(str::to_string);
                        }
                    }
                }
                let dir = PathBuf::from(&rec.pack_dir);
                self.stop_remote(rec, Duration::from_secs(20)).await?;
                let _g = self.lock_pack(&dir).await;
                self.launch_inner(&dir, opts).await
            }
        }
    }

    pub(crate) async fn restart_inner(&self, s: &Arc<Session>) -> Result<LaunchResult> {
        let mut opts = s.options.clone();
        if let Some(a) = &s.agent {
            if let Ok(st) = a.call("status", serde_json::json!({}), Duration::from_secs(3)).await {
                if st.get("in_world").and_then(|v| v.as_bool()) == Some(true) {
                    if let Some(level) = st.get("level_id").and_then(|v| v.as_str()) {
                        opts.world = Some(level.to_string());
                    }
                }
            }
        }
        self.stop(&s.id, Duration::from_secs(20)).await?;
        self.launch_inner(&s.pack_dir, opts).await
    }

    pub fn logs(&self, id: &str, since: Option<u64>, tail: Option<usize>, grep: Option<&str>) -> Result<LogsResult> {
        let re = match grep {
            Some(g) if !g.is_empty() => Some(regex::Regex::new(g).map_err(|e| Error::Invalid(format!("bad grep regex: {e}")))?),
            _ => None,
        };
        match self.session_ref(id)? {
            SessionRef::Local(s) => {
                let lines = s.logs(since, tail, re.as_ref());
                Ok(LogsResult { session: s.id.clone(), running: s.running(), next_seq: s.status().log_lines, lines })
            }
            SessionRef::Remote(rec) => {
                let all = registry::read_log_file(Path::new(&rec.log_path));
                let next_seq = all.len() as u64;
                let mut lines: Vec<launch::LogLine> = all
                    .into_iter()
                    .enumerate()
                    .map(|(i, text)| launch::LogLine { seq: i as u64, text })
                    .filter(|l| since.is_none_or(|s| l.seq >= s))
                    .filter(|l| re.as_ref().is_none_or(|r| r.is_match(&l.text)))
                    .collect();
                if let Some(n) = tail {
                    if lines.len() > n {
                        lines = lines.split_off(lines.len() - n);
                    }
                }
                let running = status_from_record(&rec).running;
                Ok(LogsResult { session: rec.id, running, next_seq, lines })
            }
        }
    }

    /// Call the in-game agent directly (works across processes).
    pub async fn agent_call(&self, id: &str, method: &str, params: serde_json::Value, timeout: Duration) -> Result<serde_json::Value> {
        let (agent, running) = match self.session_ref(id)? {
            SessionRef::Local(s) => (s.agent.clone(), s.running()),
            SessionRef::Remote(r) => {
                let running = status_from_record(&r).running;
                (r.agent, running)
            }
        };
        let a = agent.ok_or_else(|| {
            Error::Agent("this session has no agent (vanilla pack, agent disabled, or unsupported Minecraft version)".into())
        })?;
        if !running {
            return Err(Error::Agent("session is not running".into()));
        }
        a.call(method, params, timeout).await
    }

    /// Subscribe to push events (logs, session state, progress, activity, watch).
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<events::Event> {
        self.ctx.events.subscribe()
    }

    // ------------------------------------------------------------------ diagnose

    /// Diagnose the latest run of a profile (or a specific session).
    pub fn diagnose(&self, dir: &Path, profile: &str, session: Option<&str>) -> Result<Diagnosis> {
        let dir = Self::norm(dir);
        let inst = paths::instance_dir(&dir, profile);
        let (log, since, code) = match session {
            Some(id) => {
                let s = self.session(id)?;
                let exit = s.exit();
                (s.all_log_text(), Some(s.started_ms), exit.and_then(|e| e.code))
            }
            None => match self.sessions.lock().expect("sessions").values().filter(|s| s.pack_dir == dir && s.profile == profile).max_by_key(|s| s.started_ms) {
                Some(s) => (s.all_log_text(), Some(s.started_ms), s.exit().and_then(|e| e.code)),
                None => (diagnose::latest_log(&inst), None, None),
            },
        };
        let crash = diagnose::newest_crash_report(&inst, since);
        let crash_text = crash.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
        Ok(diagnose::analyze(&log, crash.as_deref().zip(crash_text.as_deref()), code))
    }

    // ------------------------------------------------------------------ discovery

    pub async fn search(&self, source: Source, query: &str, filters: SearchFilters) -> Result<Vec<SearchHit>> {
        match source {
            Source::Modrinth => Modrinth::new(self.ctx.http.clone()).search(query, &filters).await,
            Source::Curseforge => {
                CurseForge::new(self.ctx.http.clone(), self.ctx.cfg().curseforge_api_key)?.search(query, &filters).await
            }
            other => Err(Error::Invalid(format!("cannot search source '{}'", other.as_str()))),
        }
    }

    pub async fn mod_versions(&self, source: Source, id: &str, minecraft: Option<&str>, loader: Option<LoaderKind>) -> Result<Vec<VersionSummary>> {
        match source {
            Source::Modrinth => Modrinth::new(self.ctx.http.clone()).list_versions(id, minecraft, loader).await,
            Source::Curseforge => {
                CurseForge::new(self.ctx.http.clone(), self.ctx.cfg().curseforge_api_key)?
                    .list_versions(id, minecraft, loader)
                    .await
            }
            other => Err(Error::Invalid(format!("source '{}' has no versions", other.as_str()))),
        }
    }

    // ------------------------------------------------------------------ config

    pub fn config(&self) -> GlobalConfig {
        let mut c = self.ctx.cfg();
        if c.curseforge_api_key.is_some() {
            c.curseforge_api_key = Some("<set>".into());
        }
        c
    }

    /// Persist a global config value (applies immediately).
    pub fn config_set(&self, key: &str, value: &str) -> Result<GlobalConfig> {
        let mut c = GlobalConfig::load(&self.ctx.paths)?;
        match key {
            "curseforge_api_key" => c.curseforge_api_key = Some(value.to_string()).filter(|v| !v.is_empty()),
            "username" => c.username = value.to_string(),
            "java_path" => c.java_path = Some(value.to_string()).filter(|v| !v.is_empty()),
            "download_concurrency" => {
                c.download_concurrency = value.parse().map_err(|_| Error::Invalid("download_concurrency must be a number".into()))?
            }
            other => return Err(Error::Invalid(format!("unknown config key '{other}'"))),
        }
        c.save(&self.ctx.paths)?;
        *self.ctx.config.write().expect("config lock") = GlobalConfig::load(&self.ctx.paths)?;
        Ok(c)
    }
}

pub(crate) enum SessionRef {
    Local(Arc<Session>),
    Remote(registry::SessionRecord),
}

fn status_from_record(r: &registry::SessionRecord) -> SessionStatus {
    let running = r.exit.is_none() && registry::pid_alive(r.pid);
    SessionStatus {
        id: r.id.clone(),
        pack_dir: r.pack_dir.clone(),
        profile: r.profile.clone(),
        minecraft: r.minecraft.clone(),
        instance_dir: r.instance_dir.clone(),
        pid: r.pid,
        started_ms: r.started_ms,
        running,
        exit: r.exit.clone().or_else(|| {
            (!running).then(|| launch::ExitInfo { code: None, at_ms: 0, crashed: false, crash_report: None, requested: false })
        }),
        agent_port: r.agent.as_ref().map(|a| a.port),
        // Hot-swap is driven by the process that launched the game.
        hotswap_port: None,
        ready_after_ms: r.ready_after_ms,
        log_lines: registry::read_log_file(Path::new(&r.log_path)).len() as u64,
    }
}
