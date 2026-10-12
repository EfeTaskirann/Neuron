//! `MailboxEvent` — the typed payload variants persisted to the SQL
//! `kind` / `payload_json` columns and emitted on the JSON IPC
//! bindings.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::AppError;

/// Structured event-bus payload. Discriminated by `kind` field on
/// the wire (snake_case). The variant body carries the typed
/// payload; the SQL row's `payload_json` column persists the same
/// JSON verbatim so a process restart can rebuild events from
/// SQLite without losing fidelity.
///
/// Matches the W5-overview event table:
/// `task_dispatch / agent_result / agent_help_request /
/// coordinator_help_outcome / job_started / job_finished /
/// job_cancel / note`.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailboxEvent {
    /// W5-03: Coordinator brain dispatches a task to a specific
    /// agent. `target` is `agent:<id>` per the W4-07 namespacing.
    /// `prompt` is the user-message fed into the agent's session.
    /// `with_help_loop` toggles the W4-05 help-loop on the dispatch;
    /// defaults to true for builders/scout/planner, false for
    /// reviewers/tester whose persona contracts forbid help blocks.
    TaskDispatch {
        job_id: String,
        target: String,
        prompt: String,
        with_help_loop: bool,
    },
    /// W5-02: agent emitted a result for a dispatch. The
    /// originating `TaskDispatch` row's `id` is carried in the
    /// envelope's `parent_id` (NOT the variant body) so a single
    /// reply-to chain stays uniform across all variants.
    AgentResult {
        job_id: String,
        agent_id: String,
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    /// W5-02: agent emitted a `neuron_help` block via W4-05's
    /// parser. The W5-03 brain reads this and replies with a
    /// `CoordinatorHelpOutcome`.
    AgentHelpRequest {
        job_id: String,
        agent_id: String,
        reason: String,
        question: String,
    },
    /// W5-03: Coordinator's response to a help request. The
    /// `outcome_json` payload is a JSON-serialised
    /// `swarm::help_request::CoordinatorHelpOutcome` (action +
    /// answer / followup / user_question fields, depending on
    /// variant). Stored as `String` here so the bus stays
    /// decoupled from `swarm::help_request` (which depends on
    /// `swarm::agent_registry` — would create a cycle), and so
    /// the type implements `specta::Type` cleanly. Consumers
    /// parse via `serde_json::from_str`.
    CoordinatorHelpOutcome {
        job_id: String,
        target_agent_id: String,
        outcome_json: String,
    },
    /// W5-03: job lifecycle start. Emitted once per job by the
    /// `swarm:run_job_v2` IPC; CoordinatorBrain subscribes and
    /// drives the dispatch loop.
    JobStarted {
        job_id: String,
        workspace_id: String,
        goal: String,
    },
    /// W5-03: job lifecycle finish. Emitted by CoordinatorBrain
    /// when the brain returns a `finish` action. `outcome` is
    /// `"done" | "failed"`.
    JobFinished {
        job_id: String,
        outcome: String,
        summary: String,
    },
    /// W5-05: cancel signal. CoordinatorBrain + agent dispatchers
    /// subscribe; in-flight turns truncate.
    JobCancel {
        job_id: String,
    },
    /// Legacy free-form note. The default kind for the back-compat
    /// `mailbox_emit` IPC, which keeps emitting `kind='note'`
    /// implicitly via the migration 0010 column default.
    Note,
}

impl MailboxEvent {
    /// SQL `kind` string for this variant. Stable; matches both
    /// the migration's column values AND the JSON wire-form `kind`
    /// tag, so SQL filters and JSON deserialize agree byte-for-byte.
    pub fn kind_str(&self) -> &'static str {
        match self {
            MailboxEvent::TaskDispatch { .. } => "task_dispatch",
            MailboxEvent::AgentResult { .. } => "agent_result",
            MailboxEvent::AgentHelpRequest { .. } => "agent_help_request",
            MailboxEvent::CoordinatorHelpOutcome { .. } => {
                "coordinator_help_outcome"
            }
            MailboxEvent::JobStarted { .. } => "job_started",
            MailboxEvent::JobFinished { .. } => "job_finished",
            MailboxEvent::JobCancel { .. } => "job_cancel",
            MailboxEvent::Note => "note",
        }
    }

    /// Reverse of `kind_str` + JSON parse. Used by the projector
    /// (W5-04) and `list_typed` to rebuild events from SQLite rows.
    ///
    /// `payload_json` MUST carry the full tagged-enum JSON
    /// (including the `kind` field) — `MailboxBus::emit_typed`
    /// always serialises the whole event so this is byte-for-byte
    /// what landed in SQL.
    ///
    /// On parse failure, returns `AppError::Internal` rather than
    /// `AppError::InvalidInput` because a malformed payload here
    /// implies a SQL row written by a buggy emitter, not a user
    /// supplying a bad input.
    pub fn from_row_parts(
        kind: &str,
        payload_json: &str,
    ) -> Result<Self, AppError> {
        // Default-empty body ('{}') with kind='note' decodes as
        // Note via the tagged-enum form `{"kind":"note"}`. The
        // migration 0010 default is `'{}'` not `'{"kind":"note"}'`,
        // so we splice the `kind` tag in if the payload is empty
        // or doesn't already carry it.
        let trimmed = payload_json.trim();
        let parsed: serde_json::Value = if trimmed.is_empty() || trimmed == "{}" {
            serde_json::json!({ "kind": kind })
        } else {
            let mut v: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|e| AppError::Internal(format!(
                    "mailbox payload_json parse error: {e}"
                )))?;
            // If the payload is a JSON object missing the kind tag,
            // splice it in so the tagged-enum deserialise can pick
            // the variant. Defensive: emitters always write the
            // full tagged form, but legacy 'note' rows from before
            // W5-01 have payload_json='{}' and we want them to
            // round-trip cleanly.
            if let serde_json::Value::Object(map) = &mut v {
                if !map.contains_key("kind") {
                    map.insert(
                        "kind".to_string(),
                        serde_json::Value::String(kind.to_string()),
                    );
                }
            }
            v
        };
        serde_json::from_value(parsed).map_err(|e| {
            AppError::Internal(format!(
                "mailbox event deserialise error (kind={kind}): {e}"
            ))
        })
    }
}
