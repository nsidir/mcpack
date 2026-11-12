//! On-disk registries shared across processes (MCP server, CLI, UI):
//! - `<home>/sessions/<id>.json` + `<id>.log`: every launched game session
//! - `<home>/activity.jsonl`: append-only journal of every tool call
//! - `<home>/packs.json`: known pack directories

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::AgentEndpoint;
use crate::error::{IoContext, Result};
use crate::launch::{ExitInfo, LaunchOptions};
use crate::paths::Paths;
use crate::util;

// ---------------------------------------------------------------- sessions

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub pack_dir: String,
    pub profile: String,
    pub minecraft: String,
    pub instance_dir: String,
    pub pid: Option<u32>,
    pub started_ms: u64,
    pub agent: Option<AgentEndpoint>,
    pub options: LaunchOptions,
    pub log_path: String,
    /// Process id of the mcpack process that owns (supervises) the session.
    pub owner_pid: u32,
    #[serde(default)]
    pub ready_after_ms: Option<u64>,
    #[serde(default)]
    pub exit: Option<ExitInfo>,
}

pub fn sessions_dir(paths: &Paths) -> PathBuf {
    paths.home.join("sessions")
}

pub fn session_log_path(paths: &Paths, id: &str) -> PathBuf {
    sessions_dir(paths).join(format!("{id}.log"))
}

pub fn write_session(paths: &Paths, rec: &SessionRecord) -> Result<()> {
    let p = sessions_dir(paths).join(format!("{}.json", rec.id));
    util::write_atomic(&p, serde_json::to_string_pretty(rec)?.as_bytes())
}

pub fn read_session(paths: &Paths, id: &str) -> Option<SessionRecord> {
    let p = sessions_dir(paths).join(format!("{id}.json"));
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok())
}

pub fn list_sessions(paths: &Paths) -> Vec<SessionRecord> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(sessions_dir(paths)) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                if let Some(r) = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<SessionRecord>(&t).ok()) {
                    out.push(r);
                }
            }
        }
    }
    out.sort_by_key(|r| r.started_ms);
    out
}

/// Remove records (and logs) of finished sessions older than `max_age_ms`.
pub fn prune_sessions(paths: &Paths, max_age_ms: u64) {
    let now = util::now_millis();
    for r in list_sessions(paths) {
        let ended = r.exit.as_ref().map(|e| e.at_ms).or_else(|| (!pid_alive(r.pid)).then_some(r.started_ms));
        if let Some(t) = ended {
            if now.saturating_sub(t) > max_age_ms {
                let _ = std::fs::remove_file(sessions_dir(paths).join(format!("{}.json", r.id)));
                let _ = std::fs::remove_file(session_log_path(paths, &r.id));
            }
        }
    }
}

/// Is a process alive? (best effort, no extra dependencies)
pub fn pid_alive(pid: Option<u32>) -> bool {
    let Some(pid) = pid else { return false };
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")),
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        Path::new(&format!("/proc/{pid}")).exists()
            || std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().is_ok_and(|s| s.success())
    }
}

pub fn kill_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).output();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
}

/// Read lines of a session log file (seq = 0-based line number).
pub fn read_log_file(path: &Path) -> Vec<String> {
    match std::fs::File::open(path) {
        Ok(f) => std::io::BufReader::new(f).lines().map_while(|l| l.ok()).collect(),
        Err(_) => vec![],
    }
}

// ---------------------------------------------------------------- activity

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: String,
    /// unix ms of the start
    pub ts: u64,
    /// mcp | cli | app
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    pub args: serde_json::Value,
    /// running | ok | error
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

pub fn activity_path(paths: &Paths) -> PathBuf {
    paths.home.join("activity.jsonl")
}

pub fn append_activity(paths: &Paths, e: &ActivityEntry) -> Result<()> {
    let p = activity_path(paths);
    util::ensure_parent(&p)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).ctx(&p)?;
    let mut line = serde_json::to_string(e)?;
    line.push('\n');
    f.write_all(line.as_bytes()).ctx(&p)
}

/// Read the journal, merging start/end records by id (latest record wins). Newest first.
pub fn read_activity(paths: &Paths, limit: usize, since_ts: Option<u64>, tool: Option<&str>) -> Vec<ActivityEntry> {
    let p = activity_path(paths);
    let Ok(f) = std::fs::File::open(&p) else { return vec![] };
    let mut by_id: BTreeMap<String, ActivityEntry> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for line in std::io::BufReader::new(f).lines().map_while(|l| l.ok()) {
        if let Ok(e) = serde_json::from_str::<ActivityEntry>(&line) {
            if !by_id.contains_key(&e.id) {
                order.push(e.id.clone());
            }
            by_id.insert(e.id.clone(), e);
        }
    }
    order
        .iter()
        .rev()
        .filter_map(|id| by_id.get(id))
        .filter(|e| since_ts.is_none_or(|s| e.ts > s))
        .filter(|e| tool.is_none_or(|t| e.tool == t))
        .take(limit)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------- packs

pub fn packs_path(paths: &Paths) -> PathBuf {
    paths.home.join("packs.json")
}

pub fn known_packs(paths: &Paths) -> Vec<String> {
    std::fs::read_to_string(packs_path(paths))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default()
}

pub fn remember_pack(paths: &Paths, dir: &Path) {
    let d = dir.display().to_string();
    let mut list = known_packs(paths);
    if list.iter().any(|x| x == &d) {
        return;
    }
    list.push(d);
    let _ = util::write_atomic(&packs_path(paths), serde_json::to_string_pretty(&list).unwrap_or_default().as_bytes());
}

pub fn forget_pack(paths: &Paths, dir: &str) -> bool {
    let mut list = known_packs(paths);
    let before = list.len();
    list.retain(|x| x != dir);
    let _ = util::write_atomic(&packs_path(paths), serde_json::to_string_pretty(&list).unwrap_or_default().as_bytes());
    list.len() != before
}
