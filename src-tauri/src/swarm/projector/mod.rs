//! `JobProjector` — mailbox → SwarmJobEvent + swarm_jobs row
//! synthesiser (WP-W5-04).
//!
//! One projector task per workspace. Subscribes to the
//! [`MailboxBus`], walks each [`MailboxEnvelope`] through a
//! lightweight state machine that maps primitive mailbox events
//! to the existing [`SwarmJobEvent`] wire shape (the FSM's
//! contract with the W3-12c+ frontend hooks):
//!
//! | Event | Synthesises | Side effect |
//! |---|---|---|
//! | `JobStarted` | `SwarmJobEvent::Started` | INSERT `swarm_jobs` row (`source='brain'`) |
//! | `TaskDispatch` | `SwarmJobEvent::StageStarted` | none |
//! | `AgentResult` | `SwarmJobEvent::StageCompleted` | INSERT `swarm_stages` row |
//! | `AgentHelpRequest` | (none) | none |
//! | `CoordinatorHelpOutcome` | (none) | none |
//! | `JobCancel` | `SwarmJobEvent::Cancelled` | UPDATE `swarm_jobs.state='failed'`, `last_error='cancelled by user'` |
//! | `JobFinished` | `SwarmJobEvent::Finished` | UPDATE `swarm_jobs.state`, `finished_at_ms` |
//!
//! ## Retry detection
//!
//! A `TaskDispatch` whose `target` matches a previous
//! `TaskDispatch.target` for the same `job_id` counts as a retry.
//! The projector emits [`SwarmJobEvent::RetryStarted`] BEFORE the
//! matching `StageStarted`. Re-using the existing `RetryStarted`
//! variant (W3-12e) keeps the wire contract verbatim — frontend
//! reducers that already handle FSM retries don't need a second
//! switch arm.
//!
//! ## Stage row mapping (agent_id → JobState)
//!
//! Hardcoded per W5-04 contract §5. Documented inline at
//! [`agent_id_to_job_state`]:
//!
//! | `agent_id` | `JobState` |
//! |---|---|
//! | `scout` | `Scout` |
//! | `coordinator` | `Classify` |
//! | `planner` | `Plan` |
//! | `backend-builder` / `frontend-builder` | `Build` |
//! | `backend-reviewer` / `frontend-reviewer` | `Review` |
//! | `integration-tester` | `Test` |
//! | (any other) | `Build` (fallback — defensive) |
//!
//! ## Verdict parsing
//!
//! Reviewer / Tester `AgentResult.assistant_text` is fed through
//! `parse_verdict`. On parse failure the projector logs
//! `tracing::warn!` and writes a stage row with
//! `verdict_json = NULL` rather than failing the projection —
//! same fail-soft policy as the FSM today.
//!
//! ## Why string-typed `JobOutcome.last_verdict` derivation
//!
//! When the brain finishes with `outcome=failed` and the most
//! recent rejected `Verdict` is in the event log, the projector
//! ties the failure to that verdict (sets `JobOutcome.last_verdict`,
//! leaves `last_error` null). When the brain finishes failed for
//! another reason (max dispatches, cancel, parse error) the
//! verdict stays None and `last_error` carries the brain's
//! `JobFinished.summary`. Mirrors the FSM's `last_verdict` /
//! `last_error` contract from W3-12d so the frontend reducer
//! treats both paths identically.

use std::collections::HashMap;
use std::sync::Arc;

use tauri::{AppHandle, Runtime};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::JobOutcome;
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEnvelope};

mod helpers;
mod outcome;
mod persistence;
mod projection;
mod query;

#[cfg(test)]
mod tests;

pub use query::get_brain_job_detail;

use helpers::event_job_id;
use outcome::OutcomeBuilder;
use projection::{run_loop, ProjectorState};

// ---------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------

/// One per-workspace projector task. Subscribes to the workspace
/// channel of [`MailboxBus`] on spawn and consumes envelopes for
/// the lifetime of the workspace, synthesising [`SwarmJobEvent`]s
/// onto the per-job Tauri channel and writing through to
/// `swarm_jobs` / `swarm_stages`.
///
/// Construction is via [`JobProjector::spawn`] — never `new()`.
/// Shutdown is cooperative via [`ProjectorHandle::shutdown`].
pub struct JobProjector;

impl JobProjector {
    /// Spawn the projector task for one workspace. Subscribes to
    /// the bus immediately so the brain's emits never race against
    /// the projector's `recv`. Returns a [`ProjectorHandle`] that
    /// lives in `app.manage(...)` next to the registry / bus.
    ///
    /// `app` is forwarded to every emitted [`SwarmJobEvent`] (via
    /// `app.emit("swarm:job:{id}:event", ...)`). `pool` is used by
    /// the side-effect SQL writes; the same pool the bus is bound
    /// to, but passed separately so a future split is cheap.
    pub fn spawn<R: Runtime>(
        app: AppHandle<R>,
        workspace_id: String,
        bus: Arc<MailboxBus>,
        pool: DbPool,
    ) -> ProjectorHandle {
        let shutdown = Arc::new(Notify::new());
        let shutdown_for_loop = Arc::clone(&shutdown);
        let app_for_loop = app.clone();
        let workspace_for_loop = workspace_id.clone();
        let bus_for_loop = Arc::clone(&bus);
        let pool_for_loop = pool.clone();

        let handle = tokio::spawn(async move {
            let mut receiver = bus_for_loop.subscribe(&workspace_for_loop).await;
            let mut state = ProjectorState::new(workspace_for_loop);
            run_loop(
                app_for_loop,
                &mut receiver,
                &mut state,
                &pool_for_loop,
                shutdown_for_loop,
            )
            .await;
        });

        ProjectorHandle { handle, shutdown }
    }

