//! Hot reload: watch a pack directory and apply changes to the instance and running client.
//!
//! On each (debounced) change batch: re-resolve if needed, dry-run sync to classify the change,
//! then, for a running game:
//! - live-reload what can be reloaded in place (resource packs, datapacks, optionally configs)
//!   through the in-game agent;
//! - hot-swap changed mod code over the JVM debug agent when the session has it (`dev.hotswap`,
//!   see [`crate::hotswap`]);
//! - otherwise, for changes that need a restart (mods added/removed, mixins, JVM/loader settings,
//!   options, most configs): restart and rejoin the world (`dev.restart = "auto"`) or report
//!   `restart_needed` with the reasons and leave the game alone (`"ask"`, the default).
//!
//! Every batch is published as an [`Event::Watch`] (first `applying`, then the outcome) and kept
//! in the watch's event list.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::events::Event;
use crate::hotswap;
use crate::instance::{self, LiveAction, SyncReport};
use crate::pack::schema::{RestartPolicy, Source};
use crate::pack::Pack;
use crate::util;
use crate::Launcher;

const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct WatchEvent {
    pub seq: u64,
    pub at_ms: u64,
    pub pack: String,
    pub profile: String,
    /// Changed files (relative to the pack, or absolute outside it).
    pub paths: Vec<String>,
    /// applying | no_change | synced | live_reload | hotswapped | restart_needed | restarted | error
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub live_actions: Vec<LiveAction>,
    /// Why a restart is (or was) needed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restart_reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotswap: Option<HotswapResult>,
    /// Why mod code could not be hot-swapped (then a restart is needed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotswap_failed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HotswapResult {
    /// Mods whose code was swapped.
    pub mods: Vec<String>,
    /// Classes replaced in the running game.
    pub classes: Vec<String>,
    /// Changed classes not loaded yet: they keep the old version until the next launch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_loaded: Vec<String>,
    /// Changed non-class files in the jars (assets, lang, data): applied at the next launch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pending_resources: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchInfo {
    pub id: String,
    pub pack_dir: String,
    pub profile: String,
    pub session: Option<String>,
    pub events: usize,
    pub watched_paths: Vec<String>,
}

