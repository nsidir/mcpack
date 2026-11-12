//! Running game sessions: process supervision + log capture.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{oneshot, watch};

use crate::agent::AgentEndpoint;
use crate::events::{log_level, Event, EventBus};
use crate::paths::Paths;
use crate::registry::{self, SessionRecord};
use crate::util;

const LOG_CAP: usize = 20_000;

/// Options used to start a session (kept for restarts).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LaunchOptions {
    /// Profile name (default: "default").
    #[serde(default)]
    pub profile: Option<String>,
    /// Force agent on/off (default: profile setting; on for fabric/quilt).
    #[serde(default)]
    pub agent: Option<bool>,
    /// Join this singleplayer world (folder name) directly via Quick Play.
    #[serde(default)]
    pub world: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub extra_jvm_args: Vec<String>,
    #[serde(default)]
    pub extra_game_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub at_ms: u64,
    /// Non-zero exit or a crash report written during the session.
    pub crashed: bool,
    pub crash_report: Option<String>,
    /// Stopped on request (stop/restart), not by itself.
    pub requested: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub seq: u64,
    pub text: String,
}

#[derive(Default)]
struct LogBuffer {
    lines: VecDeque<LogLine>,
    next: u64,
}

pub struct Session {
    pub id: String,
    pub pack_dir: PathBuf,
    pub profile: String,
    pub instance_dir: PathBuf,
    pub pid: Option<u32>,
    pub started_ms: u64,
    pub agent: Option<AgentEndpoint>,
    pub options: LaunchOptions,
    pub minecraft: String,
    log: Mutex<LogBuffer>,
    kill_tx: Mutex<Option<oneshot::Sender<()>>>,
    stop_requested: std::sync::atomic::AtomicBool,
    exit_rx: watch::Receiver<Option<ExitInfo>>,
    exit_tx: watch::Sender<Option<ExitInfo>>,
    ready: Mutex<Option<u64>>,
    events: EventBus,
    paths: Paths,
    log_file: Mutex<Option<std::io::BufWriter<std::fs::File>>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStatus {
    pub id: String,
    pub pack_dir: String,
    pub profile: String,
    pub minecraft: String,
    pub instance_dir: String,
    pub pid: Option<u32>,
    pub started_ms: u64,
    pub running: bool,
    pub exit: Option<ExitInfo>,
    pub agent_port: Option<u16>,
    /// ms since start when the title screen was first observed.
    pub ready_after_ms: Option<u64>,
    pub log_lines: u64,
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        pack_dir: PathBuf,
        profile: String,
        instance_dir: PathBuf,
        pid: Option<u32>,
        agent: Option<AgentEndpoint>,
        options: LaunchOptions,
        minecraft: String,
        kill_tx: oneshot::Sender<()>,
        events: EventBus,
        paths: Paths,
    ) -> Self {
        let (exit_tx, exit_rx) = watch::channel(None);
        let log_path = registry::session_log_path(&paths, &id);
        let _ = util::ensure_parent(&log_path);
        let log_file = std::fs::File::create(&log_path).ok().map(std::io::BufWriter::new);
        Self {
            id,
            pack_dir,
            profile,
            instance_dir,
            pid,
            started_ms: util::now_millis(),
            agent,
            options,
            minecraft,
            log: Mutex::new(LogBuffer::default()),
            kill_tx: Mutex::new(Some(kill_tx)),
            stop_requested: std::sync::atomic::AtomicBool::new(false),
            exit_rx,
            exit_tx,
            ready: Mutex::new(None),
            events,
            paths,
            log_file: Mutex::new(log_file),
        }
    }

    /// Persist this session to the cross-process registry.
    pub fn persist(&self) {
        let rec = SessionRecord {
            id: self.id.clone(),
            pack_dir: self.pack_dir.display().to_string(),
            profile: self.profile.clone(),
            minecraft: self.minecraft.clone(),
            instance_dir: self.instance_dir.display().to_string(),
            pid: self.pid,
            started_ms: self.started_ms,
            agent: self.agent.clone(),
            options: self.options.clone(),
            log_path: registry::session_log_path(&self.paths, &self.id).display().to_string(),
            owner_pid: std::process::id(),
            ready_after_ms: self.ready_after_ms(),
            exit: self.exit(),
        };
        if let Err(e) = registry::write_session(&self.paths, &rec) {
            tracing::warn!("cannot persist session {}: {e}", self.id);
        }
    }

    fn emit_state(&self, state: &str) {
        self.events.emit(Event::Session { session: self.id.clone(), state: state.into(), status: self.status() });
    }

