//! Launching the client.

pub mod args;
pub mod session;

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use tokio::sync::oneshot;

use crate::agent::AgentEndpoint;
use crate::error::{Error, Result};
use crate::install::GameInstall;
use crate::util;
pub use session::{ExitInfo, LaunchOptions, LogLine, Session, SessionStatus};

/// Spawn the game and return a supervised session.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    install: &GameInstall,
    params: &args::LaunchParams,
    platform: &crate::meta::Platform,
    pack_dir: &Path,
    profile: &str,
    agent: Option<AgentEndpoint>,
    options: LaunchOptions,
    minecraft: &str,
    events: crate::events::EventBus,
    paths: crate::paths::Paths,
    hotswap: Option<crate::hotswap::HotswapEndpoint>,
) -> Result<Arc<Session>> {
    let mut params = params.clone();
    if let Some(a) = &agent {
        params.extra_jvm.extend(a.jvm_args());
    }
    if let Some(h) = &hotswap {
        params.extra_jvm.extend(h.jvm_args());
    }
    let cmd = args::build(install, &params, platform);
    util::ensure_dir(&params.game_dir)?;

    let mut command = tokio::process::Command::new(&cmd.java);
    command
        .args(&cmd.args)
        .current_dir(&params.game_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: no console window flashing up for the JVM.
        command.creation_flags(0x0800_0000);
    }
    tracing::debug!(java = %cmd.java.display(), args = ?cmd.args, "spawning game");
    let mut child = command
        .spawn()
        .map_err(|e| Error::Launch(format!("failed to start {}: {e}", cmd.java.display())))?;

    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    let id = format!("s-{}", &util::sha1_bytes(format!("{}{}", util::now_millis(), rand_u64()).as_bytes())[..8]);
    let session = Arc::new(Session::new(
        id,
        pack_dir.to_path_buf(),
        profile.to_string(),
        params.game_dir.clone(),
        child.id(),
        agent,
        options,
        minecraft.to_string(),
        kill_tx,
        events,
        paths,
        hotswap.clone(),
    ));
    session.announce_start();
    session.push_log(format!("[mcpack] launching {} with {}", minecraft, cmd.java.display()));
    if let Some(h) = &hotswap {
        session.push_log(format!(
            "[mcpack] hot-swap on: debug agent at 127.0.0.1:{}{}",
            h.port,
            if h.enhanced { " (JetBrains Runtime: enhanced class redefinition)" } else { " (method-body changes)" }
        ));
    }

    if let Some(out) = child.stdout.take() {
        tokio::spawn(session::pump(session.clone(), out, ""));
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(session::pump(session.clone(), err, "[stderr] "));
    }

    let s = session.clone();
    let game_dir = params.game_dir.clone();
    tokio::spawn(async move {
        let status = tokio::select! {
            st = child.wait() => st,
            _ = kill_rx => {
                let _ = child.kill().await;
                child.wait().await
            }
        };
        let code = status.ok().and_then(|s| s.code());
        let crash = crate::diagnose::newest_crash_report(&game_dir, Some(s.started_ms));
        let requested = s.stop_was_requested();
        let crashed = crash.is_some() || (!requested && code.is_some_and(|c| c != 0));
        s.push_log(format!("[mcpack] game exited with code {code:?}"));
        s.set_exit(ExitInfo {
            code,
            at_ms: util::now_millis(),
            crashed,
            crash_report: crash.map(|p| p.display().to_string()),
            requested,
        });
    });
    Ok(session)
}

fn rand_u64() -> u64 {
    use rand::Rng;
    rand::thread_rng().gen()
}
