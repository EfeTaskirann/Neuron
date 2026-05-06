//! Job state types + registry (WP-W3-12a §2 / §4 + WP-W3-12b §2/§3).
//!
//! `Job`, `JobState`, `JobOutcome`, and `StageResult` cross the IPC
//! boundary as the FSM's contract with the frontend. `JobRegistry` is
//! the in-memory store with optional SQLite write-through (W3-12b).
//!
//! The registry also owns the **per-workspace lock** map. Per the
//! owner directive 2026-05-05 ("Aynı proje için yeni bir 9 kişilik
//! ekibi çalıştırmama izin vermesin, başka bir proje için izin
//! versin."), `swarm:run_job` calls with the same `workspace_id`
//! serialize (second one rejected with `AppError::WorkspaceBusy`),
//! while different `workspace_id`s run independently in parallel.
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

use serde::{Deserialize, Serialize};
use specta::Type;
use tokio::sync::Notify;

use crate::db::DbPool;
use crate::error::AppError;

use super::decision::CoordinatorDecision;
use super::store;
use super::verdict::Verdict;

/// Lifecycle states of a swarm job. Per WP §2:
///
/// - `Init` — newly minted, before the first transition fires.
/// - `Scout` — read-only investigation stage.
/// - `Classify` — single-shot Coordinator brain decision (W3-12f);
///   sits between Scout and Plan so the FSM can short-circuit on
///   research-only goals. The variant is reachable on every job;
///   ResearchOnly takes the Done short-circuit, ExecutePlan falls
///   through to Plan.
/// - `Plan` / `Build` — the next two happy-path stages on the
///   ExecutePlan branch.
/// - `Review` / `Test` — Verdict-gated quality stages (W3-12d).
/// - `Done` / `Failed` — terminal.
///
/// `Hash` + `Eq` are derived so the FSM can build small lookup
/// tables keyed on the state if it ever needs to (W3-12d).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Init,
    Scout,
    /// Coordinator brain routing decision (W3-12f). Single-shot; the
    /// FSM enters this state once per job between Scout and Plan.
    Classify,
    Plan,
    Build,
    /// Verdict gate (W3-12d). FSM enters this state after Build on
    /// the ExecutePlan branch.
    Review,
    /// Verdict gate (W3-12d). FSM enters this state after a
    /// Review-approved verdict.
    Test,
    Done,
    /// Terminal failure state. Carries the last error in
    /// `Job.last_error`.
    Failed,
}

impl JobState {
    /// Stable string used in the `swarm_jobs.state` column. The
    /// repr matches `serde(rename_all = "snake_case")` on the
    /// JS-facing wire enum so DB values and frontend values agree
    /// (which simplifies the W3-14 hook).
    pub fn as_db_str(&self) -> &'static str {
        match self {
            JobState::Init => "init",
            JobState::Scout => "scout",
            JobState::Classify => "classify",
            JobState::Plan => "plan",
            JobState::Build => "build",
            JobState::Review => "review",
            JobState::Test => "test",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }

    /// Inverse of [`Self::as_db_str`]. Unknown discriminants surface
    /// as `AppError::Internal` so a corrupted DB never silently maps
    /// to a default state.
    pub fn from_db_str(s: &str) -> Result<Self, AppError> {
        match s {
            "init" => Ok(JobState::Init),
            "scout" => Ok(JobState::Scout),
            "classify" => Ok(JobState::Classify),
            "plan" => Ok(JobState::Plan),
            "build" => Ok(JobState::Build),
            "review" => Ok(JobState::Review),
            "test" => Ok(JobState::Test),
            "done" => Ok(JobState::Done),
            "failed" => Ok(JobState::Failed),
            other => Err(AppError::Internal(format!(
                "unknown swarm job state in DB: {other}"
            ))),
        }
    }

    /// Whether this state is terminal (`Done` or `Failed`).
    /// Used by the recovery sweep to leave already-finalized rows
    /// alone.
    pub fn is_terminal(&self) -> bool {
        matches!(self, JobState::Done | JobState::Failed)
    }
}

