//! Read-side persistence: `list_jobs` filtering/truncation/limit/order
//! and `get_job_detail` full-stage hydration.

use super::super::{get_job_detail, list_jobs};
use super::{fixture_job, fixture_stage};
use crate::swarm::coordinator::job::JobState;
use crate::swarm::coordinator::JobRegistry;
use crate::test_support::fresh_pool;

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
