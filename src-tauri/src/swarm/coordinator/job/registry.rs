//! `JobRegistry` — in-memory job + workspace-lock store with optional
//! SQLite write-through (WP-W3-12a §4 / WP-W3-12b §2/§3).
//!
//! The registry owns the **per-workspace lock** map. Per the owner
//! directive 2026-05-05 ("Aynı proje için yeni bir 9 kişilik ekibi
//! çalıştırmama izin vermesin, başka bir proje için izin versin."),
//! `swarm:run_job` calls with the same `workspace_id` serialize
//! (second one rejected with `AppError::WorkspaceBusy`), while
//! different `workspace_id`s run independently in parallel.
//!
//! Lock acquisition order is `workspace_locks` → `jobs`, consistent
//! across every method. Two independent mutexes keep job mutations
//! that don't touch lock state cheap, but every acquire-or-release
//! traverses both in the same order so two threads can never spin in
//! a deadlock.
//!
//! W3-12b adds an optional `Option<DbPool>` so the registry doubles
//! as a write-through to `swarm_jobs` / `swarm_stages` /
//! `swarm_workspace_locks`. Mutators (`try_acquire_workspace`,
//! `update`, `release_workspace`) become `async fn` and await SQL
//! inline. SQL failure rolls back the in-memory mutation and surfaces
//! `AppError::Internal`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::store;

use super::model::{Job, JobDetail, JobSummary};

/// In-memory job + workspace-lock registry, optionally backed by
/// SQLite write-through (W3-12b).
///
/// `Mutex` (std, not async) is fine here — every operation is
/// short and never `await`s while holding the lock. The async
/// mutators acquire the in-memory lock, mutate, drop it, *then*
/// await SQL.
///
/// The two maps are independent so a job mutation that doesn't
/// change workspace state (e.g. appending a `StageResult`) never
/// contests the `workspace_locks` mutex.
///
/// Lock acquisition order — both internal helpers and external
/// callers MUST follow `workspace_locks` first, then `jobs`. The
/// constructor / accessors that touch only one map are exempt.
/// **W3-12b extension**: with write-through, the order is now
/// `workspace_locks (in-mem) → jobs (in-mem) → DB tx`. All
/// in-memory locks are released BEFORE awaiting on the DB.
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Job>>,
    /// `workspace_id` → `job_id` of the in-flight job currently
    /// holding the workspace. Removed on `release_workspace`.
    workspace_locks: Mutex<HashMap<String, String>>,
    /// Per-job cancellation notify (W3-12c). Process-local; never
    /// persisted to SQLite.
    cancel_notifies: Mutex<HashMap<String, Arc<Notify>>>,
    /// Optional pool — `None` when the registry is in-memory only
    /// (test bench), `Some` in production. The mutators branch on
    /// `pool.as_ref()` so the same surface drives both modes.
    pool: Option<DbPool>,
}

