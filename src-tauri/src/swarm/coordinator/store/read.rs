//! Read surface for the swarm Coordinator: the recent-jobs list,
//! single-job detail, raw stage rows, and the full-`Job` hydration
//! used by the orphan-recovery sweep. The JSON column codecs and
//! goal truncation these call live in `cols.rs`.

use sqlx::Row;

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::job::{
    Job, JobDetail, JobState, JobSummary, StageResult,
};

use super::cols::{deserialize_decision, deserialize_verdict, truncate_chars};

/// Query the recent-jobs surface. `workspace_id_opt` filters on
/// the indexed column; `limit` caps the result set (the IPC layer
/// applies the 50/200 default/cap before calling).
///
/// Returns `JobSummary` shapes — the per-job `assistant_text` is
/// not pulled, just the aggregated `total_cost_usd` (SUM of stage
/// costs) and `stage_count` (COUNT of stage rows). Sorted
/// newest-first.
pub(in crate::swarm::coordinator) async fn list_jobs(
    pool: &DbPool,
    workspace_id_opt: Option<&str>,
    limit: u32,
) -> Result<Vec<JobSummary>, AppError> {
    let rows = if let Some(workspace_id) = workspace_id_opt {
        sqlx::query(
            "SELECT j.id, j.workspace_id, j.goal, j.created_at_ms, j.finished_at_ms, \
                    j.state, j.last_error, j.source, \
                    COALESCE((SELECT COUNT(*) FROM swarm_stages s WHERE s.job_id = j.id), 0) AS stage_count, \
                    COALESCE((SELECT SUM(s.total_cost_usd) FROM swarm_stages s WHERE s.job_id = j.id), 0.0) AS total_cost_usd \
             FROM swarm_jobs j \
             WHERE j.workspace_id = ? \
             ORDER BY j.created_at_ms DESC \
             LIMIT ?",
        )
        .bind(workspace_id)
        .bind(limit as i64)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT j.id, j.workspace_id, j.goal, j.created_at_ms, j.finished_at_ms, \
                    j.state, j.last_error, j.source, \
                    COALESCE((SELECT COUNT(*) FROM swarm_stages s WHERE s.job_id = j.id), 0) AS stage_count, \
                    COALESCE((SELECT SUM(s.total_cost_usd) FROM swarm_stages s WHERE s.job_id = j.id), 0.0) AS total_cost_usd \
             FROM swarm_jobs j \
             ORDER BY j.created_at_ms DESC \
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(pool)
        .await?
    };

    let mut out: Vec<JobSummary> = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id")?;
        let workspace_id: String = row.try_get("workspace_id")?;
        let goal: String = row.try_get("goal")?;
        let created_at_ms: i64 = row.try_get("created_at_ms")?;
        let finished_at_ms: Option<i64> =
            row.try_get("finished_at_ms")?;
        let state_str: String = row.try_get("state")?;
        let last_error: Option<String> = row.try_get("last_error")?;
        let source: String = row.try_get("source")?;
        let stage_count_i: i64 = row.try_get("stage_count")?;
        let total_cost_usd: f64 = row.try_get("total_cost_usd")?;

        let state = JobState::from_db_str(&state_str)?;
        out.push(JobSummary {
            id,
            workspace_id,
            goal: truncate_chars(&goal, 200),
            created_at_ms,
            finished_at_ms,
            state,
            stage_count: stage_count_i.max(0) as u32,
            total_cost_usd,
            last_error,
            source,
        });
    }
    Ok(out)
}

