//! `SwarmAgentRegistry` — workspace-scoped lifecycle owner for
//! W4-01's `PersistentSession`s (WP-W4-02).
//!
//! Keyed by `(workspace_id, agent_id)`. Sessions lazy-spawn on first
//! `acquire_and_invoke_turn`; reused across turns until the
//! workspace is shut down (W4-02 §"Lifecycle"). Per-agent status is
//! exposed read-only via `list_status` for the eventual W4-04 grid
//! header.
//!
//! Concurrency model:
//! - Outer `RwLock<HashMap<...>>` guards structural changes
//!   (insert / remove). Reads dominate (status checks, hash lookups
//!   on `acquire`), so the read lock keeps the hot path uncontended.
//! - Per-agent `Mutex<AgentSession>` serialises calls against a
//!   single session — `PersistentSession` is not `Sync`, and at most
//!   one `invoke_turn` against the same child can be in flight at a
//!   time (W4-01 contract).
//!
//! Out of scope (per WP §"Out of scope"): event channel emission
//! (W4-03) / 3×3 grid UI (W4-04) / `neuron_help` parser (W4-05) /
//! FSM persistent-transport adapter (W4-06).
//!
//! ## Module layout
//!
//! - [`status`] — `AgentStatus` + `AgentStatusRow` read-only wire
//!   shapes surfaced by `list_status`.
//! - [`event`] — `SwarmAgentEvent` per-agent channel payload (W4-03)
//!   + the `agent_event_channel` name builder.
//! - [`config`] — `NEURON_SWARM_AGENT_TURN_CAP` resolution.
//! - [`slot`] — internal per-agent `AgentSlot`.
//! - [`registry`] — the stateful `SwarmAgentRegistry` itself.

mod config;
mod event;
mod registry;
mod slot;
mod status;
#[cfg(test)]
mod tests;

pub use event::{agent_event_channel, SwarmAgentEvent};
pub use registry::SwarmAgentRegistry;
pub use status::{AgentStatus, AgentStatusRow};
