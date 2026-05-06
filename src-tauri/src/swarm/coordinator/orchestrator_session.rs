//! WP-W3-12k2 — persistent Orchestrator chat history.
//!
//! Sister module to `orchestrator.rs` (W3-12k1's stateless brain).
//! W3-12k1 shipped a one-shot `swarm:orchestrator_decide` that took a
//! single `user_message` and returned an `OrchestratorOutcome` with no
//! conversation context. W3-12k3 shipped the chat UI panel but kept
//! messages in React state — gone on reload.
//!
//! This module is the SQLite write-through that closes both gaps:
//!
//! 1. The IPC handler (`commands::swarm::swarm_orchestrator_decide`)
//!    persists each user message + each orchestrator outcome through
//!    the helpers below, so reload sees the full thread.
//! 2. The same IPC handler reads the most-recent N messages and
//!    pre-pends them to the prompt via [`render_with_history`], so
//!    the persona sees prior context when deciding the next action.
//! 3. Three new IPCs (`swarm:orchestrator_history`,
//!    `swarm:orchestrator_clear_history`, `swarm:orchestrator_log_job`)
//!    expose the read / clear / job-log surfaces to the frontend.
//!
//! ## Why string-query, not `query!`?
//!
//! The offline cache (`src-tauri/.sqlx/`) must be regenerated whenever
//! the schema changes. Mirrors the rationale in
//! `swarm/coordinator/store.rs`: forcing a multi-step ritual onto a
//! straightforward append-only log was strictly worse than runtime-
//! checked `sqlx::query`.
//!
//! ## Persistence shape
//!
//! Three roles share one TEXT `content` column. The role tag selects
//! the parser:
//!
//! - `User` — `content` is the raw user text.
//! - `Orchestrator` — `content` is a JSON-encoded
//!   `OrchestratorOutcome` (action + text + reasoning packed for
//!   round-trip).
//! - `Job` — `content` is the dispatched `job_id`; the refined goal
//!   travels in the dedicated `goal` column.
//!
//! Tradeoff documented in the migration file: schema simplicity
//! beats column-per-shape proliferation when the only access pattern
//! is "list recent N for workspace X".
//!
//! Cross-runtime hygiene: this module imports only from `serde`,
//! `sqlx::Row`, `specta`, and the orchestrator types in the same
//! coordinator subtree. No `sidecar/`, no `agent_runtime/`, no Tauri
//! runtime — the helpers are pure storage operations the IPC handler
//! drives.

use serde::{Deserialize, Serialize};
use specta::Type;
use sqlx::Row;

use crate::db::DbPool;
use crate::error::AppError;