    /// Walk the entire mailbox event log for one job and compute
    /// the final [`JobOutcome`]. Used by `swarm:run_job_v2` to
    /// return a JobOutcome at IPC return time without re-walking
    /// the projector's in-memory state (the projector may still be
    /// digesting later events when the IPC returns).
    ///
    /// Source-of-truth: `bus.list_typed(None, None, None)` reads
    /// the SQL `mailbox` table in oldest-first order. Filters in
    /// memory by `job_id` so the SQL stays a single static query
    /// (the W5-01 list_typed surface doesn't take a job_id filter
    /// — single-workspace assumption — so we filter here).
    pub async fn build_outcome(
        bus: &Arc<MailboxBus>,
        pool: &DbPool,
        job_id: &str,
    ) -> Result<JobOutcome, AppError> {
        // Pull every persisted event. The `list_typed` default
        // limit is 100; jobs with longer event logs need a higher
        // cap. We page through `since_id` cursors until exhausted.
        let mut events: Vec<MailboxEnvelope> = Vec::new();
        let mut since_id: Option<i64> = None;
        loop {
            // 500 is the bus's hard cap (LIST_TYPED_MAX_LIMIT).
            // One page worth is enough for any plausible single
            // job; the loop is defensive against a future bus
            // tightening of that cap.
            let page = bus.list_typed(None, since_id, Some(500)).await?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            since_id = Some(page.last().expect("non-empty page").id);
            events.extend(page);
            if page_len < 500 {
                break;
            }
        }
        // Filter in memory by job_id. Only events whose variant
        // carries a job_id matter — `Note` carries none and is
        // never tied to a job.
        let job_events: Vec<&MailboxEnvelope> = events
            .iter()
            .filter(|env| event_job_id(&env.event).map(|j| j == job_id).unwrap_or(false))
            .collect();

        let mut state = OutcomeBuilder::new(job_id.to_string());
        for env in &job_events {
            state.observe(env);
        }
        state.finish(pool).await
    }
}

/// Owned shutdown handle for one projector task. `shutdown()`
/// signals the loop to exit and awaits the join handle.
pub struct ProjectorHandle {
    handle: tokio::task::JoinHandle<()>,
    shutdown: Arc<Notify>,
}

impl ProjectorHandle {
    /// Cooperative shutdown — wakes the loop's select branch then
    /// awaits the join handle. Idempotent at the join layer (a
    /// double-call panics on the second `await` because tokio's
    /// JoinHandle is not Clone, but the Notify wake is harmless).
    pub async fn shutdown(self) {
        self.shutdown.notify_waiters();
        let _ = self.handle.await;
    }

    /// Test/diagnostics accessor — whether the underlying task has
    /// finished. Useful for asserting cooperative shutdown actually
    /// landed without blocking on the join.
    #[cfg(test)]
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

// ---------------------------------------------------------------------
// Projector registry — lazy per-workspace handle map
// ---------------------------------------------------------------------

/// App-state container holding one [`ProjectorHandle`] per
/// workspace. `swarm:run_job_v2` calls `ensure_for_workspace` to
/// lazy-spawn on first use; the handle stays alive until app
/// shutdown (the projector's loop is cheap and idempotent — a
/// resubscribe between jobs costs one channel slot, which the
/// existing 64-cap bus handles fine).
pub struct JobProjectorRegistry {
    /// `RwLock` so `ensure_for_workspace` reads the map for the
    /// fast path (existing handle) and only takes the write lock
    /// to insert a freshly-spawned task.
    handles: RwLock<HashMap<String, Arc<Mutex<Option<ProjectorHandle>>>>>,
}

impl JobProjectorRegistry {
    pub fn new() -> Self {
        Self {
            handles: RwLock::new(HashMap::new()),
        }
    }

    /// Lazy-spawn the projector for `workspace_id`. Idempotent —
    /// repeat calls return the existing handle's slot. The slot is
    /// `Mutex<Option<...>>` so a future `shutdown_workspace` API
    /// (W5-05+) can take the handle out and join it without
    /// removing the slot from the map.
    pub async fn ensure_for_workspace<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        workspace_id: &str,
        bus: Arc<MailboxBus>,
        pool: DbPool,
    ) {
        // Fast path: handle already present.
        {
            let map = self.handles.read().await;
            if map.contains_key(workspace_id) {
                return;
            }
        }
        // Slow path: spawn under write lock (race re-check inside).
        let mut map = self.handles.write().await;
        if map.contains_key(workspace_id) {
            return;
        }
        let projector = JobProjector::spawn(
            app.clone(),
            workspace_id.to_string(),
            bus,
            pool,
        );
        map.insert(
            workspace_id.to_string(),
            Arc::new(Mutex::new(Some(projector))),
        );
    }

    /// Test/diagnostics accessor — number of workspaces with a
    /// projector handle currently held.
    #[cfg(test)]
    pub async fn handle_count(&self) -> usize {
        self.handles.read().await.len()
    }

    /// Shut down every projector. Called by the `RunEvent::ExitRequested`
    /// hook in `lib.rs` so the broadcast subscribers don't outlive
    /// the runtime — same pattern as the agent registry.
    pub async fn shutdown_all(&self) {
        let mut map = self.handles.write().await;
        for (_, slot) in map.drain() {
            let mut guard = slot.lock().await;
            if let Some(handle) = guard.take() {
                handle.shutdown().await;
            }
        }
    }
}

impl Default for JobProjectorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
