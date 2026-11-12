//! Hot reload: watch a pack directory and apply changes to the instance and running client.
//!
//! On each (debounced) change batch: re-resolve if needed, dry-run sync to classify the change,
//! then either live-reload (resource packs, datapacks, optionally configs) via the agent, or
//! stop → sync → relaunch (mods, JVM, loader, options), rejoining the current world.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::instance::{self, LiveAction, SyncReport};
use crate::pack::schema::Source;
use crate::pack::Pack;
use crate::util;
use crate::Launcher;

const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct WatchEvent {
    pub seq: u64,
    pub at_ms: u64,
    pub paths: Vec<String>,
    /// no_change | synced | live_reload | restarted | launched | error
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub live_actions: Vec<LiveAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
                let sess = session_slot.lock().expect("watch session").clone();
                let mut ev = this.hot_apply(&dir, &profile, sess.as_deref()).await;
                seq += 1;
                ev.seq = seq;
                ev.paths = rels;
                if let Some(s) = &ev.session {
                    *session_slot.lock().expect("watch session") = Some(s.clone());
                }
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
        let mut ev = WatchEvent {
            seq: 0,
            at_ms: util::now_millis(),
            paths: vec![],
            action: "no_change".into(),
            session: session.map(str::to_string),
            live_actions: vec![],
            sync: None,
            error: None,
        };
        match self.hot_apply_inner(dir, profile, session, &mut ev).await {
            Ok(()) => {}
            Err(e) => {
                ev.action = "error".into();
                ev.error = Some(e.to_string());
            }
        }
        ev
    }

    async fn hot_apply_inner(&self, dir: &Path, profile: &str, session: Option<&str>, ev: &mut WatchEvent) -> Result<()> {
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        let _g = self.lock_pack(&dir).await;
        let pack = Pack::load(&dir)?;
        let (lock, _) = self.ensure_lock(&pack).await?;
        let running = session.and_then(|id| self.session(id).ok()).filter(|s| s.running());
        let agent_override = running.as_ref().map(|s| s.agent.is_some());
        let plan = instance::sync(&self.ctx, &pack, &lock, profile, agent_override, true).await?;
        if !plan.has_changes() {
            ev.sync = Some(plan);
            return Ok(());
        }
        match running {
            Some(s) if plan.restart_required => {
                let r = self.restart_inner(&s).await?;
                ev.action = "restarted".into();
                ev.session = Some(r.session.id.clone());
                ev.sync = Some(r.sync);
            }
            Some(s) => {
                let applied = instance::sync(&self.ctx, &pack, &lock, profile, agent_override, false).await?;
                ev.action = "synced".into();
                if s.agent.is_some() {
                    for a in &applied.live_actions {
                        let res = match a {
                            LiveAction::ReloadResources => {
                                s.agent
                                    .as_ref()
                                    .expect("agent")
                                    .call(
                                        "reload_resources",
                                        serde_json::json!({"enable": applied.enable_resourcepacks}),
                                        Duration::from_secs(180),
                                    )
                                    .await
                            }
                            LiveAction::ReloadDatapacks => {
                                s.agent.as_ref().expect("agent").call("reload_datapacks", serde_json::json!({}), Duration::from_secs(120)).await
                            }
                        };
                        match res {
                            Ok(_) => ev.live_actions.push(*a),
                            // Datapack reload needs a world; not being in one is fine.
                            Err(e) if *a == LiveAction::ReloadDatapacks => {
                                tracing::debug!("datapack reload skipped: {e}");
                            }
                            Err(e) => ev.error = Some(format!("live reload failed: {e}")),
                        }
                    }
                    if !ev.live_actions.is_empty() {
                        ev.action = "live_reload".into();
                    }
                }
                ev.sync = Some(applied);
            }
            None => {
                let applied = instance::sync(&self.ctx, &pack, &lock, profile, None, false).await?;
                ev.action = "synced".into();
                ev.sync = Some(applied);
            }
        }
        Ok(())
    }
}