pub struct WatchHandle {
    pub id: String,
    pub pack_dir: PathBuf,
    pub profile: String,
    pub session: Arc<Mutex<Option<String>>>,
    pub events: Arc<Mutex<VecDeque<WatchEvent>>>,
    pub watched: Vec<PathBuf>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl WatchHandle {
    fn info(&self) -> WatchInfo {
        WatchInfo {
            id: self.id.clone(),
            pack_dir: self.pack_dir.display().to_string(),
            profile: self.profile.clone(),
            session: self.session.lock().expect("watch session").clone(),
            events: self.events.lock().expect("events").len(),
            watched_paths: self.watched.iter().map(|p| p.display().to_string()).collect(),
        }
    }
}

fn ignored(rel: &str) -> bool {
    let r = rel.replace('\\', "/");
    r.starts_with("run/")
        || r.starts_with(".git/")
        || r.starts_with("target/")
        || r == "pack.lock"
        || r.ends_with(".tmp")
        || r.ends_with(".part")
        || r.ends_with('~')
        || r.contains("/.mcpack/")
}

impl Launcher {
    /// Start watching a pack. If `session` is given (or one is running for the profile), changes
    /// are pushed into that running client.
    pub async fn watch_start(&self, dir: &Path, profile: &str, session: Option<String>) -> Result<WatchInfo> {
        let dir = std::path::absolute(crate::pack::pack_dir(dir)).unwrap_or_else(|_| dir.to_path_buf());
        let pack = Pack::load(&dir)?;
        pack.effective(profile)?;
        {
            let w = self.watches.lock().expect("watches");
            if let Some(h) = w.values().find(|h| h.pack_dir == dir && h.profile == profile) {
                return Err(Error::Invalid(format!("already watching this profile (watch {})", h.id)));
            }
        }
        let session = session.or_else(|| self.find_session(&dir, profile).map(|s| s.id.clone()));

        // Paths: the pack dir plus directories of local `file` mods outside it.
        let mut watched = vec![dir.clone()];
        for spec in pack.all_specs() {
            if spec.source == Source::File {
                if let Some(p) = &spec.path {
                    let abs = util::resolve_rel(&dir, p);
                    if !abs.starts_with(&dir) {
                        if let Some(parent) = abs.parent() {
                            if !watched.iter().any(|w| parent.starts_with(w)) {
                                watched.push(parent.to_path_buf());
                            }
                        }
                    }
                }
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if matches!(ev.kind, notify::EventKind::Access(_)) {
                    return;
                }
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
        })
        .map_err(|e| Error::Other(format!("cannot start file watcher: {e}")))?;
        for w in &watched {
            let mode = if *w == dir { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
            watcher.watch(w, mode).map_err(|e| Error::Other(format!("cannot watch {}: {e}", w.display())))?;
        }

        let id = format!("w-{}", &util::sha1_bytes(format!("{}{}", dir.display(), util::now_millis()).as_bytes())[..8]);
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        let handle = WatchHandle {
            id: id.clone(),
            pack_dir: dir.clone(),
            profile: profile.to_string(),
            session: Arc::new(Mutex::new(session)),
            events: Arc::new(Mutex::new(VecDeque::new())),
            watched: watched.clone(),
            stop: stop_tx,
        };
        let info = handle.info();
        let session_slot = handle.session.clone();
        let events = handle.events.clone();
        self.watches.lock().expect("watches").insert(id.clone(), handle);

        let debounce = Duration::from_millis(pack.file.dev.debounce_ms.unwrap_or(600));
        let this = self.clone();
        let id_task = id.clone();
        let profile = profile.to_string();
        tokio::spawn(async move {
            let _watcher = watcher; // keep alive
            let mut seq = 0u64;
            loop {
                let first = tokio::select! {
                    p = rx.recv() => match p { Some(p) => p, None => break },
                    _ = stop_rx.changed() => break,
                };
                let mut batch: BTreeSet<PathBuf> = BTreeSet::from([first]);
                // Debounce: collect until quiet.
                loop {
                    match tokio::time::timeout(debounce, rx.recv()).await {
                        Ok(Some(p)) => {
                            batch.insert(p);
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
                let rels: Vec<String> = batch
                    .iter()
                    .map(|p| p.strip_prefix(&dir).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| p.display().to_string()))
                    .filter(|r| !ignored(r))
                    .collect();
                if rels.is_empty() {
                    continue;
                }
                // Prefer the session given at start; otherwise whatever runs this profile now
                // (the game may have been launched after the watch started).
                let sess = session_slot
                    .lock()
                    .expect("watch session")
                    .clone()
                    .filter(|id| this.session(id).is_ok_and(|s| s.running()))
                    .or_else(|| this.find_session(&dir, &profile).map(|s| s.id.clone()));
                seq += 1;
                let mut applying = WatchEvent::new(&dir, &profile, sess.as_deref());
                applying.seq = seq;
                applying.paths = rels.clone();
                applying.action = "applying".into();
                this.ctx.events.emit(Event::Watch { watch: id_task.clone(), event: applying });

                let mut ev = this.hot_apply(&dir, &profile, sess.as_deref()).await;
                ev.seq = seq;
                ev.paths = rels;
                if let Some(s) = &ev.session {
                    *session_slot.lock().expect("watch session") = Some(s.clone());
                }
                this.ctx.events.emit(Event::Watch { watch: id_task.clone(), event: ev.clone() });
                let mut q = events.lock().expect("events");
                q.push_back(ev);
                while q.len() > MAX_EVENTS {
                    q.pop_front();
                }
            }
        });
        Ok(info)
    }

    pub fn watch_stop(&self, id: &str) -> Result<WatchInfo> {
        let h = self
            .watches
            .lock()
            .expect("watches")
            .remove(id)
            .ok_or_else(|| Error::NotFound(format!("watch '{id}'")))?;
        let _ = h.stop.send(true);
        Ok(h.info())
    }

    pub fn watches(&self) -> Vec<WatchInfo> {
        self.watches.lock().expect("watches").values().map(|h| h.info()).collect()
    }

    /// Events with seq > `since`.
    pub fn watch_events(&self, id: &str, since: Option<u64>) -> Result<Vec<WatchEvent>> {
        let w = self.watches.lock().expect("watches");
        let h = w.get(id).ok_or_else(|| Error::NotFound(format!("watch '{id}'")))?;
        let ev = h.events.lock().expect("events");
        Ok(ev.iter().filter(|e| since.is_none_or(|s| e.seq > s)).cloned().collect())
    }

    /// Apply the current pack state to the instance and (if given) the running session,
    /// choosing live reload vs restart. This is what the watcher runs on every change.
    pub async fn hot_apply(&self, dir: &Path, profile: &str, session: Option<&str>) -> WatchEvent {
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        let mut ev = WatchEvent::new(&dir, profile, session);
        match self.hot_apply_inner(&dir, profile, session, &mut ev).await {
            Ok(()) => {}
            Err(e) => {
                ev.action = "error".into();
                ev.error = Some(e.to_string());
            }
        }
        ev
    }

    async fn hot_apply_inner(&self, dir: &Path, profile: &str, session: Option<&str>, ev: &mut WatchEvent) -> Result<()> {
        let _g = self.lock_pack(dir).await;
        let pack = Pack::load(dir)?;
        let (lock, _) = self.ensure_lock(&pack).await?;
        let running = session.and_then(|id| self.session(id).ok()).filter(|s| s.running());
        let agent_override = running.as_ref().map(|s| s.agent.is_some());
        let plan = instance::sync(&self.ctx, &pack, &lock, profile, agent_override, true).await?;
        if !plan.has_changes() {
            ev.sync = Some(plan);
            return Ok(());
        }
        let Some(s) = running else {
            // No game running: just bring the instance up to date for the next launch.
            let applied = instance::sync(&self.ctx, &pack, &lock, profile, None, false).await?;
            ev.action = "synced".into();
            ev.sync = Some(applied);
            return Ok(());
        };
        if !plan.restart_required {
            let applied = instance::sync(&self.ctx, &pack, &lock, profile, agent_override, false).await?;
            ev.action = "synced".into();
            self.live_reload(&s, &applied, ev).await;
            ev.sync = Some(applied);
            return Ok(());
        }

        // A restart is needed as things stand. Are mod jars the only reason? Then the rest can be
        // applied live while the mods are hot-swapped (or wait for a restart).
        let without_mods = instance::sync_opts(&self.ctx, &pack, &lock, profile, agent_override, true, true).await?;
        let only_mods = !without_mods.restart_required;
        if only_mods {
            if let Some(hs) = &s.hotswap {
                match self.try_hotswap(&s, hs.port, &plan).await {
                    Ok(result) => {
                        let applied = instance::sync_opts(&self.ctx, &pack, &lock, profile, agent_override, false, true).await?;
                        ev.action = "synced".into();
                        self.live_reload(&s, &applied, ev).await;
                        // Mods already swapped earlier in this session are not news.
                        if !result.mods.is_empty() {
                            ev.action = "hotswapped".into();
                            ev.hotswap = Some(result);
                        }
                        ev.sync = Some(applied);
                        return Ok(());
                    }
                    Err(why) => ev.hotswap_failed = Some(why),
                }
            } else if pack.file.dev.hotswap {
                ev.hotswap_failed = Some("the game was started before hot-swap was turned on; it applies from the next launch".into());
            }
        }

        ev.restart_reasons = plan.restart_reasons.clone();
        match pack.file.dev.restart {
            RestartPolicy::Auto => {
                let r = self.restart_inner(&s).await?;
                ev.action = "restarted".into();
                ev.session = Some(r.session.id.clone());
                ev.sync = Some(r.sync);
            }
            RestartPolicy::Ask => {
                ev.action = "restart_needed".into();
                if only_mods && without_mods.has_changes() {
                    // Apply what does not need the restart now; the mods wait for it.
                    let applied = instance::sync_opts(&self.ctx, &pack, &lock, profile, agent_override, false, true).await?;
                    self.live_reload(&s, &applied, ev).await;
                    ev.action = "restart_needed".into();
                    ev.sync = Some(applied);
                } else {
                    ev.sync = Some(plan);
                }
            }
        }
        Ok(())
    }

    /// Run the live actions a sync asks for through the in-game agent.
    async fn live_reload(&self, s: &Arc<crate::launch::Session>, applied: &SyncReport, ev: &mut WatchEvent) {
        let Some(agent) = &s.agent else { return };
        for a in &applied.live_actions {
            let res = match a {
                LiveAction::ReloadResources => {
                    agent
                        .call("reload_resources", serde_json::json!({"enable": applied.enable_resourcepacks}), Duration::from_secs(180))
                        .await
                }
                LiveAction::ReloadDatapacks => agent.call("reload_datapacks", serde_json::json!({}), Duration::from_secs(120)).await,
            };
            match res {
                Ok(_) => ev.live_actions.push(*a),
                // Datapack reload needs a world; not being in one is fine.
                Err(e) if *a == LiveAction::ReloadDatapacks => tracing::debug!("datapack reload skipped: {e}"),
                Err(e) => ev.error = Some(format!("live reload failed: {e}")),
            }
        }
        if !ev.live_actions.is_empty() {
            ev.action = "live_reload".into();
        }
    }

    /// Swap the code of changed mod jars into the running game. `Err` says why it cannot be done
    /// (a restart is needed instead).
    async fn try_hotswap(&self, s: &Arc<crate::launch::Session>, port: u16, plan: &SyncReport) -> std::result::Result<HotswapResult, String> {
        let not_swappable: Vec<&str> = plan.mod_changes.iter().filter(|c| c.status != "updated").map(|c| c.label.as_str()).collect();
        if !not_swappable.is_empty() {
            return Err(format!(
                "mods were added or removed ({}); Fabric loads mods only at startup",
                not_swappable.join(", ")
            ));
        }
        let snapshots = s.instance_dir.join(".mcpack").join("hotswap");
        let swapped_before = s.swapped.lock().expect("swapped").clone();
        let mut result = HotswapResult::default();
        let mut classes: Vec<(String, Vec<u8>)> = Vec::new();
        let mut todo: Vec<(&crate::instance::ModFileChange, PathBuf, PathBuf)> = Vec::new();
        for c in &plan.mod_changes {
            let (Some(src), Some(sha)) = (&c.src, &c.sha1) else { continue };
            if swapped_before.get(&c.rel) == Some(sha) {
                continue; // already running this version
            }
            let file = Path::new(&c.rel).file_name().map(|f| f.to_os_string()).unwrap_or_default();
            // The version the game runs now: the last one swapped in, else the jar it loaded.
            let snapshot = snapshots.join(&file);
            let base = if swapped_before.contains_key(&c.rel) && snapshot.exists() { snapshot.clone() } else { s.instance_dir.join(&c.rel) };
            let new = PathBuf::from(src);
            let diff = hotswap::diff_jars(&base, &new).map_err(|e| format!("{}: {e}", c.label))?;
            if let Some(why) = diff.blocker() {
                return Err(format!("{}: {why}", c.label));
            }
            result.mods.push(c.label.clone());
            result.pending_resources.extend(diff.resources_changed.iter().map(|r| format!("{} ({})", r, c.label)));
            classes.extend(diff.changed);
            todo.push((c, new, snapshot));
        }
        if !classes.is_empty() {
            let r = hotswap::redefine(port, &classes).await?;
            result.classes = r.classes;
            result.not_loaded = r.not_loaded;
        }
        // Remember what runs now, so the next change is compared against it.
        let _ = std::fs::create_dir_all(&snapshots);
        let mut swapped = s.swapped.lock().expect("swapped");
        for (c, new, snapshot) in todo {
            if std::fs::copy(&new, &snapshot).is_ok() {
                if let Some(sha) = &c.sha1 {
                    swapped.insert(c.rel.clone(), sha.clone());
                }
            }
        }
        Ok(result)
    }
}

impl WatchEvent {
    fn new(dir: &Path, profile: &str, session: Option<&str>) -> Self {
        WatchEvent {
            seq: 0,
            at_ms: util::now_millis(),
            pack: dir.display().to_string(),
            profile: profile.to_string(),
            paths: vec![],
            action: "no_change".into(),
            session: session.map(str::to_string),
            live_actions: vec![],
            restart_reasons: vec![],
            hotswap: None,
            hotswap_failed: None,
            sync: None,
            error: None,
        }
    }
}
