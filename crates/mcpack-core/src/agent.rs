//! In-game automation agent: bundled jars + JSON-RPC client.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::error::{Error, Result};
use crate::util;

include!(concat!(env!("OUT_DIR"), "/agent_jars.rs"));

pub const AGENT_FILENAME: &str = "mcpack-agent.jar";

/// Minecraft versions with a bundled agent.
pub fn supported_versions() -> Vec<&'static str> {
    AGENT_JARS.iter().map(|(v, _)| *v).collect()
}

/// Agent jar for a Minecraft version, materialized under `agent_dir`. Override with
/// `$MCPACK_AGENT_DIR/mcpack-agent-<mc>.jar` for agent development.
pub fn jar_for(agent_dir: &Path, mc: &str) -> Result<Option<PathBuf>> {
    if let Some(dir) = std::env::var_os("MCPACK_AGENT_DIR") {
        let p = PathBuf::from(dir).join(format!("mcpack-agent-{mc}.jar"));
        if p.exists() {
            return Ok(Some(p));
        }
    }
    let Some((_, bytes)) = AGENT_JARS.iter().find(|(v, _)| *v == mc) else {
        return Ok(None);
    };
    let sha = util::sha1_bytes(bytes);
    let p = agent_dir.join(format!("mcpack-agent-{mc}-{}.jar", &sha[..8]));
    if !p.exists() {
        util::write_atomic(&p, bytes)?;
    }
    Ok(Some(p))
}

/// Where a running game's agent listens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEndpoint {
    pub port: u16,
    pub token: String,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl AgentEndpoint {
    pub fn new() -> Result<Self> {
        // Grab a free port from the OS.
        let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| Error::io("127.0.0.1:0", e))?;
        let port = l.local_addr().map_err(|e| Error::io("127.0.0.1", e))?.port();
        drop(l);
        use rand::Rng;
        let token: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();
        Ok(Self { port, token })
    }

    pub fn jvm_args(&self) -> Vec<String> {
        vec![format!("-Dmcpack.agent.port={}", self.port), format!("-Dmcpack.agent.token={}", self.token)]
    }

    /// Call an agent method. Fails fast if the agent is not listening yet.
    pub async fn call(&self, method: &str, params: serde_json::Value, timeout: Duration) -> Result<serde_json::Value> {
        let fut = async {
            let mut stream = TcpStream::connect(("127.0.0.1", self.port))
                .await
                .map_err(|e| Error::Agent(format!("agent not reachable on port {}: {e}", self.port)))?;
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let req = serde_json::json!({"id": id, "token": self.token, "method": method, "params": params});
            let mut line = serde_json::to_string(&req)?;
            line.push('\n');
            stream.write_all(line.as_bytes()).await.map_err(|e| Error::Agent(e.to_string()))?;
            let (r, _w) = stream.split();
            let mut reader = BufReader::new(r);
            let mut resp = String::new();
            reader.read_line(&mut resp).await.map_err(|e| Error::Agent(e.to_string()))?;
            if resp.trim().is_empty() {
                return Err(Error::Agent("agent closed the connection".into()));
            }
            let v: serde_json::Value = serde_json::from_str(resp.trim())?;
            if let Some(err) = v.get("error") {
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown agent error");
                return Err(Error::Agent(msg.to_string()));
            }
            Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
        };
        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| Error::Agent(format!("agent call '{method}' timed out after {}s", timeout.as_secs())))?
    }
}