/// Output of one completed FSM stage. Append-only — the FSM pushes
/// one entry per stage that produced a result (success path) and
/// stops on the first failure (which still records the failed
/// stage's name in `Job.last_error` but does NOT push a `StageResult`
/// for the failed stage in W3-12a — see `CoordinatorFsm::run_job`).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StageResult {
    /// Which lifecycle stage produced this result. One of
    /// `Scout` / `Plan` / `Build` in 12a.
    pub state: JobState,
    /// `Profile.id` of the specialist that ran the stage —
    /// `"scout"` / `"planner"` / `"backend-builder"`.
    pub specialist_id: String,
    /// The specialist's final assistant text (the `result` event's
    /// `result` field; running deltas already concatenated).
    pub assistant_text: String,
    /// Subprocess session id (`system.init` event). Useful for
    /// W3-12b's chat-history persistence.
    pub session_id: String,
    /// Cost reported by the `claude` `result.success` event. Sums
    /// into `JobOutcome.total_cost_usd`.
    pub total_cost_usd: f64,
    /// Wall-clock duration of this stage's invoke, measured around
    /// the `transport.invoke` await. Sums into
    /// `JobOutcome.total_duration_ms`.
    pub duration_ms: u64,
    /// Parsed Verdict (W3-12d). Populated only for the `Review`
    /// and `Test` stages — Scout / Plan / Build leave this `None`.
    /// `serde(default)` lets older persisted JSON (no `verdict`
    /// key) deserialize unchanged.
    #[serde(default)]
    pub verdict: Option<Verdict>,
    /// Parsed Coordinator brain decision (W3-12f). Populated only
    /// for the `Classify` stage — every other stage leaves this
    /// `None`. `serde(default)` lets older persisted JSON (no
    /// `coordinator_decision` key) deserialize unchanged.
    #[serde(default)]
    pub coordinator_decision: Option<CoordinatorDecision>,
}

/// One in-flight (or completed) swarm job. The registry indexes by
/// `id`; lookup is exposed via `JobRegistry::get`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// ULID with `j-` prefix per ADR-0007 (e.g. `j-01H8...`).
    pub id: String,
    /// User-supplied free-text goal driving the chain.
    pub goal: String,
    /// Unix epoch milliseconds at job creation (per Charter
    /// timestamp invariant: `_ms` suffix → milliseconds).
    pub created_at_ms: i64,
    /// Current lifecycle state.
    pub state: JobState,
    /// Wired but unused in 12a; W3-12d's Verdict-gated retry loop
    /// reads this to decide whether to short-circuit on persistent
    /// failures past `MAX_RETRIES`.
    pub retry_count: u32,
    /// Append-only list of completed stages. One entry per
    /// successful stage; on failure the failing stage is NOT
    /// appended (its error rides in `last_error`).
    pub stages: Vec<StageResult>,
    /// Populated when `state == Failed`. None on the happy path.
    pub last_error: Option<String>,
    /// Parsed Verdict (W3-12d). Populated only when the FSM
    /// finalized the job as Failed because a Reviewer or Tester
    /// returned `approved=false`. The Verdict IS the structured
    /// error, so on this branch `last_error` stays `None`.
    #[serde(default)]
    pub last_verdict: Option<Verdict>,
}

impl Job {
    /// Walk `stages` newest-first looking for the most recent
    /// Review/Test entry whose `verdict` came back rejected. Used by
    /// the W3-12e retry loop to label the prior gate ("Reviewer" /
    /// "IntegrationTester") in the retry-Plan prompt.
    ///
    /// Derived rather than stored so the Plan-on-retry path doesn't
    /// require a new SQL column or a parallel field that can drift
    /// out of sync with the persisted `stages` rows. `last_verdict`
    /// alone tells *what* was rejected; this helper tells *which
    /// gate* did the rejecting.
    pub fn last_rejecting_gate(&self) -> Option<JobState> {
        for stage in self.stages.iter().rev() {
            if !matches!(stage.state, JobState::Review | JobState::Test) {
                continue;
            }
            if let Some(verdict) = &stage.verdict {
                if verdict.rejected() {
                    return Some(stage.state);
                }
            }
        }
        None
    }
}