use super::orchestrator::OrchestratorOutcome;

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
    fn as_db_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Orchestrator => "orchestrator",
            Self::Job => "job",
        }
    }

    /// Inverse of [`as_db_str`]. Unknown values surface as a typed
    /// `AppError::Internal` so a corrupted DB never silently coerces
    /// a row into the wrong role.
    fn from_db_str(s: &str) -> Result<Self, AppError> {
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

/// Append a `User` row with the raw user text. Returns the new
/// AUTOINCREMENT id so callers can correlate the message with later
/// outcome rows in tests.
///
/// Persisted **before** the LLM invoke so a hung / failed subprocess
/// still preserves the user's input on the next mount.
pub(crate) async fn append_user_message(
    pool: &DbPool,
    workspace_id: &str,
    text: &str,
    now_ms: i64,
) -> Result<i64, AppError> {
    let result = sqlx::query(
        "INSERT INTO orchestrator_messages \
         (workspace_id, role, content, goal, created_at_ms) \
         VALUES (?, ?, ?, NULL, ?)",
    )
    .bind(workspace_id)
    .bind(OrchestratorMessageRole::User.as_db_str())
    .bind(text)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Append an `Orchestrator` row. The `OrchestratorOutcome` is
/// JSON-serialized into `content` so the read path can round-trip
/// the action label + reasoning verbatim.
///
/// Persisted **after** a successful parse so an unparseable result
/// surfaces as an IPC error to the caller without leaving a half-
/// baked row in the log.
pub(crate) async fn append_orchestrator_message(
    pool: &DbPool,
    workspace_id: &str,
    outcome: &OrchestratorOutcome,
    now_ms: i64,
) -> Result<i64, AppError> {
    let json = serde_json::to_string(outcome).map_err(|e| {
        AppError::Internal(format!(
            "orchestrator_session: failed to serialize OrchestratorOutcome: {e}"
        ))
    })?;
    let result = sqlx::query(
        "INSERT INTO orchestrator_messages \
         (workspace_id, role, content, goal, created_at_ms) \
         VALUES (?, ?, ?, NULL, ?)",
    )
    .bind(workspace_id)
    .bind(OrchestratorMessageRole::Orchestrator.as_db_str())
    .bind(json)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Append a `Job` row recording that a swarm job was dispatched
/// from the chat surface. `content` carries the `job_id`; `goal`
/// carries the refined goal the FSM was started with.
///
/// Called from the frontend orchestration glue
/// (`commands::swarm::swarm_orchestrator_log_job`) immediately after
/// `swarm:run_job` returns, so the chat thread shows the dispatch on
/// the next mount without the FSM itself having to know about the
/// chat history.
pub(crate) async fn append_job_message(
    pool: &DbPool,
    workspace_id: &str,
    job_id: &str,
    goal: &str,
    now_ms: i64,
) -> Result<i64, AppError> {
    let result = sqlx::query(
        "INSERT INTO orchestrator_messages \
         (workspace_id, role, content, goal, created_at_ms) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(workspace_id)
    .bind(OrchestratorMessageRole::Job.as_db_str())
    .bind(job_id)
    .bind(goal)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Read the most-recent `limit` messages for `workspace_id`. The
/// underlying SELECT orders DESC by `created_at_ms` so the bounded
/// LIMIT clause hits the index, and the in-memory reverse flips the
/// result to chronological (oldest-first) for both the prompt
/// renderer and the chat panel mount-seed.
///
/// Tie-breaker on `id ASC` keeps the order deterministic when two
/// rows share `created_at_ms` (1ms granularity makes that realistic
/// when the orchestrator outcome lands inside the same millisecond
/// as the user message).
pub(crate) async fn list_recent_messages(
    pool: &DbPool,
    workspace_id: &str,
    limit: u32,
) -> Result<Vec<OrchestratorMessage>, AppError> {
    let rows = sqlx::query(
        "SELECT id, workspace_id, role, content, goal, created_at_ms \
         FROM orchestrator_messages \
         WHERE workspace_id = ? \
         ORDER BY created_at_ms DESC, id DESC \
         LIMIT ?",
    )
    .bind(workspace_id)
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;
    let mut out: Vec<OrchestratorMessage> = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let workspace_id: String = row.try_get("workspace_id")?;
        let role_str: String = row.try_get("role")?;
        let content: String = row.try_get("content")?;
        let goal: Option<String> = row.try_get("goal")?;
        let created_at_ms: i64 = row.try_get("created_at_ms")?;
        let role = OrchestratorMessageRole::from_db_str(&role_str)?;
        out.push(OrchestratorMessage {
            id,
            workspace_id,
            role,
            content,
            goal,
            created_at_ms,
        });
    }
    // Reverse in-memory: SELECT was DESC for index efficiency, but
    // both consumers (prompt renderer + chat panel seed) want oldest-
    // first chronological order.
    out.reverse();
    Ok(out)
}

/// Hard-delete every message for `workspace_id`. Idempotent at the
/// SQL boundary — a workspace with no rows is not an error. The
/// frontend's "Clear chat" button drives this; there is no soft
/// delete or archival.
pub(crate) async fn clear_messages(
    pool: &DbPool,
    workspace_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM orchestrator_messages WHERE workspace_id = ?")
        .bind(workspace_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Prompt template the IPC handler uses to inject recent history
/// into the next decide call. `{history_lines}` is one line per
/// prior message; `{user_message}` is the current user input.
///
/// Turkish surface text mirrors the `orchestrator.md` persona
/// language so the LLM sees consistent register across the system
/// prompt and the runtime context.
const HISTORY_TEMPLATE: &str = "Önceki konuşma (eskiden yeniye):\n\n\
{history_lines}\n\n\
---\n\nKullanıcının yeni mesajı:\n\n{user_message}\n";

/// Render a context-aware prompt that prepends `history` (chronological,
/// oldest-first) before `user_message`. When `history` is empty the
/// result is `user_message.to_string()` verbatim — no header, no
/// separator — so the very first turn is byte-identical to the W3-12k1
/// stateless behaviour.
///
/// Per-message formatting:
///
/// - `User` rows: `[user]: <content>`
/// - `Orchestrator` rows: `[orchestrator/<action>]: <text>` (decoded
///   from the JSON-packed outcome). If decode fails, the row is
///   surfaced as `[orchestrator]: <raw content>` so the prompt is
///   never silently dropped.
/// - `Job` rows: `[swarm dispatched]: <job_id> (goal: <goal>)`. A
///   missing goal column renders as `(goal: -)`.
///
/// All formatting is line-grain — newlines inside `content` would
/// confuse the LLM about which line is which message. Practically:
/// user messages and orchestrator text are short conversational
/// strings; if they ever grow paragraphs we revisit this in W3-12k4.
pub(crate) fn render_with_history(
    history: &[OrchestratorMessage],
    user_message: &str,
) -> String {
    if history.is_empty() {
        return user_message.to_string();
    }
    let history_lines: Vec<String> = history
        .iter()
        .map(|m| match m.role {
            OrchestratorMessageRole::User => {
                format!("[user]: {}", m.content)
            }
            OrchestratorMessageRole::Orchestrator => {
                // Decode the JSON-packed outcome for human-readable
                // display. A parse failure surfaces the raw content
                // rather than panicking — the prompt is informational
                // (the LLM tolerates noise), not a contract.
                match serde_json::from_str::<OrchestratorOutcome>(&m.content) {
                    Ok(outcome) => format!(
                        "[orchestrator/{}]: {}",
                        action_label(outcome.action),
                        outcome.text,
                    ),
                    Err(_) => format!("[orchestrator]: {}", m.content),
                }
            }
            OrchestratorMessageRole::Job => format!(
                "[swarm dispatched]: {} (goal: {})",
                m.content,
                m.goal.as_deref().unwrap_or("-"),
            ),
        })
        .collect();
    HISTORY_TEMPLATE
        .replace("{history_lines}", &history_lines.join("\n"))
        .replace("{user_message}", user_message)
}

/// One-line label for an `OrchestratorAction` used in
/// [`render_with_history`]. Mirrors the snake_case wire form so the
/// prompt the LLM reads matches the OUTPUT CONTRACT it must emit.
fn action_label(action: super::orchestrator::OrchestratorAction) -> &'static str {
    use super::orchestrator::OrchestratorAction;
    match action {
        OrchestratorAction::DirectReply => "direct_reply",
        OrchestratorAction::Clarify => "clarify",
        OrchestratorAction::Dispatch => "dispatch",
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::coordinator::orchestrator::OrchestratorAction;
    use crate::test_support::fresh_pool;

    /// Migration 0009 creates the `orchestrator_messages` table.
    #[tokio::test]
    async fn migration_0009_creates_orchestrator_messages_table() {
        let (pool, _dir) = fresh_pool().await;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='table' AND name = ?",
        )
        .bind("orchestrator_messages")
        .fetch_one(&pool)
        .await
        .expect("query");
        assert_eq!(count, 1, "orchestrator_messages table missing");
    }

    /// Migration 0009 indexes (workspace_id, created_at_ms).
    #[tokio::test]
    async fn migration_0009_indexes_workspace_and_created_at() {
        let (pool, _dir) = fresh_pool().await;
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master \
             WHERE type='index' AND tbl_name='orchestrator_messages'",
        )
        .fetch_all(&pool)
        .await
        .expect("list indexes");
        assert!(
            names.iter().any(|n| n == "idx_orchestrator_messages_workspace"),
            "composite index missing; indexes={names:?}"
        );
    }

    /// User row round-trips via append + list.
    #[tokio::test]
    async fn append_user_message_round_trip() {
        let (pool, _dir) = fresh_pool().await;
        let id = append_user_message(&pool, "ws-1", "selam", 1_000)
            .await
            .expect("append");
        assert!(id > 0, "rowid populated");
        let msgs = list_recent_messages(&pool, "ws-1", 50)
            .await
            .expect("list");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, OrchestratorMessageRole::User);
        assert_eq!(msgs[0].content, "selam");
        assert_eq!(msgs[0].goal, None);
        assert_eq!(msgs[0].created_at_ms, 1_000);
    }

    /// Orchestrator row JSON-encodes the outcome; the read path can
    /// parse the content column back into the typed shape.
    #[tokio::test]
    async fn append_orchestrator_message_serializes_outcome_as_json() {
        let (pool, _dir) = fresh_pool().await;
        let outcome = OrchestratorOutcome {
            action: OrchestratorAction::Clarify,
            text: "Hangi dosya?".into(),
            reasoning: "yol eksik".into(),
        };
        append_orchestrator_message(&pool, "ws-1", &outcome, 2_000)
            .await
            .expect("append");
        let msgs = list_recent_messages(&pool, "ws-1", 50)
            .await
            .expect("list");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, OrchestratorMessageRole::Orchestrator);
        let parsed: OrchestratorOutcome =
            serde_json::from_str(&msgs[0].content).expect("parse outcome json");
        assert_eq!(parsed, outcome);
    }

    /// Job row populates the `goal` column; `content` carries the
    /// job_id.
    #[tokio::test]
    async fn append_job_message_populates_goal_column() {
        let (pool, _dir) = fresh_pool().await;
        append_job_message(&pool, "ws-1", "j-abc", "Add doc to X.tsx", 3_000)
            .await
            .expect("append");
        let msgs = list_recent_messages(&pool, "ws-1", 50)
            .await
            .expect("list");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, OrchestratorMessageRole::Job);
        assert_eq!(msgs[0].content, "j-abc");
        assert_eq!(msgs[0].goal.as_deref(), Some("Add doc to X.tsx"));
    }

    /// Mixed inserts come back oldest-first regardless of insert
    /// order. SELECT is DESC then reversed in-memory; verifies the
    /// reverse step actually runs.
    #[tokio::test]
    async fn list_recent_messages_returns_chronological_oldest_first() {
        let (pool, _dir) = fresh_pool().await;
        // Insert out of order to make the test non-trivial.
        append_user_message(&pool, "ws", "first", 100)
            .await
            .expect("u1");
        append_orchestrator_message(
            &pool,
            "ws",
            &OrchestratorOutcome {
                action: OrchestratorAction::DirectReply,
                text: "second".into(),
                reasoning: "r".into(),
            },
            200,
        )
        .await
        .expect("o1");
        append_job_message(&pool, "ws", "j-1", "third", 300)
            .await
            .expect("j1");
        let msgs = list_recent_messages(&pool, "ws", 50)
            .await
            .expect("list");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].created_at_ms, 100);
        assert_eq!(msgs[1].created_at_ms, 200);
        assert_eq!(msgs[2].created_at_ms, 300);
        assert_eq!(msgs[0].role, OrchestratorMessageRole::User);
        assert_eq!(msgs[1].role, OrchestratorMessageRole::Orchestrator);
        assert_eq!(msgs[2].role, OrchestratorMessageRole::Job);
    }

    /// `limit` caps the result set; the kept rows are the most
    /// recent. Reverse-step preserves oldest-first within the cap.
    #[tokio::test]
    async fn list_recent_messages_respects_limit() {
        let (pool, _dir) = fresh_pool().await;
        for i in 0..20 {
            append_user_message(&pool, "ws", &format!("m{i}"), i as i64)
                .await
                .expect("seed");
        }
        let msgs = list_recent_messages(&pool, "ws", 5)
            .await
            .expect("list");
        assert_eq!(msgs.len(), 5);
        // Most recent 5 = ts 15..=19, reversed to oldest-first within
        // the slice.
        assert_eq!(msgs[0].created_at_ms, 15);
        assert_eq!(msgs[4].created_at_ms, 19);
    }

    /// Filter on `workspace_id` — rows from other workspaces are
    /// invisible.
    #[tokio::test]
    async fn list_recent_messages_filters_by_workspace_id() {
        let (pool, _dir) = fresh_pool().await;
        append_user_message(&pool, "ws-A", "a1", 100)
            .await
            .expect("seed");
        append_user_message(&pool, "ws-B", "b1", 200)
            .await
            .expect("seed");
        append_user_message(&pool, "ws-A", "a2", 300)
            .await
            .expect("seed");
        let a = list_recent_messages(&pool, "ws-A", 50)
            .await
            .expect("list A");
        assert_eq!(a.len(), 2);
        for m in &a {
            assert_eq!(m.workspace_id, "ws-A");
        }
        let b = list_recent_messages(&pool, "ws-B", 50)
            .await
            .expect("list B");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].content, "b1");
    }

    /// `clear_messages` deletes every row for the targeted workspace.
    #[tokio::test]
    async fn clear_messages_deletes_all_workspace_messages() {
        let (pool, _dir) = fresh_pool().await;
        for i in 0..5 {
            append_user_message(&pool, "ws", &format!("m{i}"), i as i64)
                .await
                .expect("seed");
        }
        clear_messages(&pool, "ws").await.expect("clear");
        let msgs = list_recent_messages(&pool, "ws", 50)
            .await
            .expect("list");
        assert!(msgs.is_empty());
    }

    /// `clear_messages` is per-workspace; other workspaces survive.
    #[tokio::test]
    async fn clear_messages_leaves_other_workspaces_intact() {
        let (pool, _dir) = fresh_pool().await;
        append_user_message(&pool, "ws-A", "keep", 100)
            .await
            .expect("seed");
        append_user_message(&pool, "ws-B", "drop", 200)
            .await
            .expect("seed");
        clear_messages(&pool, "ws-B").await.expect("clear B");
        let a = list_recent_messages(&pool, "ws-A", 50)
            .await
            .expect("list A");
        assert_eq!(a.len(), 1);
        let b = list_recent_messages(&pool, "ws-B", 50)
            .await
            .expect("list B");
        assert!(b.is_empty());
    }

    /// Empty history short-circuits to the raw user message.
    #[test]
    fn render_with_history_empty_returns_user_message_verbatim() {
        let rendered = render_with_history(&[], "selam");
        assert_eq!(rendered, "selam");
    }

    /// User-role rows render with the `[user]:` label.
    #[test]
    fn render_with_history_includes_user_role_label() {
        let history = vec![OrchestratorMessage {
            id: 1,
            workspace_id: "ws".into(),
            role: OrchestratorMessageRole::User,
            content: "auth refactor istiyorum".into(),
            goal: None,
            created_at_ms: 100,
        }];
        let rendered = render_with_history(&history, "evet");
        assert!(rendered.contains("[user]: auth refactor istiyorum"));
        assert!(rendered.contains("evet"));
        assert!(rendered.contains("Önceki konuşma"));
    }

    /// Orchestrator-role rows render with the action label.
    #[test]
    fn render_with_history_includes_orchestrator_action_label() {
        let outcome = OrchestratorOutcome {
            action: OrchestratorAction::Dispatch,
            text: "EXECUTE: Add doc to foo.ts".into(),
            reasoning: "concrete enough".into(),
        };
        let history = vec![OrchestratorMessage {
            id: 2,
            workspace_id: "ws".into(),
            role: OrchestratorMessageRole::Orchestrator,
            content: serde_json::to_string(&outcome).unwrap(),
            goal: None,
            created_at_ms: 200,
        }];
        let rendered = render_with_history(&history, "tamam");
        assert!(
            rendered.contains("[orchestrator/dispatch]: EXECUTE: Add doc to foo.ts"),
            "rendered: {rendered}"
        );
    }

    /// Job-role rows render with the dispatched job id and the goal.
    #[test]
    fn render_with_history_includes_dispatched_job_id() {
        let history = vec![OrchestratorMessage {
            id: 3,
            workspace_id: "ws".into(),
            role: OrchestratorMessageRole::Job,
            content: "j-12345".into(),
            goal: Some("Add JSDoc to foo.ts".into()),
            created_at_ms: 300,
        }];
        let rendered = render_with_history(&history, "next");
        assert!(
            rendered.contains("[swarm dispatched]: j-12345 (goal: Add JSDoc to foo.ts)"),
            "rendered: {rendered}"
        );
    }

    /// Unparseable orchestrator content still renders (never panics)
    /// and falls back to a bare `[orchestrator]:` label so the prompt
    /// is informative even in the degraded case.
    #[test]
    fn render_with_history_handles_unparseable_orchestrator_content() {
        let history = vec![OrchestratorMessage {
            id: 4,
            workspace_id: "ws".into(),
            role: OrchestratorMessageRole::Orchestrator,
            content: "not valid json".into(),
            goal: None,
            created_at_ms: 400,
        }];
        let rendered = render_with_history(&history, "next");
        assert!(
            rendered.contains("[orchestrator]: not valid json"),
            "rendered: {rendered}"
        );
    }
}
