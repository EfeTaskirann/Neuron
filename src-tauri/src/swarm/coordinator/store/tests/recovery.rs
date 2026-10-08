//! Process-start orphan sweep: `recover_orphans` flips non-terminal
//! rows to Failed, clears workspace locks, and leaves terminal rows
//! alone.

use super::super::recover_orphans;
use crate::test_support::fresh_pool;

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
