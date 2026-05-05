//! Job state types + in-memory registry (WP-W3-12a §2 / §4).
//!
//! `Job`, `JobState`, `JobOutcome`, and `StageResult` cross the IPC
//! boundary as the FSM's contract with the frontend. `JobRegistry` is
//! the in-memory store — `Arc<Mutex<HashMap<...>>>`-style. W3-12b
//! replaces the registry with a SQLite-backed equivalent on the same
//! method surface so callers don't churn.
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use specta::Type;
use tokio::sync::Notify;

use crate::error::AppError;

/// Lifecycle states of a swarm job. Per WP §2:
///
/// - `Init` — newly minted, before the first transition fires.
/// - `Scout` / `Plan` / `Build` — the three happy-path stages.
/// - `Review` / `Test` — reserved for W3-12d (reviewer +
///   integration-tester profiles); FSM never enters them in 12a.
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
    Plan,
    Build,
    /// Reserved for W3-12d. FSM never enters this state in W3-12a;
    /// the next-state function asserts unreachable in debug builds.
    Review,
    /// Reserved for W3-12d. Same as `Review`.
    Test,
    Done,
    /// Terminal failure state. Carries the last error in
    /// `Job.last_error`.
    Failed,
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
}

/// In-memory job + workspace-lock registry. `Mutex` (std, not async)
/// is fine here — every operation is short and never `await`s while
/// holding the lock.
///
/// The two maps are independent so a job mutation that doesn't
/// change workspace state (e.g. appending a `StageResult`) never
/// contests the `workspace_locks` mutex.
///
/// Lock acquisition order — both internal helpers and external
/// callers MUST follow `workspace_locks` first, then `jobs`. The
/// constructor / accessors that touch only one map are exempt.
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Job>>,
    /// `workspace_id` → `job_id` of the in-flight job currently
    /// holding the workspace. Removed on `release_workspace`.
    workspace_locks: Mutex<HashMap<String, String>>,
    /// WP-W3-12c — per-job cancellation notify. The FSM `select!`s
    /// each stage future against this notify; `swarm:cancel_job`
    /// looks up the entry by `job_id` and calls `notify_one()`.
    /// The map is registered on FSM start and removed on terminal
    /// transition (Done / Failed / Cancelled).
    cancel_notifies: Mutex<HashMap<String, Arc<Notify>>>,
}

