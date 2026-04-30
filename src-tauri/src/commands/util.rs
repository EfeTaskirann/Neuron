//! Shared helpers for the command surface.
//!
//! Currently:
//! - [`finalise_run_with`] — atomic run finalisation. Extracted
//!   from `runs.rs:runs_create` rollback (refactor.md §4
//!   "Compensating-action pattern'i inline yazılmış").

use crate::db::DbPool;
use crate::error::AppError;
use crate::time::now_millis;

/// Mark a run as `status` (typically `"error"` for compensating
/// rollback or `"cancelled"` for user-driven cancel) iff it is
/// currently `"running"`. Computes `duration_ms` from the row's
/// `started_at` if not already set.
///
/// Atomic: the `WHERE status = 'running'` guard prevents this from
/// overwriting a sidecar-driven success/error finalisation that
/// already landed.
pub async fn finalise_run_with(
    pool: &DbPool,
    run_id: &str,
    status: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runs SET \
            status = ?, \
            duration_ms = COALESCE(duration_ms, ? - started_at * 1000) \
         WHERE id = ? AND status = 'running'",
    )
    .bind(status)
    .bind(now_millis())
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fresh_pool;

    #[tokio::test]
    async fn finalise_run_with_marks_running_run_as_error() {
        let (pool, _dir) = fresh_pool().await;
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
             VALUES ('r-1','w1','Daily summary',1000,'running')",
        )
        .execute(&pool)
        .await
        .unwrap();

        finalise_run_with(&pool, "r-1", "error").await.unwrap();

        let (status, dur): (String, Option<i64>) =
            sqlx::query_as("SELECT status, duration_ms FROM runs WHERE id='r-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "error");
        assert!(dur.is_some(), "duration_ms must be filled");
    }

    #[tokio::test]
    async fn finalise_run_with_does_not_overwrite_completed_run() {
        let (pool, _dir) = fresh_pool().await;
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status, duration_ms) \
             VALUES ('r-1','w1','Daily summary',1000,'success',2400)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Try to flip an already-success run to error — must be no-op
        // because the WHERE status='running' guard rejects it.
        finalise_run_with(&pool, "r-1", "error").await.unwrap();

        let status: String =
            sqlx::query_scalar("SELECT status FROM runs WHERE id='r-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "success", "completed run must not be reverted");
    }
}
