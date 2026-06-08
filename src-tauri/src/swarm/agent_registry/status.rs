//! Read-only status wire shapes for `swarm:agents:list_status`.

use serde::{Deserialize, Serialize};
use specta::Type;

/// Per-agent status visible to the UI (eventually rendered by W4-04
/// grid header pills). Snake_case wire form per Charter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Default for every (workspace, agent) pair before the first
    /// lazy-spawn fires. The grid renders these as muted "—" pills.
    NotSpawned,
    /// Spawning in flight. Brief — visible only across one
    /// `acquire` window. Flips to `Idle` once the session is in the
    /// registry.
    Spawning,
    /// Session ready, no turn in flight.
    Idle,
    /// `invoke_turn` is in flight against this session.
    Running,
    /// Specialist emitted a `neuron_help` block (W4-05 will set
    /// this; W4-02 never emits it but the variant is present so
    /// W4-05 doesn't have to widen the type).
    WaitingOnCoordinator,
    /// The session crashed (subprocess died unrecoverably). Will
    /// be respawned on next `acquire_and_invoke_turn`. Distinct
    /// from `NotSpawned` so the grid can surface a "this agent had
    /// trouble" indicator separate from "this agent never ran".
    Crashed,
}

/// Wire shape for `swarm:agents:list_status`. Trimmed to what the
/// UI actually renders; richer per-agent diagnostics can be added
/// in a follow-up without breaking this surface.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusRow {
    pub workspace_id: String,
    pub agent_id: String,
    pub status: AgentStatus,
    /// `0` for un-touched agents — `NotSpawned` rows always have
    /// `turns_taken: 0`. After respawn under the turn-cap, this
    /// counter resets.
    pub turns_taken: u32,
    /// Wall-clock ms since UNIX epoch of the most recent
    /// state-changing event (spawn, turn start, turn end, crash).
    /// `None` when `status == NotSpawned`.
    pub last_activity_ms: Option<i64>,
}
