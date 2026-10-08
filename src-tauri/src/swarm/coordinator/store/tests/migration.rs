//! Schema-pragma probes — cheap checks so future migration drift
//! surfaces here rather than mid-write.

use crate::test_support::fresh_pool;

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
