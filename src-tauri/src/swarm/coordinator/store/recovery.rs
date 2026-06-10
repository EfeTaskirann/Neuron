//! Process-start orphan recovery for the swarm Coordinator. The
//! sweep flips every non-terminal job row to `Failed`, clears the
//! workspace locks, and hydrates `Job` snapshots (via `read.rs`) so
//! the registry can warm its in-memory cache.

use sqlx::Row;

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::job::Job;

use super::read::{detail_to_job, get_job_detail};

/// Result of a `recover_orphans` sweep. `count` is the number of
/// non-terminal rows the sweep flipped to `Failed`; `recovered`
/// carries the corresponding `Job` snapshots so the caller can
/// hydrate the in-memory cache.
#[derive(Debug)]
pub(in crate::swarm::coordinator) struct RecoveredOrphans {
    pub count: u32,
    pub recovered: Vec<Job>,
}

/// Sweep orphan jobs left non-terminal at process start. Three
/// steps under the hood:
///
/// 1. SELECT every non-terminal row's id (so we can hydrate the
///    cache after the UPDATE without an extra round-trip).
/// 2. UPDATE the rows to `Failed`.
/// 3. DELETE every workspace_lock row (cascade-safe; the job rows
///    survive in `Failed` state).
pub(in crate::swarm::coordinator) async fn recover_orphans(
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
