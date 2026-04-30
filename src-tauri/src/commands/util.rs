//! Shared helpers for the command surface.
//!
//! Currently:
//! - [`finalise_run_with`] — atomic run finalisation. Extracted
//!   from `runs.rs:runs_create` rollback (refactor.md §4
//!   "Compensating-action pattern'i inline yazılmış").
//! - [`update_run_aggregates`] — recompute `runs.tokens` and
//!   `runs.cost_usd` by summing `attrs_json` token/cost fields across
//!   the run's child spans. Per WP-W2-07 §"Scope" it runs after every
//!   `span.closed`. Idempotent.

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

/// Recompute `runs.tokens` and `runs.cost_usd` for `run_id` from its
/// child spans' `attrs_json` payloads. Called after each
/// `span.closed` event so the run inspector's totals stay in step
/// with completed work.
///
/// `tokens` = SUM(tokens_in) + SUM(tokens_out); `cost_usd` =
/// SUM(cost). Missing JSON keys contribute 0 (IFNULL CAST). The
/// outer COALESCE on the SUM guards against the case where the run
/// has no spans yet — `SUM` over zero rows is `NULL`, which would
/// violate the `NOT NULL DEFAULT 0` schema constraint.
///
/// Idempotent: running twice on the same `run_id` produces the same
/// totals because the source-of-truth is the spans table, not a
/// running counter.
pub async fn update_run_aggregates(pool: &DbPool, run_id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runs SET \
            tokens = ( \
                SELECT COALESCE(SUM( \
                    IFNULL(CAST(json_extract(attrs_json, '$.tokens_in')  AS INTEGER), 0) + \
                    IFNULL(CAST(json_extract(attrs_json, '$.tokens_out') AS INTEGER), 0) \
                ), 0) FROM runs_spans WHERE run_id = ?1 \
            ), \
            cost_usd = ( \
                SELECT COALESCE(SUM( \
                    IFNULL(CAST(json_extract(attrs_json, '$.cost') AS REAL), 0) \
                ), 0) FROM runs_spans WHERE run_id = ?1 \
            ) \
         WHERE id = ?1",
    )
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

    /// Helper: insert a workflow + run + N spans with given attrs_json
    /// payloads. Each span gets a unique id and a sequential t0_ms so
    /// FK + ordering hold.
    async fn seed_run_with_spans(pool: &DbPool, attrs: &[&str]) {
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status, tokens, cost_usd) \
             VALUES ('r-1','w1','Daily summary',1000,'running',0,0)",
        )
        .execute(pool)
        .await
        .unwrap();
        for (i, attrs_json) in attrs.iter().enumerate() {
            sqlx::query(
                "INSERT INTO runs_spans (id, run_id, name, type, t0_ms, attrs_json, is_running) \
                 VALUES (?, 'r-1', 'span', 'llm', ?, ?, 0)",
            )
            .bind(format!("s-{i}"))
            .bind(i as i64 * 100)
            .bind(*attrs_json)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// WP-W2-07: aggregates SUM the `tokens_in + tokens_out` and
    /// `cost` JSON fields across all child spans. Span A has both
    /// in/out, span B has only in — both contribute correctly.
    #[tokio::test]
    async fn update_run_aggregates_sums_tokens_and_cost() {
        let (pool, _dir) = fresh_pool().await;
        seed_run_with_spans(
            &pool,
            &[
                r#"{"tokens_in":10,"tokens_out":5,"cost":0.001}"#,
                r#"{"tokens_in":3,"cost":0.002}"#,
            ],
        )
        .await;

        update_run_aggregates(&pool, "r-1").await.unwrap();

        let (tokens, cost): (i64, f64) =
            sqlx::query_as("SELECT tokens, cost_usd FROM runs WHERE id='r-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tokens, 10 + 5 + 3, "tokens_in + tokens_out summed");
        assert!(
            (cost - 0.003).abs() < 1e-9,
            "cost summed (got {cost})"
        );
    }

    /// A span whose attrs_json has none of the expected keys
    /// (e.g. a tool span without token accounting) contributes 0
    /// rather than NULL-poisoning the SUM. Defends the IFNULL CAST
    /// chain inside the aggregate query.
    #[tokio::test]
    async fn update_run_aggregates_handles_missing_keys_as_zero() {
        let (pool, _dir) = fresh_pool().await;
        seed_run_with_spans(
            &pool,
            &[
                "{}",
                r#"{"tokens_in":7}"#,
                r#"{"unrelated":"data"}"#,
            ],
        )
        .await;

        update_run_aggregates(&pool, "r-1").await.unwrap();

        let (tokens, cost): (i64, f64) =
            sqlx::query_as("SELECT tokens, cost_usd FROM runs WHERE id='r-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tokens, 7, "only the one populated tokens_in counts");
        assert_eq!(cost, 0.0, "no cost keys → 0.0, not NULL");
    }

    /// Aggregates are derived from the spans table on every call,
    /// so running the helper twice produces identical totals — no
    /// double-counting like a running-counter approach would risk.
    #[tokio::test]
    async fn update_run_aggregates_idempotent() {
        let (pool, _dir) = fresh_pool().await;
        seed_run_with_spans(
            &pool,
            &[r#"{"tokens_in":100,"tokens_out":50,"cost":0.5}"#],
        )
        .await;

        update_run_aggregates(&pool, "r-1").await.unwrap();
        update_run_aggregates(&pool, "r-1").await.unwrap();

        let (tokens, cost): (i64, f64) =
            sqlx::query_as("SELECT tokens, cost_usd FROM runs WHERE id='r-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tokens, 150);
        assert!((cost - 0.5).abs() < 1e-9);
    }
}