/// Final outcome returned by `swarm:run_job`. Mirrors `Job` minus
/// the lifecycle bookkeeping fields the IPC caller doesn't need
/// (no `state` mid-run, no `created_at_ms` since the wall-clock
/// data is encoded in `total_duration_ms`).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JobOutcome {
    pub job_id: String,
    /// Always `Done` or `Failed` — the FSM never returns mid-state.
    pub final_state: JobState,
    pub stages: Vec<StageResult>,
    /// `Some` on `Failed`, `None` on `Done`.
    pub last_error: Option<String>,
    /// Sum of `StageResult.total_cost_usd` across `stages`.
    pub total_cost_usd: f64,
    /// Sum of `StageResult.duration_ms` across `stages`.
    pub total_duration_ms: u64,
    /// Parsed Verdict (W3-12d). Populated when the FSM finalized
    /// the job as Failed because a Reviewer or Tester verdict came
    /// back rejected. `None` on the happy path and on stage-error
    /// failures.
    #[serde(default)]
    pub last_verdict: Option<Verdict>,
}

/// Slim wire-shape returned by `swarm:list_jobs` (WP-W3-12b §4).
/// Drops the per-stage `assistant_text` / `session_id` payload so
/// the recent-jobs panel can render N jobs without N × per-stage
/// payload bloat.
///
/// `goal` is **char**-truncated to 200 chars at the SQL helper
/// layer (not byte-truncated — Turkish characters!) so the IPC
/// always returns a renderable string of bounded size.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub id: String,
    pub workspace_id: String,
    pub goal: String,
    pub created_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub state: JobState,
    pub stage_count: u32,
    pub total_cost_usd: f64,
    pub last_error: Option<String>,
}

/// Full job-detail wire-shape returned by `swarm:get_job` (WP-W3-12b §4).
/// Same fields as `Job` plus the aggregated `total_cost_usd` /
/// `total_duration_ms` and `finished_at_ms` pulled from the DB row.
///
/// `Job` itself stays internal to the FSM so the in-memory
/// bookkeeping fields (`retry_count` for W3-12d, etc.) don't have
/// to ship to the wire before they have a frontend consumer.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JobDetail {
    pub id: String,
    pub workspace_id: String,
    pub goal: String,
    pub created_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub state: JobState,
    pub retry_count: u32,
    pub stages: Vec<StageResult>,
    pub last_error: Option<String>,
    pub total_cost_usd: f64,
    pub total_duration_ms: u64,
    /// Parsed Verdict (W3-12d). Mirrors `Job.last_verdict` —
    /// populated only when the FSM finalized the job as Failed
    /// because a Reviewer or Tester verdict came back rejected.
    #[serde(default)]
    pub last_verdict: Option<Verdict>,
}

impl JobDetail {
    /// Build a detail wire-shape from a `Job` snapshot + the
    /// `workspace_id` it was created for + the `finished_at_ms`
    /// the DB persisted on the terminal transition. Aggregates
    /// `total_cost_usd` / `total_duration_ms` from `stages` so the
    /// sums always reflect the actual stage rows (not an out-of-
    /// sync cached value).
    pub fn from_job(
        job: Job,
        workspace_id: String,
        finished_at_ms: Option<i64>,
    ) -> Self {
        let total_cost_usd: f64 =
            job.stages.iter().map(|s| s.total_cost_usd).sum();
        let total_duration_ms: u64 =
            job.stages.iter().map(|s| s.duration_ms).sum();
        Self {
            id: job.id,
            workspace_id,
            goal: job.goal,
            created_at_ms: job.created_at_ms,
            finished_at_ms,
            state: job.state,
            retry_count: job.retry_count,
            stages: job.stages,
            last_error: job.last_error,
            total_cost_usd,
            total_duration_ms,
            last_verdict: job.last_verdict,
        }
    }
}

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

// --------------------------------------------------------------------- //
// WP-W3-12c — streaming event surface                                    //
// --------------------------------------------------------------------- //

