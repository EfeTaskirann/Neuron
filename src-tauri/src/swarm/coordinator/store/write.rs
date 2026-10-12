//! Write-through helpers for the swarm Coordinator: INSERT/UPDATE
//! of `swarm_jobs` / `swarm_stages` rows and the workspace-lock
//! bookkeeping. The read surface lives in `read.rs`; the JSON
//! column codecs these call live in `cols.rs`.

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::job::{Job, StageResult};

use super::cols::{serialize_decision, serialize_verdict};

/// Insert a fresh job row plus its workspace-lock row in one
/// transaction. Atomicity matters: the in-memory side has already
/// claimed the workspace, so a partial write (job row but no lock,
/// or vice-versa) would let the next `recover_orphans` sweep
/// quietly desync the two layers.
pub(in crate::swarm::coordinator) async fn insert_job_and_lock(
    pool: &DbPool,
    job: &Job,
    workspace_id: &str,
    acquired_at_ms: i64,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let last_verdict_json = serialize_verdict(job.last_verdict.as_ref())?;
    // WP-W5-04 — `source` discriminates FSM-driven (`'fsm'`) from
    // brain-driven (`'brain'`) job rows. Migration 0011 added the
    // column with default `'fsm'`; this INSERT carries `Job.source`
    // through verbatim so the projector's `'brain'` writes land
    // correctly.
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
    .bind(last_verdict_json)
    .bind(&job.source)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO swarm_workspace_locks \
         (workspace_id, job_id, acquired_at_ms) \
         VALUES (?, ?, ?)",
    )
    .bind(workspace_id)
    .bind(&job.id)
    .bind(acquired_at_ms)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Re-serialize a `Job` and UPDATE its `swarm_jobs` row. Stage
/// rows are NOT touched here — `insert_stage` handles new stages
/// individually so the `update` happy path doesn't pay the cost of
/// re-INSERTing every prior stage on every state transition.
///
/// `finished_at_ms` is `Some(now_ms)` iff `job.state` is terminal
/// (Done/Failed) — the registry's `update` helper supplies this so
/// the column is consistent with the state column.
pub(in crate::swarm::coordinator) async fn update_job(
    pool: &DbPool,
    job: &Job,
    finished_at_ms: Option<i64>,
) -> Result<(), AppError> {
    let last_verdict_json = serialize_verdict(job.last_verdict.as_ref())?;
    sqlx::query(
        "UPDATE swarm_jobs \
         SET state = ?, retry_count = ?, last_error = ?, finished_at_ms = ?, last_verdict_json = ? \
         WHERE id = ?",
    )
    .bind(job.state.as_db_str())
    .bind(job.retry_count as i64)
    .bind(job.last_error.as_deref())
    .bind(finished_at_ms)
    .bind(last_verdict_json)
    .bind(&job.id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Append one `StageResult` to `swarm_stages`. `idx` is the 0-based
/// position within `Job.stages` (i.e. the value returned by
/// `Vec::len()` *before* the push).
pub(in crate::swarm::coordinator) async fn insert_stage(
    pool: &DbPool,
    job_id: &str,
    idx: u32,
    stage: &StageResult,
    created_at_ms: i64,
) -> Result<(), AppError> {
    let verdict_json = serialize_verdict(stage.verdict.as_ref())?;
    let decision_json =
        serialize_decision(stage.coordinator_decision.as_ref())?;
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

/// Delete the workspace-lock row for `workspace_id`. Idempotent at
/// the SQL boundary — a missing row is not an error.
pub(in crate::swarm::coordinator) async fn delete_workspace_lock(
    pool: &DbPool,
    workspace_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM swarm_workspace_locks WHERE workspace_id = ?")
        .bind(workspace_id)
        .execute(pool)
        .await?;
    Ok(())
}
