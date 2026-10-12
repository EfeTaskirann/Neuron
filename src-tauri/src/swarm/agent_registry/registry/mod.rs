//! The stateful `SwarmAgentRegistry`: workspace-scoped session +
//! dispatcher lifecycle. See the [parent module docs](super) for the
//! concurrency model.
//!
//! ## Module layout
//!
//! The registry's surface is one struct with a single logical impl,
//! split here by responsibility axis (all blocks remain
//! `impl SwarmAgentRegistry`; the private fields are reachable from
//! every sub-module because they are descendants of this `mod.rs`):
//!
//! - this file — struct definition, constructors (`new`/`with_turn_cap`),
//!   and the read-only diagnostics (`slot_count`/`dispatcher_count`/
//!   `turn_cap`).
//! - [`invoke`] — the `acquire_and_invoke_turn` hot path (lazy spawn +
//!   turn-cap respawn + streaming-event forwarder + status FSM).
//! - [`query`] — `list_status` read-only snapshot.
//! - [`lifecycle`] — `shutdown_workspace`/`shutdown_all` teardown and
//!   the `ensure_dispatcher` lazy-spawn.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};

use crate::swarm::profile::ProfileRegistry;

use super::config::resolve_turn_cap;
use super::slot::AgentSlot;

mod invoke;
mod lifecycle;
mod query;

/// Workspace-scoped session registry. See module docs.
pub struct SwarmAgentRegistry {
    sessions: RwLock<HashMap<(String, String), Arc<Mutex<AgentSlot>>>>,
    profiles: Arc<ProfileRegistry>,
    turn_cap: u32,
    /// W5-02 — per-(workspace, agent) `MailboxAgentDispatcher`s.
    /// Spawned lazily by `ensure_dispatcher` on first dispatch
    /// targeting the agent (or eagerly by tests / IPC). On
    /// `shutdown_all` these are drained BEFORE sessions die so
    /// in-flight invokes can complete via cancel rather than
    /// being torn out from under the dispatcher.
    dispatchers: RwLock<
        HashMap<
            (String, String),
            crate::swarm::agent_dispatcher::MailboxAgentDispatcher,
        >,
    >,
}

impl SwarmAgentRegistry {
    /// Build a fresh registry. Reads `NEURON_SWARM_AGENT_TURN_CAP`
    /// from the environment; non-numeric / zero values fall back to
    /// `DEFAULT_TURN_CAP` with a warn log so a typo isn't silently
    /// ignored.
    pub fn new(profiles: Arc<ProfileRegistry>) -> Self {
        let turn_cap = resolve_turn_cap();
        Self {
            sessions: RwLock::new(HashMap::new()),
            profiles,
            turn_cap,
            dispatchers: RwLock::new(HashMap::new()),
        }
    }

    /// Builder hook for tests — lets the suite pin a small `turn_cap`
    /// without spelunking through env vars.
    #[cfg(test)]
    pub(crate) fn with_turn_cap(
        profiles: Arc<ProfileRegistry>,
        turn_cap: u32,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            profiles,
            turn_cap,
            dispatchers: RwLock::new(HashMap::new()),
        }
    }

    /// Diagnostics: how many slots does the registry hold across all
    /// workspaces? Used by tests + a future telemetry surface.
    #[cfg(test)]
    pub(crate) async fn slot_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Diagnostics: how many dispatchers are registered. Used by the
    /// `registry_ensure_dispatcher_is_idempotent` test.
    #[cfg(test)]
    pub(crate) async fn dispatcher_count(&self) -> usize {
        self.dispatchers.read().await.len()
    }

    /// Diagnostics: the configured turn cap for this registry.
    pub fn turn_cap(&self) -> u32 {
        self.turn_cap
    }
}
