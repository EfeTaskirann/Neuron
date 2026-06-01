//! `swarm:get_job` hydration shims — read `swarm_jobs` /
//! `swarm_stages` from outside the `coordinator` module (its store
//! helpers are `pub(super)`-scoped to coordinator). Column lists
//! mirror `coordinator::store` byte-for-byte (WP-W5-04). Extracted
//! verbatim from `projector.rs`.

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::{JobDetail, JobState, StageResult, Verdict};

/// Convenience accessor for tests and `swarm_get_job`: reads the
/// stored detail from SQL. Wraps `coordinator::store::get_job_detail`
/// since that helper is `pub(super)` to the coordinator module —
/// the projector lives outside that module so it can't call it
/// directly. The shim also lets us add brain-specific shaping in
/// the future (e.g. surface the projector's in-memory entry when
/// the row is still in flight).
pub async fn get_brain_job_detail(
    pool: &DbPool,
    job_id: &str,
) -> Result<Option<JobDetail>, AppError> {
    // Defer to a SQL query that mirrors the coordinator helper
    // but is reachable from this module.
    let row = sqlx::query(
        "SELECT id, workspace_id, goal, created_at_ms, finished_at_ms, \
                state, retry_count, last_error, last_verdict_json, source \
         FROM swarm_jobs \
         WHERE id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    use sqlx::Row;
    let id: String = row.try_get("id")?;
    let workspace_id: String = row.try_get("workspace_id")?;
    let goal: String = row.try_get("goal")?;
    let created_at_ms: i64 = row.try_get("created_at_ms")?;
    let finished_at_ms: Option<i64> = row.try_get("finished_at_ms")?;
    let state_str: String = row.try_get("state")?;
    let retry_count_i: i64 = row.try_get("retry_count")?;
    let last_error: Option<String> = row.try_get("last_error")?;
    let last_verdict_json: Option<String> = row.try_get("last_verdict_json")?;
    let source: String = row.try_get("source")?;
    let state = JobState::from_db_str(&state_str)?;
    let last_verdict = match last_verdict_json {
        None => None,
        Some(s) => Some(serde_json::from_str::<Verdict>(&s).map_err(|e| {
            AppError::Internal(format!(
                "JobProjector: failed to deserialize Verdict from DB: {e}"
            ))
        })?),
    };
    let stages = fetch_brain_stages(pool, &id).await?;
    let total_cost_usd: f64 = stages.iter().map(|s| s.total_cost_usd).sum();
    let total_duration_ms: u64 = stages.iter().map(|s| s.duration_ms).sum();
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

/// SQL helper paired with [`get_brain_job_detail`]. Mirrors
/// `coordinator::store::fetch_stages` byte-for-byte (the column
/// list is the same), but lives here so we can read from outside
/// the `coordinator` module.
pub(super) async fn fetch_brain_stages(
    pool: &DbPool,
    job_id: &str,
) -> Result<Vec<StageResult>, AppError> {
    use sqlx::Row;
    use crate::swarm::coordinator::CoordinatorDecision;
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
        let verdict = match verdict_json {
            None => None,
            Some(s) => Some(serde_json::from_str::<Verdict>(&s).map_err(|e| {
                AppError::Internal(format!(
                    "JobProjector: failed to deserialize Verdict: {e}"
                ))
            })?),
        };
        let coordinator_decision = match decision_json {
            None => None,
            Some(s) => Some(serde_json::from_str::<CoordinatorDecision>(&s).map_err(
                |e| {
                    AppError::Internal(format!(
                        "JobProjector: failed to deserialize CoordinatorDecision: {e}"
                    ))
                },
            )?),
        };
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
