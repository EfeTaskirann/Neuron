//! SQLite write-through helpers for the swarm Coordinator
//! (WP-W3-12b §5).
//!
//! `pub(super)` only — these helpers are FSM-internal. The Tauri
//! commands call through `JobRegistry` (or, for read-only history,
//! through the `list_jobs` / `get_job_detail` helpers below by way
//! of `commands::swarm`). Direct call sites outside `coordinator/`
//! would split the persistence story.
//!
//! Why string queries (`sqlx::query`) instead of macro queries
//! (`sqlx::query!`)? The offline cache lives in
//! `src-tauri/.sqlx/` and must be regenerated whenever the schema
//! changes. Forcing CI to refresh the cache for every `swarm_*`
//! query would couple this WP to a multi-step ritual that's easy
//! to skip; the existing tree mixes both styles, so we lean on
//! the runtime-checked variant here. The compile-time cache
//! coverage that already exists (one `agents` count) is left
//! intact.
//!
//! Goal-truncation policy. `JobSummary.goal` is char-bounded to
//! 200 chars (NOT byte-bounded — Turkish characters!) at this
//! layer so the IPC always returns the right shape without runtime
//! panics on multi-byte boundaries. Truncation lives here (not at
//! the wire serialization layer) so future read paths get the
//! same shape "for free".

use sqlx::Row;

use crate::db::DbPool;
use crate::error::AppError;

use super::decision::CoordinatorDecision;
use super::job::{
    Job, JobDetail, JobState, JobSummary, StageResult,
};
use super::verdict::Verdict;

/// Result of a `recover_orphans` sweep. `count` is the number of
/// non-terminal rows the sweep flipped to `Failed`; `recovered`
/// carries the corresponding `Job` snapshots so the caller can
/// hydrate the in-memory cache.
#[derive(Debug)]
pub(super) struct RecoveredOrphans {
    pub count: u32,
    pub recovered: Vec<Job>,
}