    pub fn push_log(&self, text: String) {
        let seq = {
            let mut log = self.log.lock().expect("log lock");
            let seq = log.next;
            log.next += 1;
            log.lines.push_back(LogLine { seq, text: text.clone() });
            while log.lines.len() > LOG_CAP {
                log.lines.pop_front();
            }
            seq
        };
        if let Some(f) = self.log_file.lock().expect("log file").as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{text}");
            let _ = f.flush();
        }
        let source = if text.starts_with("[stderr] ") {
            "stderr"
        } else if text.starts_with("[mcpack]") {
            "mcpack"
        } else {
            "game"
        };
        self.events.emit(Event::Log {
            session: self.id.clone(),
            seq,
            t: util::now_millis(),
            level: log_level(&text).into(),
            source: source.into(),
            text,
        });
    }

    /// Log lines with seq >= `since`, optionally filtered, last `tail` of them.
    pub fn logs(&self, since: Option<u64>, tail: Option<usize>, grep: Option<&regex::Regex>) -> Vec<LogLine> {
        let log = self.log.lock().expect("log lock");
        let it = log
            .lines
            .iter()
            .filter(|l| since.is_none_or(|s| l.seq >= s))
            .filter(|l| grep.is_none_or(|g| g.is_match(&l.text)));
        let v: Vec<LogLine> = it.cloned().collect();
        match tail {
            Some(n) if v.len() > n => v[v.len() - n..].to_vec(),
            _ => v,
        }
    }

    pub fn all_log_text(&self) -> Vec<String> {
        self.log.lock().expect("log lock").lines.iter().map(|l| l.text.clone()).collect()
    }

    pub fn log_contains(&self, needle: &str) -> bool {
        self.log.lock().expect("log lock").lines.iter().any(|l| l.text.contains(needle))
    }

    pub fn exit(&self) -> Option<ExitInfo> {
        self.exit_rx.borrow().clone()
    }

    pub fn running(&self) -> bool {
        self.exit().is_none()
    }

    pub fn mark_ready(&self) {
        let mut r = self.ready.lock().expect("ready lock");
        if r.is_none() {
            *r = Some(util::now_millis().saturating_sub(self.started_ms));
            drop(r);
            self.persist();
            self.emit_state("ready");
        }
    }

    pub fn ready_after_ms(&self) -> Option<u64> {
        *self.ready.lock().expect("ready lock")
    }

    /// Request a hard kill.
    pub fn kill(&self) {
        self.stop_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(tx) = self.kill_tx.lock().expect("kill lock").take() {
            let _ = tx.send(());
        }
    }

    pub fn mark_stop_requested(&self) {
        self.stop_requested.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn stop_was_requested(&self) -> bool {
        self.stop_requested.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn set_exit(&self, info: ExitInfo) {
        let state = if info.crashed { "crashed" } else if info.requested { "stopped" } else { "exited" };
        let _ = self.exit_tx.send(Some(info));
        self.persist();
        self.emit_state(state);
    }

    pub fn announce_start(&self) {
        self.persist();
        self.emit_state("starting");
    }

    /// Wait until the process exits (or timeout). Returns the exit info if exited.
    pub async fn wait_exit(&self, timeout: std::time::Duration) -> Option<ExitInfo> {
        let mut rx = self.exit_rx.clone();
        let fut = async {
            loop {
                if let Some(e) = rx.borrow().clone() {
                    return e;
                }
                if rx.changed().await.is_err() {
                    // Sender dropped without exit info; treat as exited.
                    return ExitInfo { code: None, at_ms: util::now_millis(), crashed: false, crash_report: None, requested: true };
                }
            }
        };
        tokio::time::timeout(timeout, fut).await.ok()
    }

    pub fn status(&self) -> SessionStatus {
        let log_lines = self.log.lock().expect("log lock").next;
        SessionStatus {
            id: self.id.clone(),
            pack_dir: self.pack_dir.display().to_string(),
            profile: self.profile.clone(),
            minecraft: self.minecraft.clone(),
            instance_dir: self.instance_dir.display().to_string(),
            pid: self.pid,
            started_ms: self.started_ms,
            running: self.running(),
            exit: self.exit(),
            agent_port: self.agent.as_ref().map(|a| a.port),
            ready_after_ms: self.ready_after_ms(),
            log_lines,
        }
    }
}

/// Pump a child stream into the session log (lossy UTF-8, one entry per line).
pub async fn pump<R: AsyncRead + Unpin>(session: std::sync::Arc<Session>, stream: R, prefix: &'static str) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                session.push_log(if prefix.is_empty() { line } else { format!("{prefix}{line}") });
            }
        }
    }
}
