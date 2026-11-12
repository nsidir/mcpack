// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Desktop shell: hosts the UI and links mcpack-core in-process.
//!
//! - `mcpack_call(tool, args)` runs any core tool (same names as `mcpack call` / MCP) and rejects
//!   with `{ code, message }` from `Error::code()`.
//! - `mcpack://event` carries `BackendEvent`s (see app/src/api/types.ts): instance state with launch
//!   phases/progress, batched log lines, agent status, content/pack refreshes, activity records.
//!   Sessions and activity from *other* processes (e.g. an AI agent's `mcpack serve`) are picked up
//!   from the on-disk registry and journal, so the UI shows everything regardless of who started it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mcpack_core::events::Event;
use mcpack_core::registry::{self, ActivityEntry};
use mcpack_core::tools::{self, CallContext};
use mcpack_core::{Launcher, SessionStatus};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

const EVENT: &str = "mcpack://event";

/// Tools whose success changes a pack's content or list entry.
const MUTATING: &[&str] = &[
    "pack_init", "pack_add", "pack_remove", "pack_set", "pack_unset", "resolve", "update", "doctor", "sync", "import_mrpack",
    "pack_forget", "pack_delete", "watch_start", "watch_stop", "hot_apply", "import_pack",
];

struct AppState {
    launcher: Launcher,
    bridge: Arc<Bridge>,
}

#[derive(serde::Serialize)]
struct CallError {
    code: String,
    message: String,
}

#[tauri::command]
async fn mcpack_call(state: State<'_, AppState>, tool: String, args: Value) -> Result<Value, CallError> {
    let r = tools::call(&state.launcher, &tool, args, &CallContext::new("app"))
        .await
        .map_err(|e| CallError { code: e.code().into(), message: e.to_string() });
    // A just-started local session: remember its pack right away.
    if let Ok(v) = &r {
        if let Some(id) = v.pointer("/session/id").and_then(|x| x.as_str()) {
            if let Ok(st) = state.launcher.session_status(id) {
                state.bridge.learn(&st);
            }
        }
    }
    r
}

