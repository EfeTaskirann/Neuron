//! Write-through mutators + workspace-lock lifecycle for
//! [`JobRegistry`]. Every method here takes the in-memory critical
//! section first, then (when a pool is wired) awaits the SQLite
//! write-through, rolling the in-memory mutation back on SQL failure.
//! See the package-level docs (`registry/mod.rs`) for the lock order.

use crate::error::AppError;
use crate::swarm::coordinator::store;

use super::super::model::Job;
use super::JobRegistry;

impl JobRegistry {
    /// Atomically validate non-empty `workspace_id`, check no
    /// existing lock, register both the new job and its lock, and
    /// — if a pool is wired — write through to SQLite in one
    /// transaction. SQL failure rolls back the in-memory mutation.
    ///
    /// Returns:
    ///
    /// - `Ok(())` on success.
    /// - `Err(AppError::InvalidInput)` if `workspace_id` is empty
    ///   (after trim).
    /// - `Err(AppError::WorkspaceBusy { .. })` if the workspace
    ///   already has an in-flight job.
    /// - `Err(AppError::Internal(...))` if the DB tx failed; the
    ///   in-memory state is rolled back to the pre-call shape.
    pub async fn try_acquire_workspace(
        &self,
        workspace_id: &str,
        new_job: Job,
    ) -> Result<(), AppError> {
        let trimmed = workspace_id.trim();
        if trimmed.is_empty() {
            return Err(AppError::InvalidInput(
                "workspaceId must not be empty".into(),
            ));
        }

        // 1. In-memory mutation under one critical section.
        let job_for_db = {
            let mut locks = self
                .workspace_locks
                .lock()
                .expect("workspace_locks mutex poisoned");
            if let Some(existing) = locks.get(trimmed) {
                return Err(AppError::WorkspaceBusy {
                    workspace_id: trimmed.to_string(),
                    in_flight_job_id: existing.clone(),
                });
            }
            let mut jobs =
                self.jobs.lock().expect("jobs mutex poisoned");
            locks.insert(trimmed.to_string(), new_job.id.clone());
            jobs.insert(new_job.id.clone(), new_job.clone());
            new_job
        };

        // 2. SQL write-through (when pool is wired).
        if let Some(pool) = &self.pool {
            let now_ms = crate::time::now_millis();
            if let Err(e) = store::insert_job_and_lock(
                pool,
                &job_for_db,
                trimmed,
                now_ms,
            )
            .await
            {
                // Roll back the in-memory state so the caller
                // observes a consistent failure (no orphaned lock).
                let mut locks = self
                    .workspace_locks
                    .lock()
                    .expect("workspace_locks mutex poisoned");
                let mut jobs =
                    self.jobs.lock().expect("jobs mutex poisoned");
                locks.remove(trimmed);
                jobs.remove(&job_for_db.id);
                tracing::warn!(
                    job_id = %job_for_db.id,
                    workspace_id = %trimmed,
                    error = %e,
                    "swarm: try_acquire_workspace SQL failure; rolled back in-mem"
                );
                return Err(AppError::Internal(format!(
                    "swarm: failed to persist job/lock: {e}"
                )));
            }
        }
        Ok(())
    }

    /// Release the workspace lock for `(workspace_id, job_id)`.
    /// Idempotent: callers (the brain since W5-06; previously the
    /// W4-06 FSM and its Drop-driven `WorkspaceGuard`, both removed)
    /// invoke this on success and failure paths — calling twice (or
    /// against a workspace that another job has since taken over) is
    /// a no-op.
    ///
    /// On SQL failure the in-memory remove is **not** rolled back —
    /// the lock is gone from app state and the worst case is a
    /// stale row in `swarm_workspace_locks` that the next
    /// `recover_orphans` pass will sweep. Logged at `warn!` so the
    /// drift is visible.
    pub async fn release_workspace(&self, workspace_id: &str, job_id: &str) {
        let trimmed = workspace_id.trim();
        if trimmed.is_empty() {
            return;
        }
        // 1. In-memory remove (idempotent).
        let did_remove = {
            let mut locks = self
                .workspace_locks
                .lock()
                .expect("workspace_locks mutex poisoned");
            if locks.get(trimmed).map(String::as_str) == Some(job_id) {
                locks.remove(trimmed);
                true
            } else {
                false
            }
        };
        // 2. SQL delete (when pool wired). Run unconditionally — a
        //    stale DB row from a previous process is exactly what
        //    we want to clear, even if the in-memory map was
        //    already empty.
        if let Some(pool) = &self.pool {
            if let Err(e) =
                store::delete_workspace_lock(pool, trimmed).await
            {
                tracing::warn!(
                    workspace_id = %trimmed,
                    job_id = %job_id,
                    did_remove,
                    error = %e,
                    "swarm: release_workspace SQL delete failed; in-mem already cleared"
                );
            }
        }
    }

