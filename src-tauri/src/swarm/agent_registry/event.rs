//! W4-03 per-(workspace, agent) event channel payload + name builder.

use serde::{Deserialize, Serialize};
use specta::Type;

/// W4-03 — payload of the per-(workspace, agent) event channel
/// `swarm:agent:{workspace_id}:{agent_id}:event`. The W4-04 grid
/// pane subscribes to one such channel per agent and renders a live
/// transcript as events arrive.
///
/// Variants split into two groups:
/// - **Bookend** (Spawned / TurnStarted / Result / Idle / Crashed):
///   emitted by the registry around `invoke_turn` calls. Drive
///   the pane status pill + cost-so-far counter.
/// - **Streaming** (AssistantText / ToolUse / HelpRequest): emitted
///   from inside `invoke_turn` via the `TurnStreamEvent` mpsc.
///   Drive the live transcript renderer. `HelpRequest` is reserved
///   here for W4-05 — the registry doesn't emit it in W4-03.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmAgentEvent {
    /// `PersistentSession::spawn` succeeded; the registry slot just
    /// flipped from `NotSpawned` → `Idle`. Carries the profile id
    /// so the pane can render the persona name without a separate
    /// IPC.
    Spawned { profile_id: String },
    /// `acquire_and_invoke_turn` is about to write a user message.
    /// `turn_index` mirrors the registry's `turns_taken` BEFORE the
    /// new turn (first turn is `turn_index: 0`).
    TurnStarted { turn_index: u32 },
    /// Streaming text delta from claude. May fire many times per
    /// turn; the W4-04 pane appends to a per-turn buffer.
    AssistantText { delta: String },
    /// Claude is using a tool. `name` is the tool name (Read, Edit,
    /// Glob, etc.); `input_summary` is a one-line truncation of the
    /// tool input (capped via `TOOL_USE_INPUT_SUMMARY_CAP` in
    /// `transport::classify`). The W4-04 pane shows "Scout is reading
    /// SwarmJobList.tsx" badges.
    ToolUse { name: String, input_summary: String },
    /// Turn finished cleanly. Final assistant text + accounting.
    Result {
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    /// Reserved for W4-05 — specialist emitted a `neuron_help`
    /// JSON block. W4-03 never emits this; W4-05 wires the parser.
    HelpRequest { reason: String, question: String },
    /// Turn ended (success or cancel — not crash); slot is back to
    /// `Idle`.
    Idle,
    /// Session crashed unrecoverably. Slot is `Crashed`; next
    /// `acquire` will respawn.
    Crashed { error: String },
}

/// Build the per-(workspace, agent) event channel name. Centralised
/// so the frontend hook + the backend emit + tests all agree on the
/// exact shape.
pub fn agent_event_channel(workspace_id: &str, agent_id: &str) -> String {
    format!("swarm:agent:{workspace_id}:{agent_id}:event")
}