impl JobRegistry {
    /// Build an in-memory-only registry. Used by tests and as the
    /// fallback for any call site that does not yet have access to
    /// the pool. Production code paths must use [`Self::with_pool`].
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            workspace_locks: Mutex::new(HashMap::new()),
            cancel_notifies: Mutex::new(HashMap::new()),
            pool: None,
        }
    }

    /// Build a SQLite-backed registry. Every successful in-memory
    /// mutation is mirrored to `swarm_jobs` / `swarm_stages` /
    /// `swarm_workspace_locks`. SQL failure rolls back the
    /// in-memory mutation and surfaces `AppError::Internal`.
    pub fn with_pool(pool: DbPool) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            workspace_locks: Mutex::new(HashMap::new()),
            cancel_notifies: Mutex::new(HashMap::new()),
            pool: Some(pool),
        }
    }

    /// Test/inspection accessor — returns whether SQL write-through
    /// is enabled for this registry. Internal helpers in the FSM
    /// don't need this, but tests use it to assert that
    /// `with_pool(...)` actually wired through.
    #[cfg(test)]
    pub(crate) fn has_pool(&self) -> bool {
        self.pool.is_some()
    }

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
    /// Idempotent: the FSM always calls this on the success and
    /// failure paths, and a `Drop`-driven defensive call from
    /// `WorkspaceGuard` may also fire — calling twice (or against a
    /// workspace that another job has since taken over) is a no-op.
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

    /// Snapshot of the job under `id`, or `None`. Returns an owned
    /// `Job` so the caller doesn't hold the registry lock past the
    /// call. Reads the in-memory cache only — SQLite-backed history
    /// queries go through `commands::swarm::swarm_get_job`.
    pub fn get(&self, id: &str) -> Option<Job> {
        let jobs = self.jobs.lock().expect("jobs mutex poisoned");
        jobs.get(id).cloned()
    }

    /// Snapshot of every job in the in-memory cache. Order
    /// unspecified.
    pub fn list(&self) -> Vec<Job> {
        let jobs = self.jobs.lock().expect("jobs mutex poisoned");
        jobs.values().cloned().collect()
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

    // ------------------------------------------------------------- //
    // WP-W3-12c — cancel-notify surface                              //
    // ------------------------------------------------------------- //
    //
    // LOCK ORDER: `workspace_locks → cancel_notifies → jobs`. The
    // three methods below each hold *only* the `cancel_notifies`
    // mutex while running; they never acquire `workspace_locks` or
    // `jobs` while holding it, so they cannot deadlock against the
    // existing acquire/release/update/get methods (which hold at
    // most one of the three at a time).

    /// Register a cancellation `Notify` for the in-flight `job_id`.
    /// Process-local; never persisted (see W3-12b "cancel state is
    /// process-local").
    pub fn register_cancel(
        &self,
        job_id: &str,
        notify: Arc<Notify>,
    ) -> Result<(), AppError> {
        let mut notifies = self
            .cancel_notifies
            .lock()
            .expect("cancel_notifies mutex poisoned");
        if notifies.contains_key(job_id) {
            return Err(AppError::Conflict(format!(
                "swarm job `{job_id}` already has a cancel notify registered"
            )));
        }
        notifies.insert(job_id.to_string(), notify);
        Ok(())
    }

    /// Idempotent remove of the cancel notify for `job_id`. Called
    /// on every FSM tail (success, failure, cancellation) plus by
    /// the `CancelGuard` Drop seatbelt — calling twice (or against
    /// an unknown id) is a no-op.
    pub fn unregister_cancel(&self, job_id: &str) {
        let mut notifies = self
            .cancel_notifies
            .lock()
            .expect("cancel_notifies mutex poisoned");
        notifies.remove(job_id);
    }

    /// Signal cancellation for the given `job_id`.
    ///
    /// W3-12j: switched from `notify_one()` to `notify_waiters()` so
    /// every task currently `await`-ing the per-job `Notify` wakes —
    /// the Fullstack parallel dispatch races two `tokio::select!`s on
    /// the same `Notify`, and `notify_one` would only wake the first.
    /// `notify_waiters` is a no-op when there are no current waiters
    /// (no permit is stored), but every FSM stage call sequences
    /// `notified()` on the same handle before any awaitable point, so
    /// the parallel-track waiters are always registered by the time a
    /// user-driven cancel arrives.
    pub fn signal_cancel(&self, job_id: &str) -> Result<(), AppError> {
        let notifies = self
            .cancel_notifies
            .lock()
            .expect("cancel_notifies mutex poisoned");
        match notifies.get(job_id) {
            Some(notify) => {
                notify.notify_waiters();
                Ok(())
            }
            None => Err(AppError::NotFound(format!(
                "swarm job `{job_id}` has no in-flight cancel notify"
            ))),
        }
    }

    /// Accessor used by the IPC layer to read history rows directly
    /// from the DB. Returns `None` when the registry is in-memory
    /// only.
    pub fn pool(&self) -> Option<&DbPool> {
        self.pool.as_ref()
    }

    /// List recent jobs from the persisted history. The IPC layer
    /// (`swarm:list_jobs`) calls through here so the SQL helpers
    /// stay `pub(super)`. Without a pool, returns an empty Vec —
    /// the in-memory cache is the FSM's working state, not a
    /// history surface.
    pub async fn list_jobs(
        &self,
        workspace_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JobSummary>, AppError> {
        let Some(pool) = &self.pool else {
            return Ok(Vec::new());
        };
        store::list_jobs(pool, workspace_id, limit).await
    }

    /// Fetch one job detail (job + stages) from the persisted
    /// history. Used by `swarm:get_job`. `Ok(None)` is the
    /// canonical "unknown id" signal — the IPC layer maps to
    /// `AppError::NotFound`.
    pub async fn get_job_detail(
        &self,
        job_id: &str,
    ) -> Result<Option<JobDetail>, AppError> {
        let Some(pool) = &self.pool else {
            return Ok(None);
        };
        store::get_job_detail(pool, job_id).await
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}