/// Insert a fresh job row plus its workspace-lock row in one
/// transaction. Atomicity matters: the in-memory side has already
/// claimed the workspace, so a partial write (job row but no lock,
/// or vice-versa) would let the next `recover_orphans` sweep
/// quietly desync the two layers.
pub(super) async fn insert_job_and_lock(
    pool: &DbPool,
    job: &Job,
    workspace_id: &str,
    acquired_at_ms: i64,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let last_verdict_json = serialize_verdict(job.last_verdict.as_ref())?;
    sqlx::query(
        "INSERT INTO swarm_jobs \
         (id, workspace_id, goal, created_at_ms, state, retry_count, last_error, finished_at_ms, last_verdict_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
pub(super) async fn update_job(
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
pub(super) async fn insert_stage(
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
pub(super) async fn delete_workspace_lock(
    pool: &DbPool,
    workspace_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM swarm_workspace_locks WHERE workspace_id = ?")
        .bind(workspace_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Query the recent-jobs surface. `workspace_id_opt` filters on
/// the indexed column; `limit` caps the result set (the IPC layer
/// applies the 50/200 default/cap before calling).
///
/// Returns `JobSummary` shapes — the per-job `assistant_text` is
/// not pulled, just the aggregated `total_cost_usd` (SUM of stage
/// costs) and `stage_count` (COUNT of stage rows). Sorted
/// newest-first.
pub(super) async fn list_jobs(
    pool: &DbPool,
    workspace_id_opt: Option<&str>,
    limit: u32,
) -> Result<Vec<JobSummary>, AppError> {
    let rows = if let Some(workspace_id) = workspace_id_opt {
        sqlx::query(
            "SELECT j.id, j.workspace_id, j.goal, j.created_at_ms, j.finished_at_ms, \
                    j.state, j.last_error, \
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
                    j.state, j.last_error, \
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
        });
    }
    Ok(out)
}

/// Fetch one job + every stage row in `(job_id, idx)` order.
/// Returns `None` when the job id is unknown so the IPC layer can
/// map to `AppError::NotFound`.
pub(super) async fn get_job_detail(
    pool: &DbPool,
    job_id: &str,
) -> Result<Option<JobDetail>, AppError> {
    let job_row_opt = sqlx::query(
        "SELECT id, workspace_id, goal, created_at_ms, finished_at_ms, \
                state, retry_count, last_error, last_verdict_json \
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

/// Sweep orphan jobs left non-terminal at process start. Three
/// steps under the hood:
///
/// 1. SELECT every non-terminal row's id (so we can hydrate the
///    cache after the UPDATE without an extra round-trip).
/// 2. UPDATE the rows to `Failed`.
/// 3. DELETE every workspace_lock row (cascade-safe; the job rows
///    survive in `Failed` state).
pub(super) async fn recover_orphans(
    pool: &DbPool,
    now_ms: i64,
) -> Result<RecoveredOrphans, AppError> {
    let mut tx = pool.begin().await?;

    // 1. Snapshot orphan ids.
    let orphan_rows = sqlx::query(
        "SELECT id FROM swarm_jobs WHERE state NOT IN ('done', 'failed')",
    )
    .fetch_all(&mut *tx)
    .await?;
    let orphan_ids: Vec<String> = orphan_rows
        .into_iter()
        .map(|row| row.try_get::<String, _>("id"))
        .collect::<Result<_, _>>()?;

    // 2. Flip orphan rows to Failed with the canonical message.
    if !orphan_ids.is_empty() {
        sqlx::query(
            "UPDATE swarm_jobs \
             SET state = 'failed', \
                 last_error = 'interrupted by app restart', \
                 finished_at_ms = ? \
             WHERE state NOT IN ('done', 'failed')",
        )
        .bind(now_ms)
        .execute(&mut *tx)
        .await?;
    }

    // 3. Clear all workspace locks. Locks belong to in-flight
    //    jobs; with every orphan now Failed, no job in the table
    //    can legitimately hold a lock.
    sqlx::query("DELETE FROM swarm_workspace_locks")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    // 4. Re-read each orphan's full job state so the registry can
    //    hydrate its in-memory cache. Done outside the tx so the
    //    in-flight DB lock doesn't widen.
    let mut recovered = Vec::with_capacity(orphan_ids.len());
    for id in &orphan_ids {
        if let Some(detail) = get_job_detail(pool, id).await? {
            recovered.push(detail_to_job(detail));
        }
    }

    Ok(RecoveredOrphans {
        count: orphan_ids.len() as u32,
        recovered,
    })
}

/// Read recent jobs into full `Job` snapshots — used by
/// `recover_orphans`'s hydration step to warm the in-memory cache
/// with terminal rows after the orphan sweep. Bounded by `limit`
/// (caller passes 100).
pub(super) async fn list_recent_jobs_full(
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
fn detail_to_job(detail: JobDetail) -> Job {
    Job {
        id: detail.id,
        goal: detail.goal,
        created_at_ms: detail.created_at_ms,
        state: detail.state,
        retry_count: detail.retry_count,
        stages: detail.stages,
        last_error: detail.last_error,
        last_verdict: detail.last_verdict,
    }
}

/// Serialize an optional `Verdict` to JSON for column storage.
/// `None` round-trips to `Ok(None)` (the column stays NULL).
/// Serialization failure surfaces as `AppError::Internal` —
/// `Verdict` is a closed serde shape so the only realistic failure
/// path is OOM, but we surface a typed error rather than panicking.
fn serialize_verdict(
    verdict: Option<&Verdict>,
) -> Result<Option<String>, AppError> {
    match verdict {
        None => Ok(None),
        Some(v) => serde_json::to_string(v)
            .map(Some)
            .map_err(|e| AppError::Internal(format!(
                "swarm: failed to serialize Verdict: {e}"
            ))),
    }
}

/// Deserialize an optional JSON column back to a `Verdict`.
/// `None` (NULL column) round-trips to `Ok(None)`; a non-null
/// column that fails to parse surfaces as `AppError::Internal`
/// with the parse error attached so a corrupted DB never silently
/// drops a Verdict.
fn deserialize_verdict(
    raw: Option<&str>,
) -> Result<Option<Verdict>, AppError> {
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str::<Verdict>(s)
            .map(Some)
            .map_err(|e| AppError::Internal(format!(
                "swarm: failed to deserialize Verdict from DB: {e}"
            ))),
    }
}

/// Serialize an optional `CoordinatorDecision` to JSON for column
/// storage (W3-12f). `None` round-trips to `Ok(None)` (the column
/// stays NULL). Mirrors `serialize_verdict` in shape.
fn serialize_decision(
    decision: Option<&CoordinatorDecision>,
) -> Result<Option<String>, AppError> {
    match decision {
        None => Ok(None),
        Some(d) => serde_json::to_string(d)
            .map(Some)
            .map_err(|e| AppError::Internal(format!(
                "swarm: failed to serialize CoordinatorDecision: {e}"
            ))),
    }
}

/// Deserialize an optional JSON column back to a `CoordinatorDecision`
/// (W3-12f). `None` (NULL column) round-trips to `Ok(None)`; a
/// non-null column that fails to parse surfaces as
/// `AppError::Internal` so a corrupted DB never silently drops a
/// decision.
fn deserialize_decision(
    raw: Option<&str>,
) -> Result<Option<CoordinatorDecision>, AppError> {
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str::<CoordinatorDecision>(s)
            .map(Some)
            .map_err(|e| AppError::Internal(format!(
                "swarm: failed to deserialize CoordinatorDecision from DB: {e}"
            ))),
    }
}

/// Truncate `s` to at most `max_chars` Unicode characters. Bounded
/// by `chars()` (not bytes) so multi-byte Turkish text is never
/// split mid-codepoint. Returns the original string when it's
/// already within the cap.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::coordinator::JobRegistry;
    use crate::test_support::fresh_pool;

    fn fixture_job(id: &str, goal: &str, created_at_ms: i64) -> Job {
        Job {
            id: id.to_string(),
            goal: goal.to_string(),
            created_at_ms,
            state: JobState::Init,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
            last_verdict: None,
        }
    }

    fn fixture_stage(state: JobState, cost: f64, dur: u64) -> StageResult {
        StageResult {
            state,
            specialist_id: format!("{state:?}").to_lowercase(),
            assistant_text: format!("text-{state:?}"),
            session_id: format!("sess-{state:?}"),
            total_cost_usd: cost,
            duration_ms: dur,
            verdict: None,
            coordinator_decision: None,
        }
    }

    /// Migration 0006 creates the three swarm tables.
    #[tokio::test]
    async fn migration_0006_creates_three_tables() {
        let (pool, _dir) = fresh_pool().await;
        for name in [
            "swarm_jobs",
            "swarm_stages",
            "swarm_workspace_locks",
        ] {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name = ?",
            )
            .bind(name)
            .fetch_one(&pool)
            .await
            .expect("query");
            assert_eq!(count, 1, "table `{name}` missing post-migration");
        }
    }

    /// Driving the registry's `try_acquire_workspace` writes both a
    /// job row and a workspace_lock row.
    #[tokio::test]
    async fn insert_job_and_lock_round_trip() {
        let (pool, _dir) = fresh_pool().await;
        let reg = JobRegistry::with_pool(pool.clone());
        let job = fixture_job("j-1", "goal one", 1000);
        reg.try_acquire_workspace("ws-1", job.clone())
            .await
            .expect("acquire");

        let row_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM swarm_jobs WHERE id = ?")
                .bind("j-1")
                .fetch_one(&pool)
                .await
                .expect("count");
        assert_eq!(row_count, 1);
        let lock_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_workspace_locks WHERE workspace_id = ?",
        )
        .bind("ws-1")
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(lock_count, 1);

        // The persisted state matches the in-memory snapshot.
        let state_str: String =
            sqlx::query_scalar("SELECT state FROM swarm_jobs WHERE id = ?")
                .bind("j-1")
                .fetch_one(&pool)
                .await
                .expect("state");
        assert_eq!(state_str, "init");
    }

    /// Driving a Job through Scout/Plan/Build/Done via `update`
    /// lands each intermediate state in the DB.
    #[tokio::test]
    async fn update_job_persists_state_transitions() {
        let (pool, _dir) = fresh_pool().await;
        let reg = JobRegistry::with_pool(pool.clone());
        reg.try_acquire_workspace("ws-2", fixture_job("j-2", "g", 0))
            .await
            .expect("acquire");

        for state in
            [JobState::Scout, JobState::Plan, JobState::Build, JobState::Done]
        {
            reg.update("j-2", |j| {
                j.state = state;
            })
            .await
            .expect("update");
            let on_disk: String = sqlx::query_scalar(
                "SELECT state FROM swarm_jobs WHERE id = ?",
            )
            .bind("j-2")
            .fetch_one(&pool)
            .await
            .expect("read state");
            assert_eq!(on_disk, state.as_db_str());
        }
        // Terminal state populated `finished_at_ms`.
        let finished: Option<i64> = sqlx::query_scalar(
            "SELECT finished_at_ms FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-2")
        .fetch_one(&pool)
        .await
        .expect("read finished");
        assert!(finished.is_some(), "Done state must populate finished_at_ms");
    }

    /// Pushing a `StageResult` via `update` writes a `swarm_stages`
    /// row at the right `idx`.
    #[tokio::test]
    async fn insert_stage_appends_to_job() {
        let (pool, _dir) = fresh_pool().await;
        let reg = JobRegistry::with_pool(pool.clone());
        reg.try_acquire_workspace("ws-3", fixture_job("j-3", "g", 0))
            .await
            .expect("acquire");
        reg.update("j-3", |j| {
            j.stages.push(fixture_stage(JobState::Scout, 0.01, 50));
        })
        .await
        .expect("first stage");
        reg.update("j-3", |j| {
            j.stages.push(fixture_stage(JobState::Plan, 0.02, 60));
        })
        .await
        .expect("second stage");

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_stages WHERE job_id = ?",
        )
        .bind("j-3")
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(count, 2);
        let idxs: Vec<i64> = sqlx::query_scalar(
            "SELECT idx FROM swarm_stages WHERE job_id = ? ORDER BY idx",
        )
        .bind("j-3")
        .fetch_all(&pool)
        .await
        .expect("idxs");
        assert_eq!(idxs, vec![0, 1]);
    }

    /// Calling `release_workspace` deletes the lock row.
    #[tokio::test]
    async fn release_workspace_deletes_lock_row() {
        let (pool, _dir) = fresh_pool().await;
        let reg = JobRegistry::with_pool(pool.clone());
        reg.try_acquire_workspace("ws-4", fixture_job("j-4", "g", 0))
            .await
            .expect("acquire");
        reg.release_workspace("ws-4", "j-4").await;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_workspace_locks WHERE workspace_id = ?",
        )
        .bind("ws-4")
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(count, 0);
    }

    /// `recover_orphans` flips non-terminal rows to Failed and
    /// stamps the canonical message + finished_at_ms.
    #[tokio::test]
    async fn recover_orphans_flips_non_terminal_jobs_to_failed() {
        let (pool, _dir) = fresh_pool().await;
        // Seed a Scout-state orphan directly via SQL — bypasses the
        // registry so we can simulate "previous process left this".
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("j-orphan")
        .bind("ws-x")
        .bind("g")
        .bind(123_i64)
        .bind("scout")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("seed orphan");

        let result = recover_orphans(&pool, 999_999).await.expect("recover");
        assert_eq!(result.count, 1);
        let state: String = sqlx::query_scalar(
            "SELECT state FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-orphan")
        .fetch_one(&pool)
        .await
        .expect("read state");
        assert_eq!(state, "failed");
        let last_err: Option<String> = sqlx::query_scalar(
            "SELECT last_error FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-orphan")
        .fetch_one(&pool)
        .await
        .expect("read last_error");
        assert_eq!(last_err.as_deref(), Some("interrupted by app restart"));
        let finished: Option<i64> = sqlx::query_scalar(
            "SELECT finished_at_ms FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-orphan")
        .fetch_one(&pool)
        .await
        .expect("read finished");
        assert_eq!(finished, Some(999_999));
    }

    /// `recover_orphans` clears every workspace_lock row.
    #[tokio::test]
    async fn recover_orphans_releases_workspace_locks() {
        let (pool, _dir) = fresh_pool().await;
        // Seed orphan + lock together.
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("j-locky")
        .bind("ws-locky")
        .bind("g")
        .bind(0_i64)
        .bind("plan")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("seed orphan");
        sqlx::query(
            "INSERT INTO swarm_workspace_locks (workspace_id, job_id, acquired_at_ms) \
             VALUES (?, ?, ?)",
        )
        .bind("ws-locky")
        .bind("j-locky")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("seed lock");

        recover_orphans(&pool, 1).await.expect("recover");
        let lock_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_workspace_locks",
        )
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(lock_count, 0, "lock rows cleared by recovery");
    }

    /// `recover_orphans` leaves Done/Failed rows untouched.
    #[tokio::test]
    async fn recover_orphans_leaves_terminal_jobs_alone() {
        let (pool, _dir) = fresh_pool().await;
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count, finished_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("j-done")
        .bind("ws")
        .bind("g")
        .bind(0_i64)
        .bind("done")
        .bind(0_i64)
        .bind(100_i64)
        .execute(&pool)
        .await
        .expect("seed done");
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count, last_error, finished_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("j-failed")
        .bind("ws")
        .bind("g")
        .bind(0_i64)
        .bind("failed")
        .bind(0_i64)
        .bind("boom")
        .bind(101_i64)
        .execute(&pool)
        .await
        .expect("seed failed");

        let result = recover_orphans(&pool, 999).await.expect("recover");
        assert_eq!(result.count, 0, "no orphans to recover");
        // finished_at_ms unchanged for both.
        let done_finished: Option<i64> = sqlx::query_scalar(
            "SELECT finished_at_ms FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-done")
        .fetch_one(&pool)
        .await
        .expect("read done");
        assert_eq!(done_finished, Some(100));
        let failed_finished: Option<i64> = sqlx::query_scalar(
            "SELECT finished_at_ms FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-failed")
        .fetch_one(&pool)
        .await
        .expect("read failed");
        assert_eq!(failed_finished, Some(101));
    }

    /// `list_jobs(workspace_id=Some)` filters on the workspace
    /// column. Seed 2×3 jobs and assert.
    #[tokio::test]
    async fn list_jobs_filters_by_workspace() {
        let (pool, _dir) = fresh_pool().await;
        for (id, ws, ts) in [
            ("j-a1", "ws-A", 100_i64),
            ("j-a2", "ws-A", 200_i64),
            ("j-a3", "ws-A", 300_i64),
            ("j-b1", "ws-B", 110_i64),
            ("j-b2", "ws-B", 210_i64),
            ("j-b3", "ws-B", 310_i64),
        ] {
            sqlx::query(
                "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(ws)
            .bind("g")
            .bind(ts)
            .bind("done")
            .bind(0_i64)
            .execute(&pool)
            .await
            .expect("seed");
        }
        let a = list_jobs(&pool, Some("ws-A"), 50).await.expect("list A");
        assert_eq!(a.len(), 3);
        for s in &a {
            assert_eq!(s.workspace_id, "ws-A");
        }
        let b = list_jobs(&pool, Some("ws-B"), 50).await.expect("list B");
        assert_eq!(b.len(), 3);
        let all = list_jobs(&pool, None, 50).await.expect("list all");
        assert_eq!(all.len(), 6);
    }

    /// `list_jobs` truncates `goal` to 200 characters (char count,
    /// not byte count) so multi-byte Turkish text never gets split.
    #[tokio::test]
    async fn list_jobs_truncates_goal_to_200_chars() {
        let (pool, _dir) = fresh_pool().await;
        // 500-char goal that mixes ASCII + Turkish ç so byte length
        // != char length.
        let long_goal: String = "çş".repeat(250);
        assert_eq!(long_goal.chars().count(), 500);
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("j-long")
        .bind("ws")
        .bind(&long_goal)
        .bind(0_i64)
        .bind("done")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("seed");
        let summaries = list_jobs(&pool, None, 50).await.expect("list");
        assert_eq!(summaries.len(), 1);
        assert!(
            summaries[0].goal.chars().count() <= 200,
            "goal char count: {}",
            summaries[0].goal.chars().count()
        );
        assert_eq!(summaries[0].goal.chars().count(), 200);
    }

    /// `list_jobs` respects the limit argument.
    #[tokio::test]
    async fn list_jobs_respects_limit() {
        let (pool, _dir) = fresh_pool().await;
        for i in 0..100 {
            sqlx::query(
                "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(format!("j-{i:03}"))
            .bind("ws")
            .bind("g")
            .bind(i as i64)
            .bind("done")
            .bind(0_i64)
            .execute(&pool)
            .await
            .expect("seed");
        }
        let first = list_jobs(&pool, None, 10).await.expect("list");
        assert_eq!(first.len(), 10);
    }

    /// `list_jobs` orders results newest-first.
    #[tokio::test]
    async fn list_jobs_orders_by_created_desc() {
        let (pool, _dir) = fresh_pool().await;
        for (id, ts) in [
            ("j-old", 100_i64),
            ("j-mid", 200_i64),
            ("j-new", 300_i64),
        ] {
            sqlx::query(
                "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind("ws")
            .bind("g")
            .bind(ts)
            .bind("done")
            .bind(0_i64)
            .execute(&pool)
            .await
            .expect("seed");
        }
        let summaries = list_jobs(&pool, None, 50).await.expect("list");
        let ids: Vec<&str> =
            summaries.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["j-new", "j-mid", "j-old"]);
    }

    /// `get_job_detail` returns every stage in `idx` order.
    #[tokio::test]
    async fn get_job_detail_returns_full_stages() {
        let (pool, _dir) = fresh_pool().await;
        let reg = JobRegistry::with_pool(pool.clone());
        reg.try_acquire_workspace("ws", fixture_job("j-d", "g", 0))
            .await
            .expect("acquire");
        for state in [JobState::Scout, JobState::Plan, JobState::Build] {
            reg.update("j-d", |j| {
                j.stages.push(fixture_stage(state, 0.01, 10));
            })
            .await
            .expect("push stage");
        }
        let detail = get_job_detail(&pool, "j-d")
            .await
            .expect("query")
            .expect("Some");
        assert_eq!(detail.stages.len(), 3);
        assert_eq!(detail.stages[0].state, JobState::Scout);
        assert_eq!(detail.stages[1].state, JobState::Plan);
        assert_eq!(detail.stages[2].state, JobState::Build);
        assert!(detail.total_cost_usd > 0.0);
    }

    /// Unknown ids return `Ok(None)`.
    #[tokio::test]
    async fn get_job_detail_unknown_returns_none() {
        let (pool, _dir) = fresh_pool().await;
        let detail =
            get_job_detail(&pool, "j-nope").await.expect("query");
        assert!(detail.is_none());
    }

    /// `truncate_chars` short-circuits when the input fits.
    #[test]
    fn truncate_chars_passthrough() {
        let s = "abc";
        assert_eq!(truncate_chars(s, 10), "abc");
    }

    /// `truncate_chars` cuts on character boundaries.
    #[test]
    fn truncate_chars_cuts_on_codepoint() {
        let s = "abçdeş";
        let t = truncate_chars(s, 3);
        assert_eq!(t.chars().count(), 3);
        assert_eq!(t, "abç");
    }

    /// WP-W3-12d — a Failed job with a populated `last_verdict`
    /// round-trips through the registry → SQLite → store reload.
    /// The Verdict must reappear with bit-for-bit fidelity (issue
    /// list, severities, summary). Per-stage verdicts on Review /
    /// Test stages also survive.
    #[tokio::test]
    async fn verdict_persists_across_app_restart() {
        use crate::swarm::coordinator::verdict::{
            Verdict, VerdictIssue, VerdictSeverity,
        };
        let (pool, _dir) = fresh_pool().await;
        let reg = JobRegistry::with_pool(pool.clone());
        reg.try_acquire_workspace(
            "ws-verdict",
            fixture_job("j-verdict", "g", 0),
        )
        .await
        .expect("acquire");

        // Push a Review stage with a populated Verdict, then mark
        // the job Failed with `last_verdict` set — mirrors the
        // FSM's `finalize_failed_with_verdict` shape.
        let approved_review = Verdict {
            approved: true,
            issues: Vec::new(),
            summary: "looks fine".to_string(),
        };
        let rejected_test = Verdict {
            approved: false,
            issues: vec![VerdictIssue {
                severity: VerdictSeverity::High,
                file: Some("tests/foo.rs".to_string()),
                line: Some(7),
                message: "test_bar fails".to_string(),
            }],
            summary: "1 failure".to_string(),
        };
        let approved_review_clone = approved_review.clone();
        let rejected_test_clone = rejected_test.clone();
        reg.update("j-verdict", |j| {
            j.stages.push(StageResult {
                state: JobState::Review,
                specialist_id: "backend-reviewer".into(),
                assistant_text: "ok".into(),
                session_id: "s-r".into(),
                total_cost_usd: 0.001,
                duration_ms: 12,
                verdict: Some(approved_review_clone),
                coordinator_decision: None,
            });
            j.state = JobState::Failed;
            j.last_verdict = Some(rejected_test_clone);
        })
        .await
        .expect("update");

        // Reload through the read path — get_job_detail must surface
        // both the per-stage verdict and the job-level last_verdict.
        let detail = get_job_detail(&pool, "j-verdict")
            .await
            .expect("query")
            .expect("Some");
        assert_eq!(detail.state, JobState::Failed);
        assert_eq!(detail.last_verdict.as_ref(), Some(&rejected_test));
        assert_eq!(detail.stages.len(), 1);
        assert_eq!(
            detail.stages[0].verdict.as_ref(),
            Some(&approved_review)
        );
        assert_eq!(detail.last_error, None);
    }

    /// WP-W3-12f — a Classify stage with a populated
    /// `coordinator_decision` round-trips through the registry →
    /// SQLite → store reload. The decision must reappear with
    /// bit-for-bit fidelity (route + reasoning).
    #[tokio::test]
    async fn coordinator_decision_persists_across_app_restart() {
        use crate::swarm::coordinator::decision::{
            CoordinatorDecision, CoordinatorRoute,
        };
        let (pool, _dir) = fresh_pool().await;
        let reg = JobRegistry::with_pool(pool.clone());
        reg.try_acquire_workspace(
            "ws-decision",
            fixture_job("j-decision", "g", 0),
        )
        .await
        .expect("acquire");

        let decision = CoordinatorDecision {
            route: CoordinatorRoute::ResearchOnly,
            // W3-12g: scope is required on the wire shape; persisted
            // legacy rows default to Backend via serde.
            scope: crate::swarm::coordinator::decision::CoordinatorScope::Backend,
            reasoning: "explain-only goal; Scout findings cover it".into(),
        };
        let decision_clone = decision.clone();
        reg.update("j-decision", |j| {
            // First a Scout stage with no decision (canonical shape).
            j.stages.push(StageResult {
                state: JobState::Scout,
                specialist_id: "scout".into(),
                assistant_text: "scout findings".into(),
                session_id: "s-sc".into(),
                total_cost_usd: 0.001,
                duration_ms: 10,
                verdict: None,
                coordinator_decision: None,
            });
            // Then a Classify stage with the decision stamped on.
            j.stages.push(StageResult {
                state: JobState::Classify,
                specialist_id: "coordinator".into(),
                assistant_text: serde_json::to_string(&decision_clone)
                    .unwrap(),
                session_id: "s-cls".into(),
                total_cost_usd: 0.001,
                duration_ms: 5,
                verdict: None,
                coordinator_decision: Some(decision_clone),
            });
            j.state = JobState::Done;
        })
        .await
        .expect("update");

        let detail = get_job_detail(&pool, "j-decision")
            .await
            .expect("query")
            .expect("Some");
        assert_eq!(detail.stages.len(), 2);
        assert!(detail.stages[0].coordinator_decision.is_none());
        assert_eq!(
            detail.stages[1].coordinator_decision.as_ref(),
            Some(&decision)
        );
        assert_eq!(detail.stages[1].state, JobState::Classify);
    }

    /// WP-W3-12g — `CoordinatorDecision.scope` round-trips through
    /// SQLite (route + scope + reasoning all bit-for-bit). Sister
    /// test to `coordinator_decision_persists_across_app_restart`
    /// pinning the new `scope` field specifically — guards against
    /// future serializer drift dropping the field on the wire.
    #[tokio::test]
    async fn coordinator_decision_round_trips_through_sqlite_with_scope() {
        use crate::swarm::coordinator::decision::{
            CoordinatorDecision, CoordinatorRoute, CoordinatorScope,
        };
        let (pool, _dir) = fresh_pool().await;
        let reg = JobRegistry::with_pool(pool.clone());
        reg.try_acquire_workspace(
            "ws-scope",
            fixture_job("j-scope", "g", 0),
        )
        .await
        .expect("acquire");

        let decision = CoordinatorDecision {
            route: CoordinatorRoute::ExecutePlan,
            scope: CoordinatorScope::Frontend,
            reasoning: "frontend goal; execute via FE chain".into(),
        };
        let decision_clone = decision.clone();
        reg.update("j-scope", |j| {
            j.stages.push(StageResult {
                state: JobState::Classify,
                specialist_id: "coordinator".into(),
                assistant_text: serde_json::to_string(&decision_clone)
                    .unwrap(),
                session_id: "s-cls".into(),
                total_cost_usd: 0.001,
                duration_ms: 5,
                verdict: None,
                coordinator_decision: Some(decision_clone),
            });
            j.state = JobState::Done;
        })
        .await
        .expect("update");

        let detail = get_job_detail(&pool, "j-scope")
            .await
            .expect("query")
            .expect("Some");
        let reloaded = detail.stages[0]
            .coordinator_decision
            .as_ref()
            .expect("decision present after reload");
        // Field-by-field assertion so a regression on any of the
        // three points the failure at the right line.
        assert_eq!(reloaded.route, CoordinatorRoute::ExecutePlan);
        assert_eq!(reloaded.scope, CoordinatorScope::Frontend);
        assert_eq!(
            reloaded.reasoning,
            "frontend goal; execute via FE chain"
        );
        // Sanity: the entire struct round-trips by value too.
        assert_eq!(reloaded, &decision);
    }

    /// Migration 0008 adds `decision_json` to `swarm_stages`.
    /// Cheap schema-pragma probe so future migration drift surfaces
    /// here rather than mid-write.
    #[tokio::test]
    async fn migration_0008_adds_decision_column() {
        let (pool, _dir) = fresh_pool().await;
        let stage_cols: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('swarm_stages')",
        )
        .fetch_all(&pool)
        .await
        .expect("pragma swarm_stages");
        assert!(
            stage_cols.iter().any(|c| c == "decision_json"),
            "swarm_stages.decision_json missing; cols={stage_cols:?}"
        );
    }

    /// Migration 0007 actually adds the two new columns. Cheap
    /// schema-pragma probe so future migration drift surfaces here
    /// rather than mid-write.
    #[tokio::test]
    async fn migration_0007_adds_verdict_columns() {
        let (pool, _dir) = fresh_pool().await;
        let stage_cols: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('swarm_stages')",
        )
        .fetch_all(&pool)
        .await
        .expect("pragma swarm_stages");
        assert!(
            stage_cols.iter().any(|c| c == "verdict_json"),
            "swarm_stages.verdict_json missing; cols={stage_cols:?}"
        );
        let job_cols: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('swarm_jobs')",
        )
        .fetch_all(&pool)
        .await
        .expect("pragma swarm_jobs");
        assert!(
            job_cols.iter().any(|c| c == "last_verdict_json"),
            "swarm_jobs.last_verdict_json missing; cols={job_cols:?}"
        );
    }
}
