//! Push events for in-process consumers (UI): logs, session state, progress, activity, watch.

use serde::Serialize;
use tokio::sync::broadcast;

use crate::launch::SessionStatus;
use crate::registry::ActivityEntry;
use crate::watch::WatchEvent;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Log {
        session: String,
        seq: u64,
        /// unix ms
        t: u64,
        /// info | warn | error | debug | fatal | unknown
        level: String,
        /// game | stderr | mcpack
        source: String,
        text: String,
    },
    Session {
        session: String,
        /// starting | ready | exited | crashed | stopped
        state: String,
        status: SessionStatus,
    },
    Progress {
        #[serde(skip_serializing_if = "Option::is_none")]
        pack: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// resolve | download | java | launch | agent
        phase: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        fraction: Option<f64>,
        detail: String,
    },
    Activity {
        entry: ActivityEntry,
    },
    Watch {
        watch: String,
        event: WatchEvent,
    },
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self { tx }
    }
}

impl EventBus {
    pub fn emit(&self, e: Event) {
        // No subscribers is fine.
        let _ = self.tx.send(e);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn progress(&self, pack: Option<&str>, session: Option<&str>, phase: &str, fraction: Option<f64>, detail: impl Into<String>) {
        self.progress_profile(pack, None, session, phase, fraction, detail)
    }

    pub fn progress_profile(
        &self,
        pack: Option<&str>,
        profile: Option<&str>,
        session: Option<&str>,
        phase: &str,
        fraction: Option<f64>,
        detail: impl Into<String>,
    ) {
        self.emit(Event::Progress {
            pack: pack.map(str::to_string),
            profile: profile.map(str::to_string),
            session: session.map(str::to_string),
            phase: phase.into(),
            fraction,
            detail: detail.into(),
        });
    }
}

/// Classify a log line's level (log4j `[thread/LEVEL]` pattern).
pub fn log_level(line: &str) -> &'static str {
    for (needle, lvl) in [("/ERROR]", "error"), ("/FATAL]", "fatal"), ("/WARN]", "warn"), ("/INFO]", "info"), ("/DEBUG]", "debug")] {
        if line.contains(needle) {
            return lvl;
        }
    }
    if line.contains("Exception") || line.starts_with("\tat ") || line.starts_with("Caused by:") {
        return "error";
    }
    "unknown"
}
