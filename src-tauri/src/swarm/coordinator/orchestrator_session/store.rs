//! SQLite write-through + read helpers for the Orchestrator session
//! log. See the [module docs](super) for the rationale behind
//! string-query (vs `query!`) and the role → `content` shape.

use sqlx::Row;

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::orchestrator::OrchestratorOutcome;

use super::model::{OrchestratorMessage, OrchestratorMessageRole};

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
