//! Internal per-agent slot held behind the registry's structural lock.

use crate::swarm::persistent_session::PersistentSession;

use super::status::AgentStatus;

/// Inner per-agent slot. The registry holds these behind an
/// `Arc<Mutex<...>>` so each agent's turns serialise without
/// blocking other agents.
pub(super) struct AgentSlot {
    pub(super) session: Option<PersistentSession>,
    pub(super) status: AgentStatus,
    pub(super) turns_taken: u32,
    pub(super) last_activity_ms: Option<i64>,
}

impl AgentSlot {
    pub(super) fn new() -> Self {
        Self {
            session: None,
            status: AgentStatus::NotSpawned,
            turns_taken: 0,
            last_activity_ms: None,
        }
    }
}