impl JobRegistry {
    /// Build an empty registry for `app.manage(Arc::new(...))`.
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            workspace_locks: Mutex::new(HashMap::new()),
            cancel_notifies: Mutex::new(HashMap::new()),
        }
    }

    /// Atomically validate non-empty `workspace_id`, check no
    /// existing lock, and register both the new job and its lock.
    /// Returns:
    ///
    /// - `Ok(())` on success — the workspace is now busy with this
    ///   job, and `get(&job.id)` will return the inserted record.
    /// - `Err(AppError::InvalidInput)` if `workspace_id` is empty
    ///   (after trim).
    /// - `Err(AppError::WorkspaceBusy { workspace_id, in_flight_job_id })`
    ///   if the workspace already has an in-flight job.
    ///
    /// Lock order: `workspace_locks` first (the contention point),
    /// then `jobs` (uncontested in the success path). The two
    /// `lock()` calls are deliberately interleaved to keep the
    /// "either both happen or neither does" invariant — if the
    /// `jobs` mutex is poisoned for any reason the workspace lock
    /// is also released because we never insert it on the bail
    /// path.
    pub fn try_acquire_workspace(
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

        // Reserve the lock + insert the job in one critical section
        // so a concurrent caller observing the empty `locks` map
        // can never interleave between the two writes.
        let mut jobs =
            self.jobs.lock().expect("jobs mutex poisoned");
        locks.insert(trimmed.to_string(), new_job.id.clone());
        jobs.insert(new_job.id.clone(), new_job);
        Ok(())
    }

    /// Release the workspace lock for `(workspace_id, job_id)`.
    /// Idempotent: the FSM always calls this on the success and
    /// failure paths, and a `Drop`-driven defensive call from
    /// `WorkspaceGuard` may also fire — calling twice (or against a
    /// workspace that another job has since taken over) is a no-op.
    pub fn release_workspace(&self, workspace_id: &str, job_id: &str) {
        let trimmed = workspace_id.trim();
        if trimmed.is_empty() {
            return;
        }
        let mut locks = self
            .workspace_locks
            .lock()
            .expect("workspace_locks mutex poisoned");
        // Only remove if the lock still belongs to this job_id —
        // protects against the (theoretical, in 12a impossible)
        // scenario where a stale Drop guard fires after the
        // workspace has been re-acquired by a different job.
        if locks.get(trimmed).map(String::as_str) == Some(job_id) {
            locks.remove(trimmed);
        }
    }

    /// Mutate the job under `id` in place. The closure receives
    /// `&mut Job` so callers can update any field; nothing is
    /// returned because the FSM's `update` call sites only need to
    /// know whether the id existed (mapped to `AppError::NotFound`).
    pub fn update<F: FnOnce(&mut Job)>(
        &self,
        id: &str,
        f: F,
    ) -> Result<(), AppError> {
        let mut jobs =
            self.jobs.lock().expect("jobs mutex poisoned");
        match jobs.get_mut(id) {
            Some(job) => {
                f(job);
                Ok(())
            }
            None => Err(AppError::NotFound(format!(
                "swarm job `{id}` not in registry"
            ))),
        }
    }

    /// Snapshot of the job under `id`, or `None`. Returns an owned
    /// `Job` so the caller doesn't hold the registry lock past the
    /// call.
    pub fn get(&self, id: &str) -> Option<Job> {
        let jobs = self.jobs.lock().expect("jobs mutex poisoned");
        jobs.get(id).cloned()
    }

    /// Snapshot of every job in the registry. Order is unspecified;
    /// the W3-12c streaming list will sort by `created_at_ms` on
    /// the read path, not here.
    pub fn list(&self) -> Vec<Job> {
        let jobs = self.jobs.lock().expect("jobs mutex poisoned");
        jobs.values().cloned().collect()
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
    /// The FSM owns the `Arc` for the duration of the run; the
    /// registry holds a clone so `signal_cancel` can call
    /// `notify_one()` without bouncing through the FSM.
    ///
    /// Returns `Err(AppError::Conflict)` if a notify is already
    /// registered for this `job_id` — protects against the
    /// (impossible-in-practice in 12c) double-register race.
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

    /// Signal cancellation for the given `job_id`. Looks up the
    /// `Notify` and calls `notify_one()`; the entry is left in the
    /// map (the FSM removes it when it observes the cancel).
    ///
    /// - `Ok(())` if a notify was registered and signaled.
    /// - `Err(AppError::NotFound)` if no notify is registered for
    ///   this `job_id` — i.e. the job is unknown, terminal, or had
    ///   its cancel already de-registered by the FSM tail.
    pub fn signal_cancel(&self, job_id: &str) -> Result<(), AppError> {
        let notifies = self
            .cancel_notifies
            .lock()
            .expect("cancel_notifies mutex poisoned");
        match notifies.get(job_id) {
            Some(notify) => {
                notify.notify_one();
                Ok(())
            }
            None => Err(AppError::NotFound(format!(
                "swarm job `{job_id}` has no in-flight cancel notify"
            ))),
        }
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
        }
    }

    /// Every `JobState` variant serde-roundtrips through the wire
    /// shape (specta's camelCase emission) without information loss.
    #[test]
    fn job_state_transitions_serialize_round_trip() {
        for state in [
            JobState::Init,
            JobState::Scout,
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

    /// Insert a job, immediately read it back; equality on the
    /// non-cloning fields proves the registry stores by value.
    #[test]
    fn job_registry_insert_and_get_roundtrip() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-a", fixture_job("j-1"))
            .expect("acquire");
        let got = reg.get("j-1").expect("get");
        assert_eq!(got.id, "j-1");
        assert_eq!(got.state, JobState::Init);
        assert_eq!(got.stages.len(), 0);
    }

    /// `update` mutates the entry in place; `get` reflects it.
    #[test]
    fn job_registry_update_modifies_in_place() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-b", fixture_job("j-2"))
            .expect("acquire");
        reg.update("j-2", |job| {
            job.state = JobState::Scout;
            job.retry_count = 1;
        })
        .expect("update");
        let got = reg.get("j-2").expect("get");
        assert_eq!(got.state, JobState::Scout);
        assert_eq!(got.retry_count, 1);

        // Updating a missing id surfaces NotFound.
        let err = reg
            .update("j-missing", |_| {})
            .expect_err("missing id rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// `list` returns every job currently in the registry. The
    /// order is unspecified, so we check membership by id.
    #[test]
    fn job_registry_list_returns_all() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-1", fixture_job("j-a"))
            .expect("ok");
        reg.try_acquire_workspace("ws-2", fixture_job("j-b"))
            .expect("ok");
        reg.try_acquire_workspace("ws-3", fixture_job("j-c"))
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
        });
        let r2 = Arc::clone(&reg);
        let b2 = Arc::clone(&barrier);
        let t2 = tokio::spawn(async move {
            b2.wait().await;
            r2.try_acquire_workspace(
                "shared",
                fixture_job("j-thread-2"),
            )
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
            r1.try_acquire_workspace("ws-x", fixture_job("j-x"))
        });
        let r2 = Arc::clone(&reg);
        let b2 = Arc::clone(&barrier);
        let t2 = tokio::spawn(async move {
            b2.wait().await;
            r2.try_acquire_workspace("ws-y", fixture_job("j-y"))
        });
        let (r1_out, r2_out) = tokio::join!(t1, t2);
        r1_out.expect("task 1 panic").expect("ws-x ok");
        r2_out.expect("task 2 panic").expect("ws-y ok");
    }

    /// Acquire, release, re-acquire same workspace → second acquire
    /// succeeds.
    #[test]
    fn release_workspace_unlocks_for_subsequent_acquire() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-r", fixture_job("j-first"))
            .expect("acquire 1");
        reg.release_workspace("ws-r", "j-first");
        reg.try_acquire_workspace("ws-r", fixture_job("j-second"))
            .expect("acquire 2");
    }

    /// Releasing twice (or against a stale job_id) is a no-op —
    /// matches the defensive Drop-guard contract.
    #[test]
    fn release_workspace_is_idempotent() {
        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-d", fixture_job("j-d"))
            .expect("acquire");
        reg.release_workspace("ws-d", "j-d");
        // Second release: no panic, no error surface (release is fn-> ()).
        reg.release_workspace("ws-d", "j-d");
        // Stale id (different job): also a no-op — the workspace is
        // free and stays free.
        reg.release_workspace("ws-d", "j-stale");
        reg.try_acquire_workspace("ws-d", fixture_job("j-d2"))
            .expect("acquire after idempotent releases");
    }

    /// Empty `workspace_id` (or whitespace-only) → `InvalidInput`,
    /// not `WorkspaceBusy`. The pre-flight check fires before the
    /// lock map is touched.
    #[test]
    fn try_acquire_workspace_empty_id_rejected() {
        let reg = JobRegistry::new();
        for bad in ["", "   ", "\t\n"] {
            let err = reg
                .try_acquire_workspace(bad, fixture_job("j-bad"))
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
    #[test]
    fn try_acquire_workspace_releases_on_workspaceguard_drop() {
        struct LocalGuard<'a> {
            reg: &'a JobRegistry,
            workspace_id: &'a str,
            job_id: &'a str,
        }
        impl<'a> Drop for LocalGuard<'a> {
            fn drop(&mut self) {
                self.reg
                    .release_workspace(self.workspace_id, self.job_id);
            }
        }

        let reg = JobRegistry::new();
        reg.try_acquire_workspace("ws-g", fixture_job("j-g"))
            .expect("acquire");
        {
            let _guard = LocalGuard {
                reg: &reg,
                workspace_id: "ws-g",
                job_id: "j-g",
            };
            // Guard goes out of scope at the close of this block,
            // which fires `Drop::drop`, which calls
            // `release_workspace`.
        }
        // Workspace is free — the next acquire wins.
        reg.try_acquire_workspace("ws-g", fixture_job("j-g2"))
            .expect("re-acquire after guard drop");
    }
}
