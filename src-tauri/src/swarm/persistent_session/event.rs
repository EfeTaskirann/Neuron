//! `TurnStreamEvent` — the local streaming event handed off to W4-03's
//! per-agent event channel. See the type doc for why it is deliberately
//! separate from `crate::swarm::SwarmAgentEvent`.

/// Streaming event handed off to W4-03's per-agent event channel.
/// Mirrors `crate::swarm::SwarmAgentEvent` minus the bookend variants
/// (Spawned / TurnStarted / Result / Idle / Crashed) which the
/// registry emits on its own. This local-to-the-module enum is the
/// hot-path payload the read loop sends; the registry forwarder
/// re-wraps each one into a `SwarmAgentEvent` before emitting on the
/// Tauri channel.
///
/// Why a separate enum instead of `SwarmAgentEvent` directly:
/// `persistent_session` deliberately doesn't depend on the
/// `agent_registry` module (the dep would cycle on the registry's use
/// of `PersistentSession`). Keeping a thin local enum + lifting at the
/// registry boundary keeps the dep graph acyclic.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnStreamEvent {
    AssistantText { delta: String },
    ToolUse { name: String, input_summary: String },
}