/// Current instance state per pack dir. The UI calls this once on startup: events emitted before
/// the webview subscribed are gone, so a game that was already running (started by an earlier app
/// run, the CLI or an agent) would otherwise look stopped and its Launch button stay clickable.
#[tauri::command]
async fn mcpack_instances(app: AppHandle, state: State<'_, AppState>) -> Result<Value, CallError> {
    let l = state.launcher.clone();
    let sessions = tauri::async_runtime::spawn_blocking(move || l.sessions()).await.unwrap_or_default();
    for st in sessions.iter().filter(|s| s.running) {
        state.bridge.learn(st);
        let active = state
            .bridge
            .instances
            .lock()
            .unwrap()
            .get(&st.pack_dir)
            .and_then(|s| s.get("status").and_then(|x| x.as_str()).map(|x| x == "running" || x == "preparing"))
            .unwrap_or(false);
        if !active {
            state.bridge.update_instance(&app, &st.pack_dir, Bridge::session_patch(st, false));
        }
    }
    let map = state.bridge.instances.lock().unwrap();
    Ok(Value::Object(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
}

/// Per-pack UI state and bookkeeping for event translation.
#[derive(Default)]
struct Bridge {
    /// session id → (pack dir, profile)
    sessions: Mutex<HashMap<String, (String, String)>>,
    /// sessions whose events arrive in-process (others are polled)
    local: Mutex<HashSet<String>>,
    /// pack dir → current InstanceState JSON (always emitted whole)
    instances: Mutex<HashMap<String, Value>>,
    /// pack dir → pending log lines (flushed in batches)
    logs: Mutex<HashMap<String, Vec<Value>>>,
    /// remote session id → next log seq to read
    remote_offsets: Mutex<HashMap<String, u64>>,
    /// activity id → last emitted status
    activity_seen: Mutex<HashMap<String, String>>,
    /// last agent status per pack (avoid re-emitting identical payloads)
    agent_last: Mutex<HashMap<String, Value>>,
}

impl Bridge {
    fn learn(&self, st: &SessionStatus) {
        self.sessions.lock().unwrap().insert(st.id.clone(), (st.pack_dir.clone(), st.profile.clone()));
    }

    fn pack_of(&self, session: &str, launcher: &Launcher) -> Option<(String, String)> {
        if let Some(p) = self.sessions.lock().unwrap().get(session).cloned() {
            return Some(p);
        }
        let st = launcher.session_status(session).ok()?;
        self.learn(&st);
        Some((st.pack_dir, st.profile))
    }

    /// Merge `patch` into the pack's instance state and emit the whole state.
    fn update_instance(&self, app: &AppHandle, pack: &str, patch: Value) {
        let state = {
            let mut map = self.instances.lock().unwrap();
            let cur = map.entry(pack.to_string()).or_insert_with(|| json!({"status": "stopped", "profile": "default"}));
            if let (Some(c), Some(p)) = (cur.as_object_mut(), patch.as_object()) {
                for (k, v) in p {
                    if v.is_null() {
                        c.remove(k);
                    } else {
                        c.insert(k.clone(), v.clone());
                    }
                }
            }
            cur.clone()
        };
        let _ = app.emit(EVENT, json!({"type": "instance", "packId": pack, "state": state}));
    }

    fn session_patch(st: &SessionStatus, ready: bool) -> Value {
        if st.running {
            let waiting_agent = st.agent_port.is_some() && st.ready_after_ms.is_none() && !ready;
            json!({
                "status": if waiting_agent { "preparing" } else { "running" },
                "profile": st.profile,
                "phase": if waiting_agent { json!("agent") } else { Value::Null },
                "detail": if waiting_agent { json!("Waiting for the title screen") } else { Value::Null },
                "progress": Value::Null,
                "startedAt": st.started_ms,
                "pid": st.pid,
                "agentPort": st.agent_port,
                "exitCode": Value::Null,
            })
        } else {
            let crashed = st.exit.as_ref().is_some_and(|e| e.crashed);
            json!({
                "status": if crashed { "crashed" } else { "stopped" },
                "profile": st.profile,
                "phase": Value::Null, "progress": Value::Null, "detail": Value::Null,
                "pid": Value::Null, "agentPort": Value::Null, "startedAt": Value::Null,
                "exitCode": st.exit.as_ref().and_then(|e| e.code),
            })
        }
    }

    fn push_log(&self, pack: &str, seq: u64, t: u64, level: &str, source: &str, text: String) {
        let level = match level {
            "fatal" | "error" => "error",
            "warn" => "warn",
            "debug" => "debug",
            _ => "info",
        };
        self.logs
            .lock()
            .unwrap()
            .entry(pack.to_string())
            .or_default()
            .push(json!({"id": seq, "t": t, "level": level, "source": source, "text": text}));
    }

    fn flush_logs(&self, app: &AppHandle) {
        let batches: Vec<(String, Vec<Value>)> = self.logs.lock().unwrap().drain().collect();
        for (pack, lines) in batches {
            if !lines.is_empty() {
                let _ = app.emit(EVENT, json!({"type": "log", "packId": pack, "lines": lines}));
            }
        }
    }

    /// Emit an activity record unless this exact (id, status) was already sent.
    fn emit_activity(&self, app: &AppHandle, e: &ActivityEntry) -> bool {
        {
            let mut seen = self.activity_seen.lock().unwrap();
            if seen.get(&e.id) == Some(&e.status) {
                return false;
            }
            seen.insert(e.id.clone(), e.status.clone());
        }
        let _ = app.emit(EVENT, json!({"type": "activity", "event": activity_json(e)}));
        true
    }
}

/// ActivityEntry → UI ActivityEvent.
fn activity_json(e: &ActivityEntry) -> Value {
    json!({
        "id": e.id,
        "t": e.ts,
        "origin": e.origin,
        "client": e.client,
        "tool": e.tool,
        "packId": e.pack,
        "summary": summarize(e),
        "status": e.status,
        "durationMs": e.duration_ms,
        "errorCode": e.error_code,
        "params": e.args,
        "result": e.result,
    })
}

fn summarize(e: &ActivityEntry) -> String {
    if let Some(err) = &e.error {
        return err.chars().take(160).collect();
    }
    let a = &e.args;
    let get = |k: &str| a.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let pack_name = e.pack.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().to_string());
    let s = match e.tool.as_str() {
        "pack_add" => format!("Add {}", get("id").or(get("slug")).or(get("path")).or(get("url")).unwrap_or_default()),
        "pack_remove" => format!("Remove {}", get("id").or(get("slug")).unwrap_or_default()),
        "pack_set" => format!("Set {} = {}", get("path").unwrap_or_default(), a.get("value").map(|v| v.to_string()).unwrap_or_default()),
        "pack_unset" => format!("Unset {}", get("path").unwrap_or_default()),
        "pack_init" => format!("Create pack {}", get("name").unwrap_or_default()),
        "launch" => format!("Launch profile {}", get("profile").unwrap_or_else(|| "default".into())),
        "smoke_test" => match e.result.as_ref().and_then(|r| r.get("passed")).and_then(|v| v.as_bool()) {
            Some(true) => "Smoke test passed".into(),
            Some(false) => format!(
                "Smoke test failed: {}",
                e.result.as_ref().and_then(|r| r.get("outcome")).and_then(|v| v.as_str()).unwrap_or("?")
            ),
            None => "Smoke test".into(),
        },
        "game_command" => format!("/{}", get("command").unwrap_or_default().trim_start_matches('/')),
        "agent_call" => format!("Agent {}", get("method").unwrap_or_default()),
        "doctor" => {
            let n = e.result.as_ref().and_then(|r| r.get("actions")).and_then(|v| v.as_array()).map(|v| v.len()).unwrap_or(0);
            format!("Doctor: {n} fix(es)")
        }
        other => other.replace('_', " "),
    };
    match pack_name {
        Some(p) if !s.is_empty() => format!("{s} · {p}"),
        _ => s,
    }
}

fn spawn_bridge(app: AppHandle, launcher: Launcher, bridge: Arc<Bridge>) {
    // 1) In-process events.
    {
        let app = app.clone();
        let launcher = launcher.clone();
        let bridge = bridge.clone();
        let mut rx = launcher.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                let ev = match rx.recv().await {
                    Ok(e) => e,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                match ev {
                    Event::Log { session, seq, t, level, source, text } => {
                        bridge.local.lock().unwrap().insert(session.clone());
                        if let Some((pack, _)) = bridge.pack_of(&session, &launcher) {
                            bridge.push_log(&pack, seq, t, &level, &source, text);
                        }
                    }
                    Event::Session { session, state, status } => {
                        bridge.local.lock().unwrap().insert(session.clone());
                        bridge.learn(&status);
                        bridge.update_instance(&app, &status.pack_dir, Bridge::session_patch(&status, state == "ready"));
                        if !status.running {
                            bridge.agent_last.lock().unwrap().remove(&status.pack_dir);
                            let _ = app.emit(EVENT, json!({"type": "agent", "packId": status.pack_dir, "status": null}));
                        }
                    }
                    Event::Progress { pack, profile, phase, fraction, detail, .. } => {
                        if let Some(pack) = pack {
                            bridge.update_instance(
                                &app,
                                &pack,
                                json!({
                                    "status": "preparing",
                                    "profile": profile.unwrap_or_else(|| "default".into()),
                                    "phase": phase,
                                    "progress": fraction,
                                    "detail": detail,
                                    "exitCode": Value::Null,
                                }),
                            );
                        }
                    }
                    Event::Activity { entry } => {
                        bridge.emit_activity(&app, &entry);
                        if entry.status == "ok" && MUTATING.contains(&entry.tool.as_str()) {
                            refresh_pack(&app, &launcher, entry.pack.as_deref()).await;
                        }
                        // A failed launch leaves the pack "preparing": reset it.
                        if entry.status == "error" && matches!(entry.tool.as_str(), "launch" | "smoke_test") {
                            if let Some(p) = &entry.pack {
                                bridge.update_instance(
                                    &app,
                                    p,
                                    json!({"status": "stopped", "phase": Value::Null, "progress": Value::Null, "detail": entry.error}),
                                );
                            }
                        }
                    }
                    Event::Watch { .. } => {}
                }
            }
        });
    }

    // 2) Log batching (keeps the UI smooth when the game is chatty).
    {
        let app = app.clone();
        let bridge = bridge.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(120)).await;
                bridge.flush_logs(&app);
            }
        });
    }

    // 3) Polling: other processes' sessions + activity, and live agent status.
    tauri::async_runtime::spawn(async move {
        let mut last_remote: HashMap<String, String> = HashMap::new();
        let mut tick: u64 = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            tick += 1;
            let sessions = {
                let l = launcher.clone();
                tauri::async_runtime::spawn_blocking(move || l.sessions()).await.unwrap_or_default()
            };
            let local = bridge.local.lock().unwrap().clone();
            for st in &sessions {
                bridge.learn(st);
                if local.contains(&st.id) {
                    continue;
                }
                // Session owned by another process: state changes + log tail.
                let key = format!("{}|{:?}|{:?}", st.running, st.ready_after_ms, st.exit.as_ref().map(|e| e.code));
                if last_remote.get(&st.id) != Some(&key) {
                    if st.running || last_remote.contains_key(&st.id) {
                        bridge.update_instance(&app, &st.pack_dir, Bridge::session_patch(st, false));
                    }
                    last_remote.insert(st.id.clone(), key);
                }
                if st.running {
                    let since = *bridge.remote_offsets.lock().unwrap().get(&st.id).unwrap_or(&0);
                    if let Ok(r) = launcher.logs(&st.id, Some(since), Some(2000), None) {
                        for line in r.lines {
                            let level = mcpack_core::events::log_level(&line.text).to_string();
                            bridge.push_log(&st.pack_dir, line.seq, mcpack_core::util::now_millis(), &level, "game", line.text);
                        }
                        bridge.remote_offsets.lock().unwrap().insert(st.id.clone(), r.next_seq);
                    }
                }
            }
            // Live agent status for running sessions.
            for st in sessions.iter().filter(|s| s.running && s.agent_port.is_some()) {
                if let Ok(status) = launcher.agent_call(&st.id, "status", json!({}), Duration::from_millis(1500)).await {
                    let changed = bridge.agent_last.lock().unwrap().get(&st.pack_dir) != Some(&status);
                    if changed {
                        bridge.agent_last.lock().unwrap().insert(st.pack_dir.clone(), status.clone());
                        let _ = app.emit(EVENT, json!({"type": "agent", "packId": st.pack_dir, "status": status}));
                    }
                    let ready = status.get("title_screen").and_then(|v| v.as_bool()) == Some(true)
                        || status.get("in_world").and_then(|v| v.as_bool()) == Some(true);
                    let preparing = bridge
                        .instances
                        .lock()
                        .unwrap()
                        .get(&st.pack_dir)
                        .and_then(|s| s.get("status").and_then(|x| x.as_str()).map(|x| x == "preparing"))
                        .unwrap_or(false);
                    if ready && preparing {
                        bridge.update_instance(&app, &st.pack_dir, Bridge::session_patch(st, true));
                    }
                }
            }
            // Activity from other processes (journal tail), every ~3s.
            if tick % 2 == 0 {
                let home = launcher.ctx().paths.clone();
                let entries = tauri::async_runtime::spawn_blocking(move || registry::read_activity(&home, 50, None, None))
                    .await
                    .unwrap_or_default();
                for e in entries.iter().rev().filter(|e| e.origin != "app") {
                    let emitted = bridge.emit_activity(&app, e);
                    if emitted && e.status == "ok" && MUTATING.contains(&e.tool.as_str()) {
                        refresh_pack(&app, &launcher, e.pack.as_deref()).await;
                    }
                }
            }
        }
    });
}