    /// Mutate the job under `id` in place. The closure receives
    /// `&mut Job` so callers can update any field. After the
    /// closure runs the registry re-serializes the job and (when
    /// pool is wired) UPDATEs its `swarm_jobs` row. New stages
    /// (delta vs. the pre-mutation `stages.len()`) get INSERTed
    /// into `swarm_stages`.
    ///
    /// On SQL failure the in-memory mutation is rolled back to the
    /// pre-call snapshot and the call surfaces `AppError::Internal`.
    pub async fn update<F>(&self, id: &str, f: F) -> Result<(), AppError>
    where
        F: FnOnce(&mut Job),
    {
        // 1. In-memory mutation. Snapshot the pre-call state so we
        //    can roll back if SQL fails.
        let (snapshot_before, snapshot_after, prev_len) = {
            let mut jobs =
                self.jobs.lock().expect("jobs mutex poisoned");
            let job = jobs.get_mut(id).ok_or_else(|| {
                AppError::NotFound(format!(
                    "swarm job `{id}` not in registry"
                ))
            })?;
            let before = job.clone();
            let prev_len = job.stages.len();
            f(job);
            let after = job.clone();
            (before, after, prev_len)
        };

        // 2. SQL write-through.
        if let Some(pool) = &self.pool {
            // 2a. UPDATE the job row.
            let now_ms = crate::time::now_millis();
            let finished_at_ms = if snapshot_after.state.is_terminal() {
                Some(now_ms)
            } else {
                None
            };
            if let Err(e) =
                store::update_job(pool, &snapshot_after, finished_at_ms)
                    .await
            {
                self.rollback_update(id, snapshot_before, &e);
                return Err(AppError::Internal(format!(
                    "swarm: failed to persist job update: {e}"
                )));
            }
            // 2b. INSERT each new stage (delta vs. prev_len).
            if snapshot_after.stages.len() > prev_len {
                for (idx_offset, stage) in snapshot_after
                    .stages
                    .iter()
                    .enumerate()
                    .skip(prev_len)
                {
                    let idx = idx_offset as u32;
                    if let Err(e) = store::insert_stage(
                        pool, id, idx, stage, now_ms,
                    )
                    .await
                    {
                        self.rollback_update(id, snapshot_before, &e);
                        return Err(AppError::Internal(format!(
                            "swarm: failed to persist stage {idx}: {e}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Restore the in-memory job to the pre-call snapshot and log
    /// the SQL failure that triggered the rollback. Internal helper
    /// for `update`.
    fn rollback_update(
        &self,
        id: &str,
        snapshot_before: Job,
        error: &AppError,
    ) {
        let mut jobs =
            self.jobs.lock().expect("jobs mutex poisoned");
        if let Some(slot) = jobs.get_mut(id) {
            *slot = snapshot_before;
        }
        tracing::warn!(
            job_id = %id,
            error = %error,
            "swarm: update SQL failure; rolled back in-mem"
        );
    }

    /// Sweep orphan jobs left non-terminal at process start. Called
    /// once from `lib.rs::setup` BEFORE `app.manage(registry)`.
    ///
    /// Three steps under the hood (see `store::recover_orphans`):
    ///   1. UPDATE `swarm_jobs` SET state='failed',
    ///      last_error='interrupted by app restart',
    ///      finished_at_ms=:now WHERE state NOT IN ('done','failed').
    ///   2. DELETE FROM `swarm_workspace_locks`.
    ///   3. SELECT recent rows to hydrate the in-memory cache.
    ///
    /// The in-memory hydration is capped at 100 rows (newest-first)
    /// so a long-lived install doesn't OOM. The IPC `swarm:list_jobs`
    /// hits the DB, not the cache, so this cap doesn't bound the
    /// history surface.
    pub async fn recover_orphans(&self) -> Result<u32, AppError> {
        let Some(pool) = &self.pool else {
            return Ok(0);
        };
        let now_ms = crate::time::now_millis();
        let recovered = store::recover_orphans(pool, now_ms).await?;
        // Hydrate the in-memory cache with the recovered jobs (which
        // are now Failed) plus the latest 100 terminal jobs. The
        // recovered set goes in first so that `get(job_id)` for a
        // freshly-recovered orphan hits the cache instead of round-
        // tripping to the DB.
        {
            let mut jobs =
                self.jobs.lock().expect("jobs mutex poisoned");
            for job in &recovered.recovered {
                jobs.insert(job.id.clone(), job.clone());
            }
        }
        // Pull the most-recent slice (deduped against the recovered
        // set) so the read-paths see warm rows. List-shaped: not
        // capped on workspace, just newest-first.
        let warm = store::list_recent_jobs_full(pool, 100).await?;
        {
            let mut jobs =
                self.jobs.lock().expect("jobs mutex poisoned");
            for job in warm {
                jobs.entry(job.id.clone()).or_insert(job);
            }
        }
        Ok(recovered.count)
    }
}
