//! Automated client testing: readiness detection, smoke tests, world control, screenshots.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::diagnose::Diagnosis;
use crate::error::{Error, Result};
use crate::launch::{LaunchOptions, Session};
use crate::{Launcher, LaunchResult};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyOutcome {
    /// Title screen reached (or in world when a world was requested).
    Ready,
    /// The process exited before becoming ready.
    Exited,
    Timeout,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadyResult {
    pub outcome: ReadyOutcome,
    pub elapsed_ms: u64,
    /// Last agent status (if the agent is available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<serde_json::Value>,
    /// How readiness was detected: `agent` or `log`.
    pub detected_by: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorldOptions {
    /// World folder name (default `mcpack-test`).
    #[serde(default)]
    pub name: Option<String>,
    /// Create it if missing (default true).
    #[serde(default)]
    pub create: Option<bool>,
    #[serde(default)]
    pub flat: bool,
    #[serde(default)]
    pub seed: Option<i64>,
    /// survival | creative | adventure | spectator (default creative)
    #[serde(default)]
    pub game_mode: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SmokeOptions {
    #[serde(default)]
    pub profile: Option<String>,
    /// Overall timeout in seconds (default 300).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Leave the game running after the test (default false).
    #[serde(default)]
    pub keep_running: bool,
    /// Also enter a world after reaching the title screen.
    #[serde(default)]
    pub world: Option<WorldOptions>,
    /// Commands to run in the world (requires `world`), outputs are returned.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Take a screenshot at the end.
    #[serde(default)]
    pub screenshot: bool,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandOutput {
    pub command: String,
    pub output: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SmokeResult {
    /// passed | crashed | timeout | launch_failed | world_failed
    pub outcome: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_menu_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_world_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<Diagnosis>,
    pub log_tail: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub still_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_ready(status: &serde_json::Value, want_world: bool) -> bool {
    let b = |k: &str| status.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    if want_world {
        b("in_world") && !b("loading")
    } else {
        b("title_screen") && !b("loading")
    }
}

impl Launcher {
    /// Wait until the client reaches the title screen (or a world when `want_world`).
    pub async fn wait_ready(&self, id: &str, timeout: Duration, want_world: bool) -> Result<ReadyResult> {
        match self.session_ref(id)? {
            crate::SessionRef::Local(s) => Ok(wait_ready_session(&s, timeout, want_world).await),
            crate::SessionRef::Remote(rec) => {
                let log_path = std::path::PathBuf::from(&rec.log_path);
                let pid = rec.pid;
                let probe = SessionProbe {
                    running: &|| crate::registry::pid_alive(pid),
                    log_contains: &|n: &str| crate::registry::read_log_file(&log_path).iter().any(|l| l.contains(n)),
                    mark_ready: &|| {},
                };
                Ok(wait_ready_probe(&probe, rec.agent.as_ref(), timeout, want_world).await)
            }
        }
    }

    /// Launch, wait for the title screen (and optionally a world), run commands, report.
    pub async fn smoke_test(&self, dir: &Path, opts: SmokeOptions) -> Result<SmokeResult> {
        let timeout = Duration::from_secs(opts.timeout_secs.unwrap_or(300));
        let started = Instant::now();
        let profile = opts.profile.clone().unwrap_or_else(|| crate::pack::DEFAULT_PROFILE.to_string());
        let mut result = SmokeResult {
            outcome: "launch_failed".into(),
            passed: false,
            session: None,
            time_to_menu_ms: None,
            time_to_world_ms: None,
            status: None,
            commands: vec![],
            screenshot: None,
            diagnosis: None,
            log_tail: vec![],
            warnings: vec![],
            still_running: false,
            error: None,
        };
        let launch = self
            .launch(
                dir,
                LaunchOptions { profile: Some(profile.clone()), agent: Some(true), username: opts.username.clone(), ..Default::default() },
            )
            .await;
        let LaunchResult { session, sync, .. } = match launch {
            Ok(l) => l,
            Err(e) => {
                result.error = Some(e.to_string());
                return Ok(result);
            }
        };
        result.warnings = sync.warnings.clone();
        let s = self.session(&session.id)?;
        result.session = Some(s.id.clone());

        let ready = wait_ready_session(&s, timeout, false).await;
        result.status = ready.status.clone();
        match ready.outcome {
            ReadyOutcome::Ready => {
                result.time_to_menu_ms = Some(ready.elapsed_ms);
                result.outcome = "passed".into();
                result.passed = true;
            }
            ReadyOutcome::Exited => result.outcome = "crashed".into(),
            ReadyOutcome::Timeout => result.outcome = "timeout".into(),
        }

        if result.passed {
            if let Some(w) = &opts.world {
                let remaining = timeout.saturating_sub(started.elapsed()).max(Duration::from_secs(60));
                match self.enter_world(&s, w, remaining).await {
                    Ok(st) => {
                        result.time_to_world_ms = Some(started.elapsed().as_millis() as u64);
                        result.status = Some(st);
                        for c in &opts.commands {
                            match self.agent_call(&s.id, "command", serde_json::json!({"command": c}), Duration::from_secs(60)).await {
                                Ok(v) => result.commands.push(CommandOutput {
                                    command: c.clone(),
                                    output: v
                                        .get("output")
                                        .and_then(|o| o.as_array())
                                        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                                        .unwrap_or_default(),
                                    error: None,
                                }),
                                Err(e) => result.commands.push(CommandOutput { command: c.clone(), output: vec![], error: Some(e.to_string()) }),
                            }
                        }
                    }
                    Err(e) => {
                        result.passed = false;
                        result.outcome = if s.running() { "world_failed".into() } else { "crashed".into() };
                        result.error = Some(e.to_string());
                    }
                }
            }
            if opts.screenshot && s.running() {
                match self.screenshot(&s.id).await {
                    Ok(p) => result.screenshot = Some(p),
                    Err(e) => result.warnings.push(format!("screenshot failed: {e}")),
                }
            }
        }

        if !result.passed {
            // Give the process a moment to flush crash output.
            if s.running() && result.outcome == "crashed" {
                s.wait_exit(Duration::from_secs(5)).await;
            }
            result.diagnosis = self.diagnose(dir, &profile, Some(&s.id)).ok();
        }
        result.log_tail = s.logs(None, Some(40), None).into_iter().map(|l| l.text).collect();
        if !opts.keep_running && s.running() {
            let _ = self.stop(&s.id, Duration::from_secs(20)).await;
        }
        result.still_running = s.running();
        Ok(result)
    }

    async fn enter_world(&self, s: &Arc<Session>, w: &WorldOptions, timeout: Duration) -> Result<serde_json::Value> {
        self.enter_world_by_id(&s.id, &s.instance_dir, w, timeout).await
    }

    async fn enter_world_by_id(&self, id: &str, instance_dir: &Path, w: &WorldOptions, timeout: Duration) -> Result<serde_json::Value> {
        let name = w.name.clone().unwrap_or_else(|| "mcpack-test".into());
        let level_id = crate::util::safe_component(&name);
        let exists = instance_dir.join("saves").join(&level_id).join("level.dat").exists();
        if exists {
            self.agent_call(id, "join_world", serde_json::json!({"level_id": level_id}), Duration::from_secs(10)).await?;
        } else if w.create.unwrap_or(true) {
            self.agent_call(
                id,
                "create_world",
                serde_json::json!({
                    "level_id": level_id,
                    "name": name,
                    "flat": w.flat,
                    "seed": w.seed,
                    "game_mode": w.game_mode.clone().unwrap_or_else(|| "creative".into()),
                    "cheats": true,
                }),
                Duration::from_secs(10),
            )
            .await?;
        } else {
            return Err(Error::NotFound(format!("world '{level_id}' does not exist")));
        }
        let r = self.wait_ready(id, timeout, true).await?;
        match r.outcome {
            ReadyOutcome::Ready => Ok(r.status.unwrap_or_default()),
            ReadyOutcome::Exited => Err(Error::Launch("game exited while entering the world".into())),
            ReadyOutcome::Timeout => Err(Error::Launch("timed out entering the world".into())),
        }
    }

    /// Create (or join, if it exists) a singleplayer world and wait until in-game.
    pub async fn world_enter(&self, id: &str, opts: WorldOptions, timeout: Duration) -> Result<serde_json::Value> {
        let inst = std::path::PathBuf::from(self.session_status(id)?.instance_dir);
        self.enter_world_by_id(id, &inst, &opts, timeout).await
    }

    /// Leave the current world and wait for the title screen.
    pub async fn world_leave(&self, id: &str, timeout: Duration) -> Result<serde_json::Value> {
        self.agent_call(id, "leave_world", serde_json::json!({}), Duration::from_secs(10)).await?;
        let r = self.wait_ready(id, timeout, false).await?;
        match r.outcome {
            ReadyOutcome::Ready => Ok(r.status.unwrap_or_default()),
            _ => Err(Error::Agent("did not return to the title screen".into())),
        }
    }

    /// Take a screenshot; returns the PNG path.
    pub async fn screenshot(&self, id: &str) -> Result<String> {
        let v = self.agent_call(id, "screenshot", serde_json::json!({}), Duration::from_secs(30)).await?;
        v.get("path")
            .and_then(|p| p.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::Agent("screenshot returned no path".into()))
    }

    /// Run a server command in the current singleplayer world and return its output lines.
    pub async fn game_command(&self, id: &str, command: &str) -> Result<serde_json::Value> {
        self.agent_call(id, "command", serde_json::json!({"command": command}), Duration::from_secs(60)).await
    }
}

pub(crate) async fn wait_ready_session(s: &Arc<Session>, timeout: Duration, want_world: bool) -> ReadyResult {
    let probe = SessionProbe { running: &|| s.running(), log_contains: &|n: &str| s.log_contains(n), mark_ready: &|| s.mark_ready() };
    wait_ready_probe(&probe, s.agent.as_ref(), timeout, want_world).await
}

/// Abstracts over local sessions and sessions owned by another process.
pub(crate) struct SessionProbe<'a> {
    pub running: &'a (dyn Fn() -> bool + Sync),
    pub log_contains: &'a (dyn Fn(&str) -> bool + Sync),
    pub mark_ready: &'a (dyn Fn() + Sync),
}

pub(crate) async fn wait_ready_probe(
    s: &SessionProbe<'_>,
    agent: Option<&crate::agent::AgentEndpoint>,
    timeout: Duration,
    want_world: bool,
) -> ReadyResult {
    let start = Instant::now();
    let mut last_status = None;
    let mut log_ready_at: Option<Instant> = None;
    loop {
        let elapsed = start.elapsed();
        if !(s.running)() {
            return ReadyResult { outcome: ReadyOutcome::Exited, elapsed_ms: elapsed.as_millis() as u64, status: last_status, detected_by: "exit".into() };
        }
        if elapsed >= timeout {
            return ReadyResult { outcome: ReadyOutcome::Timeout, elapsed_ms: elapsed.as_millis() as u64, status: last_status, detected_by: "timeout".into() };
        }
        if let Some(a) = agent {
            if let Ok(st) = a.call("status", serde_json::json!({}), Duration::from_secs(3)).await {
                let ready = is_ready(&st, want_world);
                last_status = Some(st);
                if ready {
                    if !want_world {
                        (s.mark_ready)();
                    }
                    return ReadyResult {
                        outcome: ReadyOutcome::Ready,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        status: last_status,
                        detected_by: "agent".into(),
                    };
                }
            }
        } else if !want_world {
            // No agent: the sound engine starting is the last step before the title screen.
            if log_ready_at.is_none() && (s.log_contains)("Sound engine started") {
                log_ready_at = Some(Instant::now());
            }
            if log_ready_at.is_some_and(|t| t.elapsed() >= Duration::from_secs(3)) {
                (s.mark_ready)();
                return ReadyResult {
                    outcome: ReadyOutcome::Ready,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    status: None,
                    detected_by: "log".into(),
                };
            }
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}
