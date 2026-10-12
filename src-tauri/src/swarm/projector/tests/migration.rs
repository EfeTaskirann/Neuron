//! Migration 0011 (`swarm_jobs.source`) round-trip + serde default.

use crate::swarm::coordinator::Job;
use crate::test_support::mock_app_with_pool;

/// Migration 0011 adds `source` column with default 'fsm';
/// W3-shape Job JSON without `source` deserialises with
/// `source == "fsm"` (gotcha #3).
#[tokio::test]
async fn migration_0011_round_trip() {
    let (_, pool, _dir) = mock_app_with_pool().await;
    // Direct INSERT of a row with no `source` column reference
    // → migration default applies.
    sqlx::query(
        "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("j-mig-0011")
    .bind("ws")
    .bind("g")
    .bind(0_i64)
    .bind("done")
    .bind(0_i64)
    .execute(&pool)
    .await
    .expect("seed");
    let source: String = sqlx::query_scalar(
        "SELECT source FROM swarm_jobs WHERE id = ?",
    )
    .bind("j-mig-0011")
    .fetch_one(&pool)
    .await
    .expect("read source");
    assert_eq!(source, "fsm", "default backfill is 'fsm'");

    // Insert a row with explicit 'brain'.
    sqlx::query(
        "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("j-brain-0011")
    .bind("ws")
    .bind("g")
    .bind(0_i64)
    .bind("done")
    .bind(0_i64)
    .bind("brain")
    .execute(&pool)
    .await
    .expect("seed brain");
    let brain_source: String = sqlx::query_scalar(
        "SELECT source FROM swarm_jobs WHERE id = ?",
    )
    .bind("j-brain-0011")
    .fetch_one(&pool)
    .await
    .expect("read brain source");
    assert_eq!(brain_source, "brain");

    // Job struct deserialises from a W3-vintage JSON (no
    // `source` key) and surfaces 'fsm' via the serde default.
    let w3_shape = r#"{"id":"j-old","goal":"g","createdAtMs":0,"state":"init","retryCount":0,"stages":[],"lastError":null}"#;
    let job: Job = serde_json::from_str(w3_shape).expect("parse W3 JSON");
    assert_eq!(job.source, "fsm", "default_source=='fsm'");
}
