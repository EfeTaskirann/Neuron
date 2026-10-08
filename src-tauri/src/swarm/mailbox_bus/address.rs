//! Canonical pane addresses used as the `from_pane` / `to_pane`
//! fields on mailbox envelopes.
//!
//! These strings are the swarm's routing vocabulary: every
//! `emit_typed` call names a sender and a recipient pane. They were
//! previously inlined as ~20 bare `"agent:*"` string literals across
//! the dispatcher, brain, and command layers. Centralising them here
//! gives the vocabulary a single source of truth — a typo can no
//! longer silently misroute an event — without changing any wire
//! value: each constant holds the exact same bytes as the literal it
//! replaces, so the persisted `from_pane`/`to_pane` columns and the
//! emitted envelope shape are byte-identical.

/// The Coordinator brain's pane. Sends dispatch/finish events and
/// receives every specialist `AgentResult` / `AgentHelpRequest`.
pub const COORDINATOR_PANE: &str = "agent:coordinator";

/// The end-user's pane. Origin of a job and the sink the Coordinator
/// posts `JobFinished` / cancel events back to.
pub const USER_PANE: &str = "agent:user";

/// The pane address for a specialist agent by id: `agent:{agent_id}`.
pub fn agent_pane(agent_id: &str) -> String {
    format!("agent:{agent_id}")
}
