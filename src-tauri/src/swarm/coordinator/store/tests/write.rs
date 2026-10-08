//! Write-side persistence: `JobRegistry`-driven INSERT/UPDATE of
//! `swarm_jobs` / `swarm_stages` and the workspace-lock bookkeeping.

use super::{fixture_job, fixture_stage};
use crate::swarm::coordinator::job::JobState;
use crate::swarm::coordinator::JobRegistry;
use crate::test_support::fresh_pool;

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