/// Fetch one job + every stage row in `(job_id, idx)` order.
/// Returns `None` when the job id is unknown so the IPC layer can
/// map to `AppError::NotFound`.
pub(in crate::swarm::coordinator) async fn get_job_detail(
    pool: &DbPool,
    job_id: &str,
) -> Result<Option<JobDetail>, AppError> {
    let job_row_opt = sqlx::query(
        "SELECT id, workspace_id, goal, created_at_ms, finished_at_ms, \
                state, retry_count, last_error, last_verdict_json, source \
         FROM swarm_jobs \
         WHERE id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = job_row_opt else {
        return Ok(None);
    };

    let id: String = row.try_get("id")?;
    let workspace_id: String = row.try_get("workspace_id")?;
    let goal: String = row.try_get("goal")?;
    let created_at_ms: i64 = row.try_get("created_at_ms")?;
    let finished_at_ms: Option<i64> = row.try_get("finished_at_ms")?;
    let state_str: String = row.try_get("state")?;
    let retry_count_i: i64 = row.try_get("retry_count")?;
    let last_error: Option<String> = row.try_get("last_error")?;
    let last_verdict_json: Option<String> =
        row.try_get("last_verdict_json")?;
    let source: String = row.try_get("source")?;
    let state = JobState::from_db_str(&state_str)?;
    let last_verdict = deserialize_verdict(last_verdict_json.as_deref())?;

    let stages = fetch_stages(pool, &id).await?;
    let total_cost_usd: f64 =
        stages.iter().map(|s| s.total_cost_usd).sum();
    let total_duration_ms: u64 =
        stages.iter().map(|s| s.duration_ms).sum();
    Ok(Some(JobDetail {
        id,
        workspace_id,
        goal,
        created_at_ms,
        finished_at_ms,
        state,
        retry_count: retry_count_i.max(0) as u32,
        stages,
        last_error,
        total_cost_usd,
        total_duration_ms,
        last_verdict,
        source,
    }))
}

/// Read every stage row for `job_id`, ordered by `idx`. Used by
/// `get_job_detail` and by `recover_orphans`'s hydration sweep.
async fn fetch_stages(
    pool: &DbPool,
    job_id: &str,
) -> Result<Vec<StageResult>, AppError> {
    let rows = sqlx::query(
        "SELECT idx, state, specialist_id, assistant_text, session_id, \
                total_cost_usd, duration_ms, verdict_json, decision_json \
         FROM swarm_stages \
         WHERE job_id = ? \
         ORDER BY idx ASC",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let state_str: String = row.try_get("state")?;
        let specialist_id: String = row.try_get("specialist_id")?;
        let assistant_text: String = row.try_get("assistant_text")?;
        let session_id: String = row.try_get("session_id")?;
        let total_cost_usd: f64 = row.try_get("total_cost_usd")?;
        let duration_ms_i: i64 = row.try_get("duration_ms")?;
        let verdict_json: Option<String> = row.try_get("verdict_json")?;
        let decision_json: Option<String> = row.try_get("decision_json")?;
        let state = JobState::from_db_str(&state_str)?;
        let verdict = deserialize_verdict(verdict_json.as_deref())?;
        let coordinator_decision =
            deserialize_decision(decision_json.as_deref())?;
        out.push(StageResult {
            state,
            specialist_id,
            assistant_text,
            session_id,
            total_cost_usd,
            duration_ms: duration_ms_i.max(0) as u64,
            verdict,
            coordinator_decision,
        });
    }
    Ok(out)
}

/// Read recent jobs into full `Job` snapshots — used by
/// `recover_orphans`'s hydration step to warm the in-memory cache
/// with terminal rows after the orphan sweep. Bounded by `limit`
/// (caller passes 100).
pub(in crate::swarm::coordinator) async fn list_recent_jobs_full(
    pool: &DbPool,
    limit: u32,
) -> Result<Vec<Job>, AppError> {
    let rows = sqlx::query(
        "SELECT id FROM swarm_jobs \
         ORDER BY created_at_ms DESC \
         LIMIT ?",
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id")?;
        if let Some(detail) = get_job_detail(pool, &id).await? {
            out.push(detail_to_job(detail));
        }
    }
    Ok(out)
}

/// Strip the wire-only fields off a `JobDetail` to reconstruct the
/// in-memory `Job` shape. The aggregates (`total_cost_usd`,
/// `total_duration_ms`) are dropped because `Job` does not carry
/// them — they are recomputed on the fly when needed.
pub(super) fn detail_to_job(detail: JobDetail) -> Job {
    Job {
        id: detail.id,
        goal: detail.goal,
        created_at_ms: detail.created_at_ms,
        state: detail.state,
        retry_count: detail.retry_count,
        stages: detail.stages,
        last_error: detail.last_error,
        last_verdict: detail.last_verdict,
        source: detail.source,
    }
}
