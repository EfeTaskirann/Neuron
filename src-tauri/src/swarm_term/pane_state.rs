//! Each pane's readiness as the session's kickoff loop sees it (C3) —
//! for the UI's "n/9 hazır" badge and per-pane chips, and for gating
//! the chat bar until the orchestrator can take a task. The snapshot
//! rides on the session handle; every change is also emitted as a
//! `swarm-term:pane-state` event.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PaneReadiness {
    /// Spawned; claude is booting or its screen is still settling.
    Starting,
    /// On a screen only the user can answer (login, first-run).
    Blocked,
    /// The kickoff went out; waiting for `@<id> READY`.
    KickoffSent,
    /// Answered the kickoff: a delivery target.
    Ready,
    /// No answer in time. Still watched — a late answer makes it ready.
    NotReady,
    /// The pane's process ended.
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSwarmPaneState {
    pub agent_id: String,
    pub readiness: PaneReadiness,
    /// Why, for the states the user may need to act on (Turkish).
    pub detail: Option<String>,
}

/// The readiness of every pane of one session.
#[derive(Debug, Default)]
pub(crate) struct PaneStates {
    inner: Mutex<BTreeMap<String, (PaneReadiness, Option<String>)>>,
}

impl PaneStates {
    /// Every agent starts as [`PaneReadiness::Starting`].
    pub(crate) fn new<'a>(agents: impl IntoIterator<Item = &'a str>) -> Self {
        let map = agents
            .into_iter()
            .map(|a| (a.to_string(), (PaneReadiness::Starting, None)))
            .collect();
        Self { inner: Mutex::new(map) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, (PaneReadiness, Option<String>)>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record `agent`'s state; true when it changed (worth an event).
    pub(crate) fn set(&self, agent: &str, readiness: PaneReadiness, detail: Option<String>) -> bool {
        let mut g = self.lock();
        let next = (readiness, detail);
        if g.get(agent) == Some(&next) {
            return false;
        }
        g.insert(agent.to_string(), next);
        true
    }

    #[cfg(test)]
    pub(crate) fn get(&self, agent: &str) -> Option<PaneReadiness> {
        self.lock().get(agent).map(|(r, _)| *r)
    }

    /// Sorted by agent id.
    pub(crate) fn snapshot(&self) -> Vec<TerminalSwarmPaneState> {
        self.lock()
            .iter()
            .map(|(agent, (readiness, detail))| TerminalSwarmPaneState {
                agent_id: agent.clone(),
                readiness: *readiness,
                detail: detail.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_start_starting_and_report_only_real_changes() {
        let s = PaneStates::new(["scout", "orchestrator"]);
        assert_eq!(s.get("scout"), Some(PaneReadiness::Starting));
        assert!(!s.set("scout", PaneReadiness::Starting, None), "no change");
        assert!(s.set("scout", PaneReadiness::Blocked, Some("giriş".into())));
        assert!(!s.set("scout", PaneReadiness::Blocked, Some("giriş".into())));
        assert!(s.set("scout", PaneReadiness::Blocked, Some("başka".into())), "detail counts");
        let snap = s.snapshot();
        assert_eq!(snap.iter().map(|p| p.agent_id.as_str()).collect::<Vec<_>>(), ["orchestrator", "scout"]);
        assert_eq!(snap[1].readiness, PaneReadiness::Blocked);
        assert_eq!(serde_json::to_value(PaneReadiness::KickoffSent).unwrap(), "kickoff_sent");
        assert_eq!(serde_json::to_value(PaneReadiness::NotReady).unwrap(), "not_ready");
    }
}