/// Per-job lifecycle event streamed to `swarm:job:{job_id}:event`.
///
/// One event name carries every transition in the FSM via a
/// `kind` tag (matches W3-06's `runs:{id}:span` pattern). Frontend
/// subscribers register one listener per job and switch on `kind`.
///
/// Order on the happy path:
///
///   `started → stage_started(scout) → stage_completed(scout)
///           → stage_started(plan)  → stage_completed(plan)
///           → stage_started(build) → stage_completed(build)
///           → finished`
///
/// On a stage error: `stage_started(stage) → finished` (no
/// `stage_completed` for the failing stage).
///
/// On cancellation: `… → stage_started(stage) → cancelled → finished`.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmJobEvent {
    /// Fires once at FSM start, after the workspace lock is
    /// acquired and the cancel notify is registered, before any
    /// stage spawns.
    Started {
        job_id: String,
        workspace_id: String,
        goal: String,
        created_at_ms: i64,
    },
    /// Fires before every stage's `transport.invoke` is awaited.
    /// `state` is the upcoming lifecycle stage (Scout / Plan /
    /// Build); `prompt_preview` is the first 200 *chars* of the
    /// rendered prompt (char-bounded so multi-byte Turkish text
    /// is never split mid-codepoint).
    StageStarted {
        job_id: String,
        state: JobState,
        specialist_id: String,
        prompt_preview: String,
    },
    /// Fires after a stage's `StageResult` is built and pushed to
    /// the registry, on the success path only.
    StageCompleted {
        job_id: String,
        stage: StageResult,
    },
    /// Fires once at the FSM tail, regardless of outcome
    /// (Done / Failed / Cancelled). `outcome.final_state` is one
    /// of `Done` or `Failed`; cancelled jobs ride the `Failed`
    /// path with `last_error = Some("cancelled by user")`.
    Finished {
        job_id: String,
        outcome: JobOutcome,
    },
    /// Fires when the FSM observes the cancel `Notify` mid-stage,
    /// before the job is finalized as `Failed`. The next event on
    /// this channel is always `Finished`.
    Cancelled {
        job_id: String,
        cancelled_during: JobState,
    },
    /// Fires once per Verdict-rejected retry attempt (W3-12e). The
    /// FSM emits this event AFTER incrementing `Job.retry_count`
    /// and BEFORE re-entering the Plan stage of the next attempt.
    ///
    /// Field semantics:
    ///
    /// - `attempt` is **1-indexed** so the first retry is "attempt 2"
    ///   — the UI renders this as `Attempt {attempt} of {max_retries
    ///   + 1}`.
    /// - `max_retries` is the budget cap (currently 2); included on
    ///   the wire so the UI doesn't have to import the const.
    /// - `triggered_by` is the rejecting gate (`Review` or `Test`).
    /// - `verdict` is the rejecting Verdict — same value the FSM
    ///   stamps onto `Job.last_verdict` before looping back.
    ///
    /// No `Cancelled` or `Finished` event fires on the retry
    /// transition; the job is still running, just looping back.
    /// Subsequent `StageStarted` / `StageCompleted` events on this
    /// channel belong to the new attempt.
    RetryStarted {
        job_id: String,
        attempt: u32,
        max_retries: u32,
        triggered_by: JobState,
        verdict: Verdict,
    },
    /// Fires once per job after the Classify stage's `StageCompleted`
    /// (W3-12f), carrying the parsed `CoordinatorDecision`. The next
    /// event on this channel is either a `StageStarted(Plan)` (when
    /// `route == ExecutePlan`) or a `Finished` (when `route ==
    /// ResearchOnly`, since the FSM short-circuits to Done).
    ///
    /// Optional for cache shape — the same decision rides along on
    /// the prior `StageCompleted`'s `stage.coordinator_decision`
    /// field, so frontend reducers may treat this event as a no-op
    /// (the W3-14 UI uses it to render the route pill before the
    /// next stage starts).
    DecisionMade {
        job_id: String,
        decision: CoordinatorDecision,
    },
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Barrier;

    fn fixture_job(id: &str) -> Job {
        Job {
            id: id.to_string(),
            goal: "test goal".into(),
            created_at_ms: 0,
            state: JobState::Init,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
            last_verdict: None,
        }
    }

    /// Every `JobState` variant serde-roundtrips through the wire
    /// shape (specta's camelCase emission) without information loss.
    #[test]
    fn job_state_transitions_serialize_round_trip() {
        for state in [
            JobState::Init,
            JobState::Scout,
            JobState::Classify,
            JobState::Plan,
            JobState::Build,
            JobState::Review,
            JobState::Test,
            JobState::Done,
            JobState::Failed,
        ] {
            let json =
                serde_json::to_string(&state).expect("serialize");
            let back: JobState =
                serde_json::from_str(&json).expect("deserialize");
            assert_eq!(state, back, "round-trip failed for {state:?}");
        }
        // Spot-check the on-wire shape so future renames don't
        // silently break the frontend bindings.
        assert_eq!(
            serde_json::to_string(&JobState::Init).unwrap(),
            "\"init\""
        );
        assert_eq!(
            serde_json::to_string(&JobState::Failed).unwrap(),
            "\"failed\""
        );
    }

    /// `as_db_str` and `from_db_str` round-trip every variant.
    /// W3-12b §6 acceptance criterion: "JobState::{as,from}_db_str
    /// round-trip on every variant".
    #[test]
    fn job_state_db_str_round_trips() {
        for state in [
            JobState::Init,
            JobState::Scout,
            JobState::Classify,
            JobState::Plan,
            JobState::Build,
            JobState::Review,
            JobState::Test,
            JobState::Done,
            JobState::Failed,
        ] {
            let s = state.as_db_str();
            let back = JobState::from_db_str(s)
                .unwrap_or_else(|_| panic!("round-trip {state:?} via {s}"));
            assert_eq!(state, back, "db_str round-trip failed for {state:?}");
        }
    }

    /// Unknown DB-string values surface as `Internal`, not silently
    /// mapped to a default state.
    #[test]
    fn job_state_from_db_str_unknown_errors() {
        let err = JobState::from_db_str("nonsense")
            .expect_err("unknown discriminant rejected");
        assert_eq!(err.kind(), "internal");
    }

    /// `JobState::is_terminal` matches the documented contract.
    #[test]
    fn job_state_is_terminal_matches_done_or_failed() {
        assert!(JobState::Done.is_terminal());
        assert!(JobState::Failed.is_terminal());
        for s in [
            JobState::Init,
            JobState::Scout,
            JobState::Classify,
            JobState::Plan,
            JobState::Build,
            JobState::Review,
            JobState::Test,
        ] {
            assert!(!s.is_terminal(), "{s:?} should not be terminal");
        }
    }

    /// Insert a job, immediately read it back; equality on the
    /// non-cloning fields proves the registry stores by value.
    #[tokio::test]
    async fn job_registry_insert_and_get_roundtrip() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-a", fixture_job("j-1"))
            .await
            .expect("acquire");
        let got = reg.get("j-1").expect("get");
        assert_eq!(got.id, "j-1");
        assert_eq!(got.state, JobState::Init);
        assert_eq!(got.stages.len(), 0);
    }

    /// `update` mutates the entry in place; `get` reflects it.
    #[tokio::test]
    async fn job_registry_update_modifies_in_place() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-b", fixture_job("j-2"))
            .await
            .expect("acquire");
        reg.update("j-2", |job| {
            job.state = JobState::Scout;
            job.retry_count = 1;
        })
        .await
        .expect("update");
        let got = reg.get("j-2").expect("get");
        assert_eq!(got.state, JobState::Scout);
        assert_eq!(got.retry_count, 1);

        // Updating a missing id surfaces NotFound.
        let err = reg
            .update("j-missing", |_| {})
            .await
            .expect_err("missing id rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// `list` returns every job currently in the registry. The
    /// order is unspecified, so we check membership by id.
    #[tokio::test]
    async fn job_registry_list_returns_all() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-1", fixture_job("j-a"))
            .await
            .expect("ok");
        reg.try_acquire_workspace("ws-2", fixture_job("j-b"))
            .await
            .expect("ok");
        reg.try_acquire_workspace("ws-3", fixture_job("j-c"))
            .await
            .expect("ok");
        let mut ids: Vec<String> =
            reg.list().into_iter().map(|j| j.id).collect();
        ids.sort();
        assert_eq!(ids, vec!["j-a", "j-b", "j-c"]);
    }

    /// Two concurrent acquires for the SAME `workspace_id` — exactly
    /// one returns Ok, the other returns `WorkspaceBusy`. We use a
    /// barrier to force both tasks to call `try_acquire_workspace`
    /// at the same instant; whichever the OS scheduler runs first
    /// wins, but the *count* of winners is always exactly one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn try_acquire_workspace_first_caller_wins() {
        let reg = Arc::new(JobRegistry::new());
        let barrier = Arc::new(Barrier::new(2));

        let r1 = Arc::clone(&reg);
        let b1 = Arc::clone(&barrier);
        let t1 = tokio::spawn(async move {
            b1.wait().await;
            r1.try_acquire_workspace(
                "shared",
                fixture_job("j-thread-1"),
            )
            .await
        });
        let r2 = Arc::clone(&reg);
        let b2 = Arc::clone(&barrier);
        let t2 = tokio::spawn(async move {
            b2.wait().await;
            r2.try_acquire_workspace(
                "shared",
                fixture_job("j-thread-2"),
            )
            .await
        });
        let (r1_out, r2_out) =
            tokio::join!(t1, t2);
        let r1_out = r1_out.expect("task 1 panic");
        let r2_out = r2_out.expect("task 2 panic");

        let oks = [&r1_out, &r2_out]
            .into_iter()
            .filter(|r| r.is_ok())
            .count();
        let errs = [&r1_out, &r2_out]
            .into_iter()
            .filter_map(|r| r.as_ref().err())
            .collect::<Vec<_>>();
        assert_eq!(oks, 1, "exactly one acquire must win");
        assert_eq!(errs.len(), 1, "exactly one acquire must lose");
        assert_eq!(errs[0].kind(), "workspace_busy");
    }

    /// Two concurrent acquires for DIFFERENT `workspace_id`s both
    /// succeed — no global FSM lock.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn try_acquire_workspace_different_workspaces_dont_collide() {
        let reg = Arc::new(JobRegistry::new());
        let barrier = Arc::new(Barrier::new(2));

        let r1 = Arc::clone(&reg);
        let b1 = Arc::clone(&barrier);
        let t1 = tokio::spawn(async move {
            b1.wait().await;
            r1.try_acquire_workspace("ws-x", fixture_job("j-x")).await
        });
        let r2 = Arc::clone(&reg);
        let b2 = Arc::clone(&barrier);
        let t2 = tokio::spawn(async move {
            b2.wait().await;
            r2.try_acquire_workspace("ws-y", fixture_job("j-y")).await
        });
        let (r1_out, r2_out) = tokio::join!(t1, t2);
        r1_out.expect("task 1 panic").expect("ws-x ok");
        r2_out.expect("task 2 panic").expect("ws-y ok");
    }

    /// Acquire, release, re-acquire same workspace → second acquire
    /// succeeds.
    #[tokio::test]
    async fn release_workspace_unlocks_for_subsequent_acquire() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-r", fixture_job("j-first"))
            .await
            .expect("acquire 1");
        reg.release_workspace("ws-r", "j-first").await;
        reg.try_acquire_workspace("ws-r", fixture_job("j-second"))
            .await
            .expect("acquire 2");
    }

    /// Releasing twice (or against a stale job_id) is a no-op —
    /// matches the defensive Drop-guard contract.
    #[tokio::test]
    async fn release_workspace_is_idempotent() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-d", fixture_job("j-d"))
            .await
            .expect("acquire");
        reg.release_workspace("ws-d", "j-d").await;
        // Second release: no panic, no error surface (release is fn-> ()).
        reg.release_workspace("ws-d", "j-d").await;
        // Stale id (different job): also a no-op — the workspace is
        // free and stays free.
        reg.release_workspace("ws-d", "j-stale").await;
        reg.try_acquire_workspace("ws-d", fixture_job("j-d2"))
            .await
            .expect("acquire after idempotent releases");
    }

    /// Empty `workspace_id` (or whitespace-only) → `InvalidInput`,
    /// not `WorkspaceBusy`. The pre-flight check fires before the
    /// lock map is touched.
    #[tokio::test]
    async fn try_acquire_workspace_empty_id_rejected() {
        let reg = JobRegistry::new();
        for bad in ["", "   ", "\t\n"] {
            let err = reg
                .try_acquire_workspace(bad, fixture_job("j-bad"))
                .await
                .expect_err(&format!("`{bad:?}` should be rejected"));
            assert_eq!(err.kind(), "invalid_input");
        }
    }

    // ---------------------------------------------------------------
    // WP-W3-12c — cancel-notify surface tests
    // ---------------------------------------------------------------

    /// Registering a cancel notify for a job_id twice surfaces
    /// `Conflict` — protects against the (theoretical) double-
    /// register that would silently shadow the original Notify.
    #[test]
    fn register_cancel_duplicate_returns_conflict() {
        let reg = JobRegistry::new();
        let n1 = Arc::new(tokio::sync::Notify::new());
        reg.register_cancel("j-c1", Arc::clone(&n1))
            .expect("first register ok");
        let n2 = Arc::new(tokio::sync::Notify::new());
        let err = reg
            .register_cancel("j-c1", n2)
            .expect_err("second register rejected");
        assert_eq!(err.kind(), "conflict");
    }

    /// `unregister_cancel` is idempotent — calling against a
    /// missing id is a no-op (mirrors `release_workspace`'s
    /// contract so the FSM tail + Drop guard can both fire).
    #[test]
    fn unregister_cancel_is_idempotent() {
        let reg = JobRegistry::new();
        let n = Arc::new(tokio::sync::Notify::new());
        reg.register_cancel("j-u1", Arc::clone(&n))
            .expect("register ok");
        reg.unregister_cancel("j-u1");
        // Second unregister: no panic, no error surface.
        reg.unregister_cancel("j-u1");
        // Stale id (never registered): also a no-op.
        reg.unregister_cancel("j-never");
    }

    /// `signal_cancel` against an unknown job_id surfaces
    /// `NotFound` — distinguishes "never started" from "already
    /// finished" only by virtue of the FSM unregistering on tail.
    #[test]
    fn signal_cancel_unknown_returns_not_found() {
        let reg = JobRegistry::new();
        let err = reg
            .signal_cancel("j-nope")
            .expect_err("unknown rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// `signal_cancel` wakes a waiter on the registered Notify.
    /// We register, await `notified()` from one task, signal from
    /// another, and assert the waiter task observes the wake-up.
    #[tokio::test]
    async fn signal_cancel_wakes_registered_notify() {
        let reg = Arc::new(JobRegistry::new());
        let notify = Arc::new(tokio::sync::Notify::new());
        reg.register_cancel("j-w1", Arc::clone(&notify))
            .expect("register ok");

        let waiter_notify = Arc::clone(&notify);
        let waiter = tokio::spawn(async move {
            waiter_notify.notified().await;
        });

        // Give the waiter a tick to register its waker.
        tokio::task::yield_now().await;

        reg.signal_cancel("j-w1").expect("signal ok");
        // The wait must complete promptly; bound it so a regression
        // surfaces as a test failure rather than a hang.
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("waiter did not wake within 1s")
            .expect("waiter task panicked");
    }

    /// The `WorkspaceGuard` (defined in `fsm.rs`) calls
    /// `release_workspace` on Drop. We simulate that here with a
    /// minimal closure-scoped guard so the registry-side test stays
    /// independent of the FSM module's internals.
    ///
    /// W3-12b update: `release_workspace` is async, but Drop is
    /// sync — we exercise it by calling `release_workspace` from
    /// an async block here rather than from a Drop handler. The
    /// FSM-side guard uses `tauri::async_runtime::block_on` which
    /// is integration-tested in fsm.rs.
    #[tokio::test]
    async fn try_acquire_workspace_releases_for_re_acquire() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-g", fixture_job("j-g"))
            .await
            .expect("acquire");
        reg.release_workspace("ws-g", "j-g").await;
        // Workspace is free — the next acquire wins.
        reg.try_acquire_workspace("ws-g", fixture_job("j-g2"))
            .await
            .expect("re-acquire after release");
    }

    // ---------------------------------------------------------------
    // WP-W3-12b — `with_pool` smoke (in-memory only — exercising the
    // pool-wired path lives in store.rs::tests + commands tests).
    // ---------------------------------------------------------------

    /// `with_pool` constructor wires the pool and `has_pool()`
    /// returns true. `new()` returns false. Used by parameterized
    /// FSM tests to assert the right backend is in use.
    #[tokio::test]
    async fn with_pool_constructor_records_pool_handle() {
        let (pool, _dir) = crate::test_support::fresh_pool().await;
        let reg = JobRegistry::with_pool(pool);
        assert!(reg.has_pool(), "with_pool wires the handle");
        let reg2 = JobRegistry::new();
        assert!(!reg2.has_pool(), "new() leaves pool unset");
    }

    // ---------------------------------------------------------------
    // WP-W3-12e — last_rejecting_gate derivation
    // ---------------------------------------------------------------

    fn stage_with_verdict(state: JobState, approved: bool) -> StageResult {
        StageResult {
            state,
            specialist_id: format!("{state:?}").to_lowercase(),
            assistant_text: "x".into(),
            session_id: "sess".into(),
            total_cost_usd: 0.0,
            duration_ms: 0,
            verdict: Some(Verdict {
                approved,
                issues: Vec::new(),
                summary: "s".into(),
            }),
            coordinator_decision: None,
        }
    }

    /// Empty stages → no rejecting gate.
    #[test]
    fn last_rejecting_gate_empty_stages_returns_none() {
        let job = fixture_job("j-no-stages");
        assert!(job.last_rejecting_gate().is_none());
    }

    /// All-approved chain → no rejecting gate.
    #[test]
    fn last_rejecting_gate_all_approved_returns_none() {
        let mut job = fixture_job("j-all-ok");
        job.stages.push(stage_with_verdict(JobState::Review, true));
        job.stages.push(stage_with_verdict(JobState::Test, true));
        assert!(job.last_rejecting_gate().is_none());
    }

    /// Reviewer rejected on the most recent attempt → returns Review.
    #[test]
    fn last_rejecting_gate_returns_review_when_review_rejected() {
        let mut job = fixture_job("j-rev");
        job.stages.push(stage_with_verdict(JobState::Review, false));
        assert_eq!(job.last_rejecting_gate(), Some(JobState::Review));
    }

    /// Tester rejected after Reviewer approved → returns Test (the
    /// most recent rejecting gate, NOT the most recent gate overall).
    #[test]
    fn last_rejecting_gate_returns_test_when_test_rejected() {
        let mut job = fixture_job("j-test");
        job.stages.push(stage_with_verdict(JobState::Review, true));
        job.stages.push(stage_with_verdict(JobState::Test, false));
        assert_eq!(job.last_rejecting_gate(), Some(JobState::Test));
    }

    /// Stages without verdicts (Scout/Plan/Build) are skipped.
    #[test]
    fn last_rejecting_gate_skips_non_verdict_stages() {
        let mut job = fixture_job("j-mix");
        // A Scout stage with `verdict=None` must not throw the helper
        // off; only Review/Test entries with rejected verdicts count.
        job.stages.push(StageResult {
            state: JobState::Scout,
            specialist_id: "scout".into(),
            assistant_text: "sc".into(),
            session_id: "s".into(),
            total_cost_usd: 0.0,
            duration_ms: 0,
            verdict: None,
            coordinator_decision: None,
        });
        job.stages.push(stage_with_verdict(JobState::Review, false));
        assert_eq!(job.last_rejecting_gate(), Some(JobState::Review));
    }

    /// Newest rejection wins — even if an earlier Review rejected,
    /// the most recent rejecting gate is the one returned. This
    /// matches the retry loop's intent: label the gate that just
    /// triggered the upcoming retry, not an older one.
    #[test]
    fn last_rejecting_gate_returns_newest_rejection() {
        let mut job = fixture_job("j-newest");
        job.stages.push(stage_with_verdict(JobState::Review, false));
        // Retry round: Reviewer approved this time, Tester rejected.
        job.stages.push(stage_with_verdict(JobState::Plan, true)); // no verdict shape; helper ignores
        job.stages.push(stage_with_verdict(JobState::Review, true));
        job.stages.push(stage_with_verdict(JobState::Test, false));
        assert_eq!(job.last_rejecting_gate(), Some(JobState::Test));
    }

    /// `SwarmJobEvent::RetryStarted` serializes to the documented
    /// snake_case wire shape with all fields present at the top level.
    #[test]
    fn swarm_job_event_retry_started_serializes() {
        let evt = SwarmJobEvent::RetryStarted {
            job_id: "j-1".into(),
            attempt: 2,
            max_retries: 2,
            triggered_by: JobState::Review,
            verdict: Verdict {
                approved: false,
                issues: Vec::new(),
                summary: "rejected".into(),
            },
        };
        let json = serde_json::to_value(&evt).expect("serialize");
        assert_eq!(
            json.get("kind").and_then(|v| v.as_str()),
            Some("retry_started")
        );
        assert_eq!(json.get("attempt").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(
            json.get("max_retries").and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            json.get("triggered_by").and_then(|v| v.as_str()),
            Some("review"),
            "triggered_by uses JobState's snake_case wire shape"
        );
        let verdict = json.get("verdict").expect("verdict embedded");
        assert_eq!(
            verdict.get("approved").and_then(|v| v.as_bool()),
            Some(false)
        );
    }
}