/// Emit fresh `packs` and `content` events after a change.
async fn refresh_pack(app: &AppHandle, launcher: &Launcher, pack: Option<&str>) {
    let cx = CallContext::new("app");
    if let Ok(packs) = tools::call(launcher, "pack_list", json!({}), &cx).await {
        let _ = app.emit(EVENT, json!({"type": "packs", "packs": packs}));
    }
    if let Some(p) = pack {
        if let Ok(info) = tools::call(launcher, "pack_show", json!({"pack": p}), &cx).await {
            let _ = app.emit(EVENT, json!({"type": "content", "packId": p, "content": info.get("content").cloned().unwrap_or(json!([]))}));
        }
    }
}

fn main() {
    let launcher = match Launcher::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("mcpack: cannot initialize launcher: {e}");
            std::process::exit(1);
        }
    };
    // Forget sessions that ended more than a day ago.
    registry::prune_sessions(&launcher.ctx().paths, 24 * 3600 * 1000);
    let bridge = Arc::new(Bridge::default());
    tauri::Builder::default()
        .manage(AppState { launcher: launcher.clone(), bridge: bridge.clone() })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![mcpack_call, mcpack_instances])
        .setup(move |app| {
            spawn_bridge(app.handle().clone(), launcher.clone(), bridge.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start mcpack");
}
