//! `swarm_jobs` / `swarm_stages` write-through used by the
//! projection loop. Mirrors `coordinator::store` column sets so
//! brain-authored rows read back identically to FSM-authored ones
//! (WP-W5-04). Extracted verbatim from `projector.rs`.

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::{Job, JobState, StageResult};

/// Insert a brain-driven `swarm_jobs` row. Idempotent at the
/// projector level: if the row already exists (the IPC pre-
/// inserted via `try_acquire_workspace`), the unique-key
/// violation is swallowed and the projector continues. We do NOT
/// migrate the existing row's `source` value — the IPC always
/// inserts with `source='brain'` for v2 jobs, so the value lines
/// up with the projector's intent.
pub(super) async fn upsert_brain_job_row(
    pool: &DbPool,
    job: &Job,
    workspace_id: &str,
) -> Result<(), AppError> {
    // Detect existing row first; insert only when missing. SQLite's
    // INSERT OR IGNORE would also work but we want to surface
    // unrelated errors (e.g. column-default drift) cleanly.
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM swarm_jobs WHERE id = ?",
    )
    .bind(&job.id)
    .fetch_one(pool)
    .await?;
    if exists > 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO swarm_jobs \
         (id, workspace_id, goal, created_at_ms, state, retry_count, last_error, finished_at_ms, last_verdict_json, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&job.id)
    .bind(workspace_id)
    .bind(&job.goal)
    .bind(job.created_at_ms)
    .bind(job.state.as_db_str())
    .bind(job.retry_count as i64)
    .bind(job.last_error.as_deref())
    .bind(Option::<i64>::None)
    .bind(Option::<String>::None)
    .bind(&job.source)
    .execute(pool)
    .await?;
    Ok(())
}

/// Append one stage row. Mirrors `coordinator::store::insert_stage`
/// but reachable from outside the `coordinator` module — the
/// projector lives next to (not inside) `coordinator/`. Same
/// column set / same idx semantics so reads via
/// `coordinator::store::get_job_detail` see the same shape FSM-
/// authored stages produce.
pub(super) async fn persist_stage(
    pool: &DbPool,
    job_id: &str,
    idx: u32,
    stage: &StageResult,
    created_at_ms: i64,
) -> Result<(), AppError> {
    let verdict_json = match stage.verdict.as_ref() {
        None => None,
        Some(v) => Some(serde_json::to_string(v).map_err(|e| {
            AppError::Internal(format!(
                "JobProjector: failed to serialize Verdict: {e}"
            ))
        })?),
    };
    let decision_json = match stage.coordinator_decision.as_ref() {
        None => None,
        Some(d) => Some(serde_json::to_string(d).map_err(|e| {
            AppError::Internal(format!(
                "JobProjector: failed to serialize CoordinatorDecision: {e}"
            ))
        })?),
    };
    sqlx::query(
        "INSERT INTO swarm_stages \
         (job_id, idx, state, specialist_id, assistant_text, session_id, total_cost_usd, duration_ms, created_at_ms, verdict_json, decision_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(idx as i64)
    .bind(stage.state.as_db_str())
    .bind(&stage.specialist_id)
    .bind(&stage.assistant_text)
    .bind(&stage.session_id)
    .bind(stage.total_cost_usd)
    .bind(stage.duration_ms as i64)
    .bind(created_at_ms)
    .bind(verdict_json)
    .bind(decision_json)
    .execute(pool)
    .await?;
    Ok(())
}

/// Cancellation update — flip the row to Failed with the
/// canonical `cancelled by user` last_error. `finished_at_ms` is
/// NOT stamped here; the trailing JobFinished stamp does that.
pub(super) async fn update_job_cancelled(
    pool: &DbPool,
    job_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE swarm_jobs \
         SET state = ?, last_error = ? \
         WHERE id = ?",
    )
    .bind(JobState::Failed.as_db_str())
    .bind("cancelled by user")
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Terminal update. Stamps `state`, `last_error`, `finished_at_ms`
/// in one statement. Does NOT touch `last_verdict_json` — the
/// projector's per-stage rows already carry the verdicts; the
/// row-level `last_verdict_json` is FSM-only bookkeeping.
pub(super) async fn update_job_finished(
    pool: &DbPool,
    job_id: &str,
    state: JobState,
    last_error: Option<&str>,
    finished_at_ms: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE swarm_jobs \
         SET state = ?, last_error = COALESCE(?, last_error), finished_at_ms = ? \
         WHERE id = ?",
    )
    .bind(state.as_db_str())
    .bind(last_error)
    .bind(finished_at_ms)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}
