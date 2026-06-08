//! Persisted chat-message wire types for the Orchestrator session log.
//!
//! See the [module docs](super) for the persistence shape and the
//! role → `content` interpretation table.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::AppError;

/// Three-way tag identifying which role authored a persisted chat
/// message. Wire form is snake_case so the frontend bindings match
/// the OUTPUT CONTRACT verbatim:
///
/// - `User` → `"user"`        — user-typed text.
/// - `Orchestrator` → `"orchestrator"` — assistant outcome bubble.
/// - `Job` → `"job"`          — "swarm dispatched" footer bubble.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMessageRole {
    User,
    Orchestrator,
    Job,
}

impl OrchestratorMessageRole {
    /// Persisted column value (lower-snake_case). Mirrors the
    /// `as_db_str` pattern from `JobState` so future migrations can
    /// pin the on-disk encoding without touching the wire form.
    pub(super) fn as_db_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Orchestrator => "orchestrator",
            Self::Job => "job",
        }
    }

    /// Inverse of [`as_db_str`](Self::as_db_str). Unknown values
    /// surface as a typed `AppError::Internal` so a corrupted DB never
    /// silently coerces a row into the wrong role.
    pub(super) fn from_db_str(s: &str) -> Result<Self, AppError> {
        match s {
            "user" => Ok(Self::User),
            "orchestrator" => Ok(Self::Orchestrator),
            "job" => Ok(Self::Job),
            other => Err(AppError::Internal(format!(
                "orchestrator_messages.role: unknown value `{other}`"
            ))),
        }
    }
}

/// One persisted chat message. The `content` column's interpretation
/// depends on `role`; the `goal` column is populated only for `Job`
/// rows.
///
/// Free-form by role:
///
/// - `User`: `content` is the raw user text; `goal` is `None`.
/// - `Orchestrator`: `content` is a JSON-encoded
///   `OrchestratorOutcome`; `goal` is `None`.
/// - `Job`: `content` is the dispatched `job_id`; `goal` carries the
///   refined goal that the Coordinator FSM was started with.
///
/// `rename_all = "camelCase"` so the wire shape matches the rest of
/// the swarm domain types (`JobSummary`, `JobOutcome`, etc.). The
/// `OrchestratorOutcome` JSON inside `content` is serialized
/// independently and keeps its own field naming (snake_case via the
/// W3-12k1 OUTPUT CONTRACT).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorMessage {
    pub id: i64,
    pub workspace_id: String,
    pub role: OrchestratorMessageRole,
    pub content: String,
    pub goal: Option<String>,
    pub created_at_ms: i64,
}
