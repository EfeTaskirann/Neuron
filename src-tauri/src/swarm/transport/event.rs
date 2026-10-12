//! Transport data types: the parsed stream-json event enum and the
//! public one-shot invoke result.
//!
//! Split out of the former single-file `transport.rs` (DEEP refactor)
//! so the pure types live apart from the spawn/drive side
//! ([`super::subprocess`]) and the line classifier
//! ([`super::classify`]).

use serde::{Deserialize, Serialize};
use specta::Type;

/// One classified line from the `claude` stream-json output. Mid-loop
/// state (running assistant text, captured `session_id`) lives in the
/// caller's accumulator; this enum is the parser's *event* output.
///
/// Public-within-crate so `tests` and future state-machine consumers
/// can drive the parser independently of the spawn side.
///
/// As of W4-03, a single `assistant` line can produce multiple events
/// (one `AssistantDelta` per text block PLUS one `ToolUse` per
/// tool_use block) — `classify_event` returns `Vec<StreamEvent>`
/// instead of a scalar.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StreamEvent {
    /// `system.init` — carries the subprocess session id.
    SystemInit {
        session_id: String,
    },
    /// `assistant` — one text delta to append to the running buffer.
    AssistantDelta {
        text: String,
    },
    /// W4-03 — `assistant` content carries a `tool_use` block.
    /// Surfaced as a separate event so the live-feed UI can render
    /// "Scout is reading X" while the model continues streaming text.
    ToolUse {
        name: String,
        /// One-line truncation of the tool input (capped ~120 chars).
        /// Full input lives in the eventual `ResultSuccess`.
        input_summary: String,
    },
    /// `result.success` — final answer + accounting; reader stops.
    ResultSuccess {
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    /// `result.error` — the model produced an error result; reader
    /// bails with `AppError::SwarmInvoke`.
    ResultError {
        reason: String,
    },
    /// Recognised but uninteresting (`ping`, `system.compact_boundary`,
    /// etc.) — the caller silently keeps reading. Unused as of W4-03
    /// (uninteresting events now land as an empty `Vec<StreamEvent>`
    /// from `classify_event`); the variant is retained for forward
    /// compatibility — a future event type that we want to log but
    /// not act on can be classified as `Other` rather than threading
    /// a separate enum through.
    #[allow(dead_code)]
    Other,
}

/// Output of one successful `SubprocessTransport::invoke`.
///
/// `total_cost_usd` is what `claude` reports in the `result` event;
/// `turn_count` reflects the number of model turns the child consumed
/// before finishing (≤ `Profile.max_turns`).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct InvokeResult {
    pub session_id: String,
    pub assistant_text: String,
    pub total_cost_usd: f64,
    pub turn_count: u32,
}
