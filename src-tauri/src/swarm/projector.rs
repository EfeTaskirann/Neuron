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

use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{broadcast, Mutex, Notify, RwLock};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::swarm::coordinator::{
    parse_verdict, Job, JobDetail, JobOutcome, JobState, StageResult,
    SwarmJobEvent, Verdict,
};
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEnvelope, MailboxEvent};

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

// ---------------------------------------------------------------------
// Private implementation — projector state & main loop
// ---------------------------------------------------------------------

/// Per-job bookkeeping the projector needs to synthesise events.
/// Everything is local to the projector task — no other code
/// reads it; the SQL `swarm_jobs` / `swarm_stages` rows + the
/// emitted `SwarmJobEvent`s are the wire-facing shapes.
///
/// `workspace_id` / `goal` / `created_at_ms` are stored for
/// completeness (debug logs, future restart-recovery) — currently
/// only consumed at the `Started` event emit. The
/// `#[allow(dead_code)]`s are intentional: those fields are part
/// of the stable shape, not transient locals.
#[derive(Debug, Clone)]
struct ProjectorJobEntry {
    #[allow(dead_code)]
    workspace_id: String,
    #[allow(dead_code)]
    goal: String,
    /// Chronological list of dispatch targets — used by the retry
    /// detector (`is_retry_dispatch` walks history newest-first).
    dispatch_history: Vec<String>,
    /// Idx of the next stage row in `swarm_stages`. Increments on
    /// every `AgentResult` we persist. 0-based to match the
    /// existing `insert_stage(idx)` contract.
    next_stage_idx: u32,
    /// Accumulated stage results — used by `JobOutcome` aggregation
    /// at JobFinished time.
    stages: Vec<StageResult>,
    /// Wall-clock created_at_ms — needed for the `Started` event.
    #[allow(dead_code)]
    created_at_ms: i64,
    /// Most recent rejected verdict, if any — flows into
    /// `JobOutcome.last_verdict` on a Failed termination.
    last_rejected_verdict: Option<Verdict>,
}

struct ProjectorState {
    workspace_id: String,
    jobs: HashMap<String, ProjectorJobEntry>,
}

impl ProjectorState {
    fn new(workspace_id: String) -> Self {
        Self {
            workspace_id,
            jobs: HashMap::new(),
        }
    }
}

/// Main loop body. Awaits envelopes from the per-workspace bus
/// channel; on each one, dispatches to the right handler under
/// the per-job state slot. Exits when `shutdown` is signalled or
/// when the broadcast channel closes (every sender dropped — only
/// happens at app shutdown).
async fn run_loop<R: Runtime>(
    app: AppHandle<R>,
    receiver: &mut broadcast::Receiver<MailboxEnvelope>,
    state: &mut ProjectorState,
    pool: &DbPool,
    shutdown: Arc<Notify>,
) {
    loop {
        tokio::select! {
            biased;
            _ = shutdown.notified() => {
                tracing::debug!(
                    workspace_id = %state.workspace_id,
                    "JobProjector: shutdown signalled, exiting loop"
                );
                return;
            }
            recv = receiver.recv() => {
                match recv {
                    Ok(env) => handle_envelope(&app, state, pool, env).await,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            workspace_id = %state.workspace_id,
                            skipped = skipped,
                            "JobProjector: broadcast receiver lagged; \
                             SQL log is source of truth — replay via \
                             list_typed if downstream needs missed events"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!(
                            workspace_id = %state.workspace_id,
                            "JobProjector: broadcast channel closed, exiting loop"
                        );
                        return;
                    }
                }
            }
        }
    }
}

/// Dispatch one envelope to the right per-event handler. Most
/// helpers are sync — they read the per-job entry, mutate, emit a
/// SwarmJobEvent, and return. Only the SQL writes are awaited.
async fn handle_envelope<R: Runtime>(
    app: &AppHandle<R>,
    state: &mut ProjectorState,
    pool: &DbPool,
    env: MailboxEnvelope,
) {
    match &env.event {
        MailboxEvent::JobStarted {
            job_id,
            workspace_id,
            goal,
        } => {
            on_job_started(
                app,
                state,
                pool,
                job_id,
                workspace_id,
                goal,
                env.ts,
            )
            .await;
        }
        MailboxEvent::TaskDispatch {
            job_id, target, ..
        } => {
            on_task_dispatch(app, state, job_id, target);
        }
        MailboxEvent::AgentResult {
            job_id,
            agent_id,
            assistant_text,
            total_cost_usd,
            turn_count,
        } => {
            on_agent_result(
                app,
                state,
                pool,
                job_id,
                agent_id,
                assistant_text,
                *total_cost_usd,
                *turn_count,
                env.ts,
            )
            .await;
        }
        MailboxEvent::JobCancel { job_id } => {
            on_job_cancel(app, state, pool, job_id).await;
        }
        MailboxEvent::JobFinished {
            job_id,
            outcome,
            summary,
        } => {
            on_job_finished(app, state, pool, job_id, outcome, summary).await;
        }
        // No SwarmJobEvent for these — the agent help-loop is a
        // private exchange between the brain and the specialist;
        // the existing per-agent event channel surfaces it on the
        // grid pane (W4-04).
        MailboxEvent::AgentHelpRequest { .. }
        | MailboxEvent::CoordinatorHelpOutcome { .. }
        | MailboxEvent::Note => {}
    }
}

async fn on_job_started<R: Runtime>(
    app: &AppHandle<R>,
    state: &mut ProjectorState,
    pool: &DbPool,
    job_id: &str,
    workspace_id: &str,
    goal: &str,
    ts: i64,
) {
    // ts is unix epoch *seconds* on the bus envelope (W5-01); the
    // SwarmJobEvent and swarm_jobs row both use *milliseconds*
    // (Charter §8 invariant). Multiply.
    let created_at_ms = ts * 1_000;

    state.jobs.insert(
        job_id.to_string(),
        ProjectorJobEntry {
            workspace_id: workspace_id.to_string(),
            goal: goal.to_string(),
            dispatch_history: Vec::new(),
            next_stage_idx: 0,
            stages: Vec::new(),
            created_at_ms,
            last_rejected_verdict: None,
        },
    );

    // Emit the SwarmJobEvent::Started.
    emit_event(
        app,
        job_id,
        SwarmJobEvent::Started {
            job_id: job_id.to_string(),
            workspace_id: workspace_id.to_string(),
            goal: goal.to_string(),
            created_at_ms,
        },
    );

    // Persist the swarm_jobs row. Idempotent at the projector level
    // — `swarm:run_job_v2` already inserted the row via the registry
    // (with `source='brain'`) before spawning the brain. We
    // therefore short-circuit on duplicate inserts; a `Conflict`-
    // shaped SQL error is the existence signal we expect on every
    // brain-driven job. Tests that don't pre-insert (running the
    // projector standalone) still get the row created here.
    let job_row = Job {
        id: job_id.to_string(),
        goal: goal.to_string(),
        created_at_ms,
        state: JobState::Init,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        // Projector-driven row → 'brain' source discriminator.
        source: "brain".into(),
    };
    if let Err(e) = upsert_brain_job_row(pool, &job_row, workspace_id).await {
        tracing::warn!(
            job_id = %job_id,
            workspace_id = %workspace_id,
            error = %e,
            "JobProjector: swarm_jobs upsert failed; \
             SwarmJobEvent::Started already emitted, projection \
             continues but persistent row may be missing"
        );
    }
}

fn on_task_dispatch<R: Runtime>(
    app: &AppHandle<R>,
    state: &mut ProjectorState,
    job_id: &str,
    target: &str,
) {
    let Some(entry) = state.jobs.get_mut(job_id) else {
        // Defensive — a TaskDispatch arriving before JobStarted is
        // a contract violation by the brain; log and drop. The
        // bus's broadcast is FIFO per subscriber, so this branch
        // is theoretically unreachable on a single-projector setup.
        tracing::warn!(
            job_id = %job_id,
            target = %target,
            "JobProjector: TaskDispatch for unknown job; ignoring"
        );
        return;
    };

    // Map target to JobState — `agent:<id>` → `<id>` → JobState.
    let agent_id_opt = target.strip_prefix("agent:").unwrap_or(target);
    let job_state = agent_id_to_job_state(agent_id_opt);
    let specialist_id = agent_id_opt.to_string();

    // Retry detection: count prior dispatches to the same target.
    let attempt = is_retry_dispatch(target, &entry.dispatch_history);
    entry.dispatch_history.push(target.to_string());

    // Emit RetryStarted BEFORE StageStarted so the frontend
    // reducer sees the retry transition first (matches the W3-12e
    // FSM order). Re-using RetryStarted (the existing variant)
    // keeps the wire shape stable; we synthesise a dummy verdict
    // shape from the brain's `last_rejected_verdict` if any, else
    // a placeholder summary so the `verdict` field is never
    // null-shaped on the wire (which the existing reducer
    // doesn't expect).
    if let Some(attempt_n) = attempt {
        let verdict = entry
            .last_rejected_verdict
            .clone()
            .unwrap_or_else(|| Verdict {
                approved: false,
                issues: Vec::new(),
                summary: "retry triggered by repeated dispatch".to_string(),
            });
        emit_event(
            app,
            job_id,
            SwarmJobEvent::RetryStarted {
                job_id: job_id.to_string(),
                attempt: attempt_n,
                // The brain's max-dispatch cap is the retry budget
                // analog. We surface it as 0 (unbounded) here
                // because the brain does not enforce a per-target
                // retry budget; the frontend can render it as
                // "retry attempt N" without a denominator.
                max_retries: 0,
                triggered_by: job_state,
                verdict,
            },
        );
    }

    // Emit StageStarted regardless of retry status.
    emit_event(
        app,
        job_id,
        SwarmJobEvent::StageStarted {
            job_id: job_id.to_string(),
            state: job_state,
            specialist_id,
            // No prompt preview tracked here — the brain's
            // dispatch prompt lives on the bus envelope's
            // payload_json; the W5-04 wire shape doesn't carry it
            // through to StageStarted (frontend already shows the
            // bus row in the chat panel).
            prompt_preview: String::new(),
        },
    );
}

#[allow(clippy::too_many_arguments)]
async fn on_agent_result<R: Runtime>(
    app: &AppHandle<R>,
    state: &mut ProjectorState,
    pool: &DbPool,
    job_id: &str,
    agent_id: &str,
    assistant_text: &str,
    total_cost_usd: f64,
    turn_count: u32,
    ts: i64,
) {
    let Some(entry) = state.jobs.get_mut(job_id) else {
        tracing::warn!(
            job_id = %job_id,
            agent_id = %agent_id,
            "JobProjector: AgentResult for unknown job; ignoring"
        );
        return;
    };
    let job_state = agent_id_to_job_state(agent_id);
    // Reviewer / Tester — try to parse a Verdict. Fail-soft on
    // parse error per WP §"Notes / risks".
    let verdict = if matches!(job_state, JobState::Review | JobState::Test) {
        match parse_verdict(assistant_text) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(
                    job_id = %job_id,
                    agent_id = %agent_id,
                    error = %e.message(),
                    "JobProjector: verdict parse failed; \
                     persisting stage with verdict_json=NULL"
                );
                None
            }
        }
    } else {
        None
    };

    // Track the most recent rejected verdict so JobOutcome /
    // RetryStarted can attach it. The 'newest wins' policy
    // matches the FSM's `last_verdict` semantics (W3-12e).
    if let Some(v) = &verdict {
        if v.rejected() {
            entry.last_rejected_verdict = Some(v.clone());
        }
    }

    let stage = StageResult {
        state: job_state,
        specialist_id: agent_id.to_string(),
        assistant_text: assistant_text.to_string(),
        // The brain-driven path doesn't surface a `claude` session
        // id through the AgentResult event (the agent dispatcher
        // owns that detail). Future polish: thread it through via
        // the help-loop branch.
        session_id: String::new(),
        total_cost_usd,
        // `turn_count` lives on AgentResult but the Stage shape
        // wants `duration_ms`. We don't have a wall-clock duration
        // measurement on the bus event (the dispatcher doesn't
        // emit one), so we leave it 0 and let the JobOutcome
        // aggregator surface 0 too. Future polish: thread duration
        // through AgentResult.
        duration_ms: 0,
        verdict: verdict.clone(),
        // The brain doesn't run the W3-12f Classify stage
        // explicitly; if the user adds a `coordinator`-tagged
        // dispatch, the verdict_json column captures the persona's
        // emit but the structured `CoordinatorDecision` is not
        // parsed here.
        coordinator_decision: None,
    };
    let idx = entry.next_stage_idx;
    entry.next_stage_idx += 1;
    entry.stages.push(stage.clone());

    // Persist the stage row.
    let created_at_ms = ts * 1_000;
    if let Err(e) = persist_stage(pool, job_id, idx, &stage, created_at_ms).await {
        tracing::warn!(
            job_id = %job_id,
            agent_id = %agent_id,
            idx = idx,
            error = %e,
            "JobProjector: swarm_stages insert failed; \
             SwarmJobEvent::StageCompleted will still emit"
        );
    }

    let _ = turn_count; // not surfaced on StageResult; tracked via the bus row only

    emit_event(
        app,
        job_id,
        SwarmJobEvent::StageCompleted {
            job_id: job_id.to_string(),
            stage,
        },
    );
}

async fn on_job_cancel<R: Runtime>(
    app: &AppHandle<R>,
    state: &mut ProjectorState,
    pool: &DbPool,
    job_id: &str,
) {
    let cancelled_during = state
        .jobs
        .get(job_id)
        .and_then(|e| e.dispatch_history.last())
        .map(|t| {
            agent_id_to_job_state(t.strip_prefix("agent:").unwrap_or(t))
        })
        .unwrap_or(JobState::Init);

    emit_event(
        app,
        job_id,
        SwarmJobEvent::Cancelled {
            job_id: job_id.to_string(),
            cancelled_during,
        },
    );

    // Side effect: flip swarm_jobs to Failed with the canonical
    // cancelled message. The Finished event arrives on a
    // following JobFinished envelope, so don't stamp finished_at_ms
    // here — let `on_job_finished` do that.
    if let Err(e) = update_job_cancelled(pool, job_id).await {
        tracing::warn!(
            job_id = %job_id,
            error = %e,
            "JobProjector: swarm_jobs cancel update failed"
        );
    }
}

async fn on_job_finished<R: Runtime>(
    app: &AppHandle<R>,
    state: &mut ProjectorState,
    pool: &DbPool,
    job_id: &str,
    outcome: &str,
    summary: &str,
) {
    let final_state = if outcome == "done" {
        JobState::Done
    } else {
        // brain emits 'failed' / 'ask_user' / 'cancelled'; all map
        // to Failed at the JobOutcome wire shape (matches the
        // FSM's contract).
        JobState::Failed
    };
    let last_error = if final_state == JobState::Failed {
        Some(summary.to_string())
    } else {
        None
    };
    // Pull the entry's accumulated stages + verdict for the
    // outcome shape. Missing entry (JobFinished without prior
    // JobStarted) falls back to a minimal outcome — same shape as
    // the W5-03 stub returned earlier.
    let (stages, last_verdict, total_cost_usd, total_duration_ms) = match state
        .jobs
        .get(job_id)
    {
        Some(entry) => {
            let cost: f64 = entry.stages.iter().map(|s| s.total_cost_usd).sum();
            let dur: u64 = entry.stages.iter().map(|s| s.duration_ms).sum();
            (
                entry.stages.clone(),
                entry.last_rejected_verdict.clone(),
                cost,
                dur,
            )
        }
        None => (Vec::new(), None, 0.0, 0),
    };

    let outcome_shape = JobOutcome {
        job_id: job_id.to_string(),
        final_state,
        stages,
        // Brain-side last_error includes the summary text on the
        // failed branch; on success we leave it None.
        last_error: last_error.clone(),
        total_cost_usd,
        total_duration_ms,
        // Tie last_verdict only when the brain itself signalled
        // failure due to a Verdict reject (we don't know that
        // explicitly, but the heuristic of 'failed AND most
        // recent rejected verdict exists' matches the FSM).
        last_verdict: if final_state == JobState::Failed {
            last_verdict
        } else {
            None
        },
    };

    emit_event(
        app,
        job_id,
        SwarmJobEvent::Finished {
            job_id: job_id.to_string(),
            outcome: outcome_shape,
        },
    );

    // Persist terminal state.
    if let Err(e) = update_job_finished(
        pool,
        job_id,
        final_state,
        last_error.as_deref(),
        crate::time::now_millis(),
    )
    .await
    {
        tracing::warn!(
            job_id = %job_id,
            error = %e,
            "JobProjector: swarm_jobs finish update failed"
        );
    }

    // Drop the per-job state — the row is terminal; downstream
    // queries hit the SQL store.
    state.jobs.remove(job_id);
}

// ---------------------------------------------------------------------
// SwarmJobEvent emission helper
// ---------------------------------------------------------------------

/// Emit one [`SwarmJobEvent`] on the `swarm:job:{job_id}:event`
/// channel. Errors are swallowed with a structured warning —
/// matches the FSM's `emit_swarm_event` policy (the IPC return
/// value is the source of truth, the event is a wake-up
/// optimisation for live UIs).
fn emit_event<R: Runtime>(
    app: &AppHandle<R>,
    job_id: &str,
    event: SwarmJobEvent,
) {
    let event_name = events::swarm_job_event(job_id);
    if let Err(e) = app.emit(&event_name, event) {
        tracing::warn!(
            event_name = %event_name,
            error = %e,
            "JobProjector: swarm event emit failed; continuing"
        );
    }
}

// ---------------------------------------------------------------------
// Pure helpers — agent_id mapping, retry detection, event filtering
// ---------------------------------------------------------------------

/// Map an `agent_id` (post-`agent:` prefix strip) to a [`JobState`]
/// per the W5-04 contract §5 table. Returns `Build` for unknown
/// agents — defensive default; future personas (post-W5) need an
/// explicit row in this table or they'll show as "Build" in the UI.
fn agent_id_to_job_state(agent_id: &str) -> JobState {
    match agent_id {
        "scout" => JobState::Scout,
        "coordinator" => JobState::Classify,
        "planner" => JobState::Plan,
        "backend-builder" | "frontend-builder" => JobState::Build,
        "backend-reviewer" | "frontend-reviewer" => JobState::Review,
        "integration-tester" => JobState::Test,
        // Defensive fallback. The W5-04 tests pin every personavin
        // the table; an unknown id surfaces here for the warn-log
        // path, with `Build` as a tame default.
        _ => JobState::Build,
    }
}

/// Retry detection: a `target` is a retry iff it has been
/// dispatched to before in the same job's `dispatch_history`.
/// Returns `Some(attempt_count)` (1-indexed retry count: first
/// retry is "attempt 2") on a retry, `None` otherwise.
///
/// Pseudocode from WP-W5-04 §6 — `dispatch_history` stores
/// targets in chronological order; we count prior occurrences
/// of `target` and return `prior + 1` (the new dispatch is the
/// (prior+1)-th attempt).
pub(super) fn is_retry_dispatch(
    target: &str,
    dispatch_history: &[String],
) -> Option<u32> {
    let prior = dispatch_history
        .iter()
        .filter(|t| t.as_str() == target)
        .count();
    if prior > 0 {
        Some(prior as u32 + 1)
    } else {
        None
    }
}

/// Pull a job_id off any [`MailboxEvent`] variant that carries
/// one. Used by `build_outcome` to filter the workspace event log
/// to one job. Note: Note has no job_id; returns None.
fn event_job_id(event: &MailboxEvent) -> Option<&str> {
    match event {
        MailboxEvent::TaskDispatch { job_id, .. }
        | MailboxEvent::AgentResult { job_id, .. }
        | MailboxEvent::AgentHelpRequest { job_id, .. }
        | MailboxEvent::CoordinatorHelpOutcome { job_id, .. }
        | MailboxEvent::JobStarted { job_id, .. }
        | MailboxEvent::JobFinished { job_id, .. }
        | MailboxEvent::JobCancel { job_id } => Some(job_id),
        MailboxEvent::Note => None,
    }
}

// ---------------------------------------------------------------------
// SQL helpers — `swarm_jobs` / `swarm_stages` write-through
// ---------------------------------------------------------------------

/// Insert a brain-driven `swarm_jobs` row. Idempotent at the
/// projector level: if the row already exists (the IPC pre-
/// inserted via `try_acquire_workspace`), the unique-key
/// violation is swallowed and the projector continues. We do NOT
/// migrate the existing row's `source` value — the IPC always
/// inserts with `source='brain'` for v2 jobs, so the value lines
/// up with the projector's intent.
async fn upsert_brain_job_row(
    pool: &DbPool,
    job: &Job,
    workspace_id: &str,
) -> Result<(), AppError> {
    // Detect existing row first; insert only when missing. SQLite's
    // INSERT OR IGNORE would also work but we want to surface
    // unrelated errors (e.g. column-default drift) cleanly.
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM swarm_jobs WHERE id = ?",
    )
    .bind(&job.id)
    .fetch_one(pool)
    .await?;
    if exists > 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO swarm_jobs \
         (id, workspace_id, goal, created_at_ms, state, retry_count, last_error, finished_at_ms, last_verdict_json, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&job.id)
    .bind(workspace_id)
    .bind(&job.goal)
    .bind(job.created_at_ms)
    .bind(job.state.as_db_str())
    .bind(job.retry_count as i64)
    .bind(job.last_error.as_deref())
    .bind(Option::<i64>::None)
    .bind(Option::<String>::None)
    .bind(&job.source)
    .execute(pool)
    .await?;
    Ok(())
}

/// Append one stage row. Mirrors `coordinator::store::insert_stage`
/// but reachable from outside the `coordinator` module — the
/// projector lives next to (not inside) `coordinator/`. Same
/// column set / same idx semantics so reads via
/// `coordinator::store::get_job_detail` see the same shape FSM-
/// authored stages produce.
async fn persist_stage(
    pool: &DbPool,
    job_id: &str,
    idx: u32,
    stage: &StageResult,
    created_at_ms: i64,
) -> Result<(), AppError> {
    let verdict_json = match stage.verdict.as_ref() {
        None => None,
        Some(v) => Some(serde_json::to_string(v).map_err(|e| {
            AppError::Internal(format!(
                "JobProjector: failed to serialize Verdict: {e}"
            ))
        })?),
    };
    let decision_json = match stage.coordinator_decision.as_ref() {
        None => None,
        Some(d) => Some(serde_json::to_string(d).map_err(|e| {
            AppError::Internal(format!(
                "JobProjector: failed to serialize CoordinatorDecision: {e}"
            ))
        })?),
    };
    sqlx::query(
        "INSERT INTO swarm_stages \
         (job_id, idx, state, specialist_id, assistant_text, session_id, total_cost_usd, duration_ms, created_at_ms, verdict_json, decision_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(idx as i64)
    .bind(stage.state.as_db_str())
    .bind(&stage.specialist_id)
    .bind(&stage.assistant_text)
    .bind(&stage.session_id)
    .bind(stage.total_cost_usd)
    .bind(stage.duration_ms as i64)
    .bind(created_at_ms)
    .bind(verdict_json)
    .bind(decision_json)
    .execute(pool)
    .await?;
    Ok(())
}

/// Cancellation update — flip the row to Failed with the
/// canonical `cancelled by user` last_error. `finished_at_ms` is
/// NOT stamped here; the trailing JobFinished stamp does that.
async fn update_job_cancelled(
    pool: &DbPool,
    job_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE swarm_jobs \
         SET state = ?, last_error = ? \
         WHERE id = ?",
    )
    .bind(JobState::Failed.as_db_str())
    .bind("cancelled by user")
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Terminal update. Stamps `state`, `last_error`, `finished_at_ms`
/// in one statement. Does NOT touch `last_verdict_json` — the
/// projector's per-stage rows already carry the verdicts; the
/// row-level `last_verdict_json` is FSM-only bookkeeping.
async fn update_job_finished(
    pool: &DbPool,
    job_id: &str,
    state: JobState,
    last_error: Option<&str>,
    finished_at_ms: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE swarm_jobs \
         SET state = ?, last_error = COALESCE(?, last_error), finished_at_ms = ? \
         WHERE id = ?",
    )
    .bind(state.as_db_str())
    .bind(last_error)
    .bind(finished_at_ms)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------
// build_outcome aggregation
// ---------------------------------------------------------------------

/// Walks the event log (chronological order) and accumulates the
/// fields needed for [`JobOutcome`]. Stateless across jobs — one
/// builder per `build_outcome` call.
struct OutcomeBuilder {
    job_id: String,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    final_state: JobState,
    last_error: Option<String>,
    last_rejected_verdict: Option<Verdict>,
    stages: Vec<StageResult>,
    /// Track agent_id → number of prior dispatches (for retry
    /// observation, but `build_outcome` doesn't emit retries — it
    /// just totalises stages).
    _dispatch_history: Vec<String>,
}

impl OutcomeBuilder {
    fn new(job_id: String) -> Self {
        Self {
            job_id,
            started_at_ms: 0,
            finished_at_ms: None,
            final_state: JobState::Failed,
            last_error: Some("no JobFinished event in log".into()),
            last_rejected_verdict: None,
            stages: Vec::new(),
            _dispatch_history: Vec::new(),
        }
    }

    fn observe(&mut self, env: &MailboxEnvelope) {
        match &env.event {
            MailboxEvent::JobStarted { .. } => {
                self.started_at_ms = env.ts * 1_000;
            }
            MailboxEvent::TaskDispatch { target, .. } => {
                self._dispatch_history.push(target.clone());
            }
            MailboxEvent::AgentResult {
                agent_id,
                assistant_text,
                total_cost_usd,
                ..
            } => {
                let job_state = agent_id_to_job_state(agent_id);
                let verdict = if matches!(
                    job_state,
                    JobState::Review | JobState::Test
                ) {
                    parse_verdict(assistant_text).ok()
                } else {
                    None
                };
                if let Some(v) = &verdict {
                    if v.rejected() {
                        self.last_rejected_verdict = Some(v.clone());
                    }
                }
                self.stages.push(StageResult {
                    state: job_state,
                    specialist_id: agent_id.clone(),
                    assistant_text: assistant_text.clone(),
                    session_id: String::new(),
                    total_cost_usd: *total_cost_usd,
                    duration_ms: 0,
                    verdict,
                    coordinator_decision: None,
                });
            }
            MailboxEvent::JobFinished {
                outcome, summary, ..
            } => {
                self.final_state = if outcome == "done" {
                    JobState::Done
                } else {
                    JobState::Failed
                };
                self.last_error = if self.final_state == JobState::Failed {
                    Some(summary.clone())
                } else {
                    None
                };
                self.finished_at_ms = Some(env.ts * 1_000);
            }
            MailboxEvent::JobCancel { .. } => {
                self.final_state = JobState::Failed;
                self.last_error = Some("cancelled by user".into());
            }
            // help requests / outcomes / notes don't shape the
            // outcome aggregate.
            _ => {}
        }
    }

    async fn finish(self, _pool: &DbPool) -> Result<JobOutcome, AppError> {
        let total_cost_usd: f64 =
            self.stages.iter().map(|s| s.total_cost_usd).sum();
        let total_duration_ms = self
            .finished_at_ms
            .map(|f| (f - self.started_at_ms).max(0) as u64)
            .unwrap_or(0);
        let last_verdict = if self.final_state == JobState::Failed {
            self.last_rejected_verdict
        } else {
            None
        };
        Ok(JobOutcome {
            job_id: self.job_id,
            final_state: self.final_state,
            stages: self.stages,
            last_error: self.last_error,
            total_cost_usd,
            total_duration_ms,
            last_verdict,
        })
    }
}

// ---------------------------------------------------------------------
// Hydration helper for `swarm:get_job` post-projector
// ---------------------------------------------------------------------

/// Convenience accessor for tests and `swarm_get_job`: reads the
/// stored detail from SQL. Wraps `coordinator::store::get_job_detail`
/// since that helper is `pub(super)` to the coordinator module —
/// the projector lives outside that module so it can't call it
/// directly. The shim also lets us add brain-specific shaping in
/// the future (e.g. surface the projector's in-memory entry when
/// the row is still in flight).
pub async fn get_brain_job_detail(
    pool: &DbPool,
    job_id: &str,
) -> Result<Option<JobDetail>, AppError> {
    // Defer to a SQL query that mirrors the coordinator helper
    // but is reachable from this module.
    let row = sqlx::query(
        "SELECT id, workspace_id, goal, created_at_ms, finished_at_ms, \
                state, retry_count, last_error, last_verdict_json, source \
         FROM swarm_jobs \
         WHERE id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    use sqlx::Row;
    let id: String = row.try_get("id")?;
    let workspace_id: String = row.try_get("workspace_id")?;
    let goal: String = row.try_get("goal")?;
    let created_at_ms: i64 = row.try_get("created_at_ms")?;
    let finished_at_ms: Option<i64> = row.try_get("finished_at_ms")?;
    let state_str: String = row.try_get("state")?;
    let retry_count_i: i64 = row.try_get("retry_count")?;
    let last_error: Option<String> = row.try_get("last_error")?;
    let last_verdict_json: Option<String> = row.try_get("last_verdict_json")?;
    let source: String = row.try_get("source")?;
    let state = JobState::from_db_str(&state_str)?;
    let last_verdict = match last_verdict_json {
        None => None,
        Some(s) => Some(serde_json::from_str::<Verdict>(&s).map_err(|e| {
            AppError::Internal(format!(
                "JobProjector: failed to deserialize Verdict from DB: {e}"
            ))
        })?),
    };
    let stages = fetch_brain_stages(pool, &id).await?;
    let total_cost_usd: f64 = stages.iter().map(|s| s.total_cost_usd).sum();
    let total_duration_ms: u64 = stages.iter().map(|s| s.duration_ms).sum();
    Ok(Some(JobDetail {
        id,
        workspace_id,
        goal,
        created_at_ms,
        finished_at_ms,
        state,
        retry_count: retry_count_i.max(0) as u32,
        stages,
        last_error,
        total_cost_usd,
        total_duration_ms,
        last_verdict,
        source,
    }))
}

/// SQL helper paired with [`get_brain_job_detail`]. Mirrors
/// `coordinator::store::fetch_stages` byte-for-byte (the column
/// list is the same), but lives here so we can read from outside
/// the `coordinator` module.
async fn fetch_brain_stages(
    pool: &DbPool,
    job_id: &str,
) -> Result<Vec<StageResult>, AppError> {
    use sqlx::Row;
    use crate::swarm::coordinator::CoordinatorDecision;
    let rows = sqlx::query(
        "SELECT idx, state, specialist_id, assistant_text, session_id, \
                total_cost_usd, duration_ms, verdict_json, decision_json \
         FROM swarm_stages \
         WHERE job_id = ? \
         ORDER BY idx ASC",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let state_str: String = row.try_get("state")?;
        let specialist_id: String = row.try_get("specialist_id")?;
        let assistant_text: String = row.try_get("assistant_text")?;
        let session_id: String = row.try_get("session_id")?;
        let total_cost_usd: f64 = row.try_get("total_cost_usd")?;
        let duration_ms_i: i64 = row.try_get("duration_ms")?;
        let verdict_json: Option<String> = row.try_get("verdict_json")?;
        let decision_json: Option<String> = row.try_get("decision_json")?;
        let state = JobState::from_db_str(&state_str)?;
        let verdict = match verdict_json {
            None => None,
            Some(s) => Some(serde_json::from_str::<Verdict>(&s).map_err(|e| {
                AppError::Internal(format!(
                    "JobProjector: failed to deserialize Verdict: {e}"
                ))
            })?),
        };
        let coordinator_decision = match decision_json {
            None => None,
            Some(s) => Some(serde_json::from_str::<CoordinatorDecision>(&s).map_err(
                |e| {
                    AppError::Internal(format!(
                        "JobProjector: failed to deserialize CoordinatorDecision: {e}"
                    ))
                },
            )?),
        };
        out.push(StageResult {
            state,
            specialist_id,
            assistant_text,
            session_id,
            total_cost_usd,
            duration_ms: duration_ms_i.max(0) as u64,
            verdict,
            coordinator_decision,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::coordinator::Verdict;
    use crate::test_support::mock_app_with_pool;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tauri::Listener;

    /// One captured `swarm:job:{id}:event` payload. `kind` is the
    /// top-level `kind` tag from `SwarmJobEvent`'s
    /// `#[serde(tag = "kind")]`; `json` is the full payload Value
    /// so individual assertions can dig into per-variant fields.
    /// `SwarmJobEvent` is `Serialize`-only (no Deserialize), which
    /// is why we capture as `Value` rather than the typed enum.
    #[derive(Debug, Clone)]
    struct CapturedEvent {
        kind: String,
        json: serde_json::Value,
    }

    /// Capture every `swarm:job:{job_id}:event` payload in
    /// chronological order. Mirrors the FSM tests' `capture_events`
    /// helper but scoped to this module so we don't reach into
    /// `coordinator`.
    fn install_event_capturer<R: Runtime>(
        app: &tauri::App<R>,
        job_id: &str,
    ) -> Arc<StdMutex<Vec<CapturedEvent>>> {
        let captured: Arc<StdMutex<Vec<CapturedEvent>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_w = Arc::clone(&captured);
        app.listen(events::swarm_job_event(job_id), move |event| {
            let payload = event.payload().to_string();
            if let Ok(value) =
                serde_json::from_str::<serde_json::Value>(&payload)
            {
                let kind = value
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                captured_w
                    .lock()
                    .expect("lock")
                    .push(CapturedEvent { kind, json: value });
            }
        });
        captured
    }

    /// Wait until the captured events match a predicate, polling
    /// on 20 ms ticks. Bounded so a regression surfaces as a test
    /// failure rather than a hang.
    async fn drain_until<F: Fn(&[CapturedEvent]) -> bool>(
        captured: &Arc<StdMutex<Vec<CapturedEvent>>>,
        pred: F,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let snap = captured.lock().expect("lock").clone();
            if pred(&snap) {
                return;
            }
            if std::time::Instant::now() > deadline {
                let snap = captured.lock().expect("lock").clone();
                panic!(
                    "timeout waiting for predicate; captured kinds: {:?}",
                    snap.iter().map(|e| e.kind.clone()).collect::<Vec<_>>()
                );
            }
        }
    }

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

    #[tokio::test]
    async fn projector_emits_started_on_job_started() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let captured = install_event_capturer(&app, "j-started");
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );

        // Sleep one tick to let `subscribe` complete inside the
        // task before we emit (otherwise the broadcast send would
        // race the recv on slow runners).
        tokio::time::sleep(Duration::from_millis(20)).await;

        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-started".into(),
                workspace_id: "default".into(),
                goal: "test goal".into(),
            },
        )
        .await
        .expect("emit");

        drain_until(&captured, |evts| {
            evts.iter().any(|e| e.kind == "started")
        })
        .await;

        let snap = captured.lock().expect("lock").clone();
        let started = snap
            .iter()
            .find(|e| e.kind == "started")
            .expect("started present");
        assert_eq!(
            started.json.get("job_id").and_then(|v| v.as_str()),
            Some("j-started")
        );
        assert_eq!(
            started.json.get("workspace_id").and_then(|v| v.as_str()),
            Some("default")
        );
        assert_eq!(
            started.json.get("goal").and_then(|v| v.as_str()),
            Some("test goal")
        );
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn projector_emits_stage_started_on_task_dispatch() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let captured = install_event_capturer(&app, "j-disp");
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-disp".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:scout",
            "dispatch",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-disp".into(),
                target: "agent:scout".into(),
                prompt: "investigate".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit dispatch");
        drain_until(&captured, |evts| {
            evts.iter().any(|e| e.kind == "stage_started")
        })
        .await;
        let snap = captured.lock().expect("lock").clone();
        let ev = snap
            .iter()
            .find(|e| e.kind == "stage_started")
            .expect("StageStarted present");
        assert_eq!(
            ev.json.get("job_id").and_then(|v| v.as_str()),
            Some("j-disp")
        );
        assert_eq!(
            ev.json.get("state").and_then(|v| v.as_str()),
            Some("scout")
        );
        assert_eq!(
            ev.json.get("specialist_id").and_then(|v| v.as_str()),
            Some("scout")
        );
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn projector_emits_stage_completed_on_agent_result() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let captured = install_event_capturer(&app, "j-res");
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-res".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "result",
            None,
            MailboxEvent::AgentResult {
                job_id: "j-res".into(),
                agent_id: "scout".into(),
                assistant_text: "found auth.rs".into(),
                total_cost_usd: 0.05,
                turn_count: 1,
            },
        )
        .await
        .expect("emit result");
        drain_until(&captured, |evts| {
            evts.iter().any(|e| e.kind == "stage_completed")
        })
        .await;
        let snap = captured.lock().expect("lock").clone();
        let ev = snap
            .iter()
            .find(|e| e.kind == "stage_completed")
            .expect("StageCompleted present");
        assert_eq!(
            ev.json.get("job_id").and_then(|v| v.as_str()),
            Some("j-res")
        );
        let stage = ev.json.get("stage").expect("stage embedded");
        assert_eq!(
            stage.get("state").and_then(|v| v.as_str()),
            Some("scout")
        );
        assert_eq!(
            stage.get("specialistId").and_then(|v| v.as_str()),
            Some("scout")
        );
        assert_eq!(
            stage.get("assistantText").and_then(|v| v.as_str()),
            Some("found auth.rs")
        );
        let cost = stage
            .get("totalCostUsd")
            .and_then(|v| v.as_f64())
            .expect("cost number");
        assert!((cost - 0.05).abs() < f64::EPSILON);
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn projector_inserts_swarm_jobs_row_with_brain_source() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-brainrow".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");

        // Poll the SQL row with a deadline.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT state, source FROM swarm_jobs WHERE id = ?",
            )
            .bind("j-brainrow")
            .fetch_optional(&pool)
            .await
            .expect("query");
            if let Some((state, source)) = row {
                assert_eq!(state, "init");
                assert_eq!(source, "brain");
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("swarm_jobs row never landed");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn projector_inserts_swarm_stages_row() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-stagerow".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit started");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "result",
            None,
            MailboxEvent::AgentResult {
                job_id: "j-stagerow".into(),
                agent_id: "planner".into(),
                assistant_text: "plan".into(),
                total_cost_usd: 0.10,
                turn_count: 2,
            },
        )
        .await
        .expect("emit result");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM swarm_stages WHERE job_id = ?",
            )
            .bind("j-stagerow")
            .fetch_one(&pool)
            .await
            .expect("count");
            if count >= 1 {
                let (state_s, specialist_s): (String, String) =
                    sqlx::query_as(
                        "SELECT state, specialist_id FROM swarm_stages WHERE job_id = ? AND idx = 0",
                    )
                    .bind("j-stagerow")
                    .fetch_one(&pool)
                    .await
                    .expect("row");
                assert_eq!(state_s, "plan");
                assert_eq!(specialist_s, "planner");
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("swarm_stages row never landed");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        projector.shutdown().await;
    }

    #[test]
    fn projector_maps_agent_id_to_correct_job_state() {
        // Pin every row of the W5-04 §5 mapping table.
        assert_eq!(agent_id_to_job_state("scout"), JobState::Scout);
        assert_eq!(
            agent_id_to_job_state("coordinator"),
            JobState::Classify
        );
        assert_eq!(agent_id_to_job_state("planner"), JobState::Plan);
        assert_eq!(
            agent_id_to_job_state("backend-builder"),
            JobState::Build
        );
        assert_eq!(
            agent_id_to_job_state("frontend-builder"),
            JobState::Build
        );
        assert_eq!(
            agent_id_to_job_state("backend-reviewer"),
            JobState::Review
        );
        assert_eq!(
            agent_id_to_job_state("frontend-reviewer"),
            JobState::Review
        );
        assert_eq!(
            agent_id_to_job_state("integration-tester"),
            JobState::Test
        );
        // Defensive default for unknown ids.
        assert_eq!(
            agent_id_to_job_state("future-persona-id"),
            JobState::Build
        );
    }

    #[tokio::test]
    async fn projector_emits_retry_attempt_on_repeated_dispatch() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let captured = install_event_capturer(&app, "j-retry");
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-retry".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");
        // First dispatch — no retry.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:planner",
            "first",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-retry".into(),
                target: "agent:planner".into(),
                prompt: "plan v1".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");
        // Wait for the first StageStarted to land so the
        // projector's history is committed before the second
        // dispatch (otherwise the broadcast race could see both
        // dispatches before the first updates history).
        drain_until(&captured, |evts| {
            evts.iter().filter(|e| e.kind == "stage_started").count() >= 1
        })
        .await;
        // Second dispatch — same target → retry.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:planner",
            "second",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-retry".into(),
                target: "agent:planner".into(),
                prompt: "plan v2".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");
        drain_until(&captured, |evts| {
            evts.iter().any(|e| e.kind == "retry_started")
        })
        .await;
        let snap = captured.lock().expect("lock").clone();
        let ev = snap
            .iter()
            .find(|e| e.kind == "retry_started")
            .expect("RetryStarted present");
        assert_eq!(
            ev.json.get("attempt").and_then(|v| v.as_u64()),
            Some(2),
            "second dispatch is attempt 2"
        );
        assert_eq!(
            ev.json.get("triggered_by").and_then(|v| v.as_str()),
            Some("plan")
        );
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn projector_does_not_emit_retry_on_first_dispatch() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let captured = install_event_capturer(&app, "j-noretry");
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-noretry".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:scout",
            "first",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-noretry".into(),
                target: "agent:scout".into(),
                prompt: "go".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");
        drain_until(&captured, |evts| {
            evts.iter().any(|e| e.kind == "stage_started")
        })
        .await;
        // Settle a beat so any (incorrect) RetryStarted would land.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let snap = captured.lock().expect("lock").clone();
        let retry_count = snap.iter().filter(|e| e.kind == "retry_started").count();
        assert_eq!(retry_count, 0, "first dispatch must not emit RetryStarted");
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn projector_emits_finished_on_job_finished_done() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let captured = install_event_capturer(&app, "j-fin-done");
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-fin-done".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit started");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:user",
            "finished",
            None,
            MailboxEvent::JobFinished {
                job_id: "j-fin-done".into(),
                outcome: "done".into(),
                summary: "ok".into(),
            },
        )
        .await
        .expect("emit finished");
        drain_until(&captured, |evts| evts.iter().any(|e| e.kind == "finished"))
            .await;
        let snap = captured.lock().expect("lock").clone();
        let ev = snap
            .iter()
            .find(|e| e.kind == "finished")
            .expect("Finished present");
        let outcome = ev.json.get("outcome").expect("outcome");
        assert_eq!(
            outcome.get("finalState").and_then(|v| v.as_str()),
            Some("done")
        );
        assert!(
            outcome.get("lastError").map(|v| v.is_null()).unwrap_or(true),
            "last_error null on done"
        );
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn projector_emits_finished_on_job_finished_failed() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let captured = install_event_capturer(&app, "j-fin-failed");
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-fin-failed".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:user",
            "finished",
            None,
            MailboxEvent::JobFinished {
                job_id: "j-fin-failed".into(),
                outcome: "failed".into(),
                summary: "exceeded max dispatches".into(),
            },
        )
        .await
        .expect("emit");
        drain_until(&captured, |evts| evts.iter().any(|e| e.kind == "finished"))
            .await;
        let snap = captured.lock().expect("lock").clone();
        let ev = snap
            .iter()
            .find(|e| e.kind == "finished")
            .expect("Finished present");
        let outcome = ev.json.get("outcome").expect("outcome");
        assert_eq!(
            outcome.get("finalState").and_then(|v| v.as_str()),
            Some("failed")
        );
        assert_eq!(
            outcome.get("lastError").and_then(|v| v.as_str()),
            Some("exceeded max dispatches")
        );
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn projector_emits_cancelled_then_finished_on_job_cancel() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let captured = install_event_capturer(&app, "j-cancel");
        let projector = JobProjector::spawn(
            app.handle().clone(),
            "default".into(),
            Arc::clone(&bus),
            pool.clone(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-cancel".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");
        // Mid-stage dispatch so cancelled_during is non-default.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:scout",
            "dispatch",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-cancel".into(),
                target: "agent:scout".into(),
                prompt: "go".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");
        drain_until(&captured, |evts| {
            evts.iter().any(|e| e.kind == "stage_started")
        })
        .await;
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "cancel",
            None,
            MailboxEvent::JobCancel {
                job_id: "j-cancel".into(),
            },
        )
        .await
        .expect("emit cancel");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:user",
            "finished",
            None,
            MailboxEvent::JobFinished {
                job_id: "j-cancel".into(),
                outcome: "failed".into(),
                summary: "cancelled by user".into(),
            },
        )
        .await
        .expect("emit");
        drain_until(&captured, |evts| {
            let has_cancel = evts.iter().any(|e| e.kind == "cancelled");
            let has_finished = evts.iter().any(|e| e.kind == "finished");
            has_cancel && has_finished
        })
        .await;
        let snap = captured.lock().expect("lock").clone();
        let cancel_idx = snap
            .iter()
            .position(|e| e.kind == "cancelled")
            .expect("Cancelled present");
        let finished_idx = snap
            .iter()
            .position(|e| e.kind == "finished")
            .expect("Finished present");
        assert!(
            cancel_idx < finished_idx,
            "Cancelled must precede Finished: kinds={:?}",
            snap.iter().map(|e| e.kind.clone()).collect::<Vec<_>>()
        );
        let cancel = &snap[cancel_idx];
        assert_eq!(
            cancel.json.get("cancelled_during").and_then(|v| v.as_str()),
            Some("scout")
        );
        projector.shutdown().await;
    }

    #[tokio::test]
    async fn build_outcome_walks_event_log_correctly() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        // Seed a complete event chain via the bus directly. No
        // projector needed — `build_outcome` is pure aggregation.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-bo".into(),
                workspace_id: "default".into(),
                goal: "build the thing".into(),
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "result",
            None,
            MailboxEvent::AgentResult {
                job_id: "j-bo".into(),
                agent_id: "scout".into(),
                assistant_text: "found".into(),
                total_cost_usd: 0.10,
                turn_count: 1,
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:planner",
            "agent:coordinator",
            "result",
            None,
            MailboxEvent::AgentResult {
                job_id: "j-bo".into(),
                agent_id: "planner".into(),
                assistant_text: "plan".into(),
                total_cost_usd: 0.20,
                turn_count: 1,
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:user",
            "finished",
            None,
            MailboxEvent::JobFinished {
                job_id: "j-bo".into(),
                outcome: "done".into(),
                summary: "ok".into(),
            },
        )
        .await
        .expect("emit");

        let outcome = JobProjector::build_outcome(&bus, &pool, "j-bo")
            .await
            .expect("build_outcome");
        assert_eq!(outcome.job_id, "j-bo");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 2);
        assert_eq!(outcome.stages[0].state, JobState::Scout);
        assert_eq!(outcome.stages[1].state, JobState::Plan);
        assert!(
            (outcome.total_cost_usd - 0.30).abs() < 1e-9,
            "cost summed: {}",
            outcome.total_cost_usd
        );
        assert!(outcome.last_error.is_none());
        assert!(outcome.last_verdict.is_none());
    }

    #[tokio::test]
    async fn build_outcome_handles_brain_driven_job_with_retry() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        // A retry-shaped chain: planner → reviewer (rejected) →
        // planner (retry) → reviewer (approved) → finished.
        let job_id = "j-retry-bo";
        let dispatches: &[(MailboxEvent, &str)] = &[
            (
                MailboxEvent::JobStarted {
                    job_id: job_id.into(),
                    workspace_id: "default".into(),
                    goal: "g".into(),
                },
                "started",
            ),
            (
                MailboxEvent::AgentResult {
                    job_id: job_id.into(),
                    agent_id: "planner".into(),
                    assistant_text: "plan v1".into(),
                    total_cost_usd: 0.05,
                    turn_count: 1,
                },
                "plan-v1",
            ),
            (
                MailboxEvent::AgentResult {
                    job_id: job_id.into(),
                    agent_id: "backend-reviewer".into(),
                    assistant_text:
                        r#"{"approved":false,"issues":[],"summary":"plan rough"}"#
                            .into(),
                    total_cost_usd: 0.02,
                    turn_count: 1,
                },
                "rev-1",
            ),
            (
                MailboxEvent::AgentResult {
                    job_id: job_id.into(),
                    agent_id: "planner".into(),
                    assistant_text: "plan v2".into(),
                    total_cost_usd: 0.05,
                    turn_count: 1,
                },
                "plan-v2",
            ),
            (
                MailboxEvent::AgentResult {
                    job_id: job_id.into(),
                    agent_id: "backend-reviewer".into(),
                    assistant_text:
                        r#"{"approved":true,"issues":[],"summary":"good"}"#.into(),
                    total_cost_usd: 0.02,
                    turn_count: 1,
                },
                "rev-2",
            ),
            (
                MailboxEvent::JobFinished {
                    job_id: job_id.into(),
                    outcome: "done".into(),
                    summary: "ok".into(),
                },
                "fin",
            ),
        ];
        for (ev, sum) in dispatches {
            bus.emit_typed(
                app.handle(),
                "default",
                "agent:user",
                "agent:coordinator",
                sum,
                None,
                ev.clone(),
            )
            .await
            .expect("emit");
        }
        let outcome = JobProjector::build_outcome(&bus, &pool, job_id)
            .await
            .expect("build_outcome");
        assert_eq!(outcome.final_state, JobState::Done);
        assert_eq!(outcome.stages.len(), 4);
        // last stage is the approved reviewer.
        let last = outcome.stages.last().expect("non-empty");
        assert_eq!(last.state, JobState::Review);
        assert!(last.verdict.as_ref().expect("approved verdict").approved);
        assert!(outcome.last_verdict.is_none(), "Done outcome must clear last_verdict");
    }

    /// `swarm:get_job` semantics — a brain-driven job's row + stages
    /// reload through the legacy `JobDetail` shape.
    #[tokio::test]
    async fn swarm_get_job_returns_brain_driven_job_in_legacy_shape() {
        let (_, pool, _dir) = mock_app_with_pool().await;
        // Seed a brain-driven row directly via SQL so the test
        // doesn't depend on the projector running.
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count, source) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("j-brainshape")
        .bind("default")
        .bind("brain-shaped goal")
        .bind(123_i64)
        .bind("done")
        .bind(0_i64)
        .bind("brain")
        .execute(&pool)
        .await
        .expect("seed");
        sqlx::query(
            "INSERT INTO swarm_stages (job_id, idx, state, specialist_id, assistant_text, session_id, total_cost_usd, duration_ms, created_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("j-brainshape")
        .bind(0_i64)
        .bind("scout")
        .bind("scout")
        .bind("found")
        .bind("")
        .bind(0.05)
        .bind(0_i64)
        .bind(123_i64)
        .execute(&pool)
        .await
        .expect("seed stage");

        let detail = get_brain_job_detail(&pool, "j-brainshape")
            .await
            .expect("query")
            .expect("Some");
        assert_eq!(detail.id, "j-brainshape");
        assert_eq!(detail.workspace_id, "default");
        assert_eq!(detail.source, "brain");
        assert_eq!(detail.state, JobState::Done);
        assert_eq!(detail.stages.len(), 1);
        assert_eq!(detail.stages[0].state, JobState::Scout);
        assert_eq!(detail.stages[0].specialist_id, "scout");
    }

    /// The projector_emits_retry_attempt test covers RetryStarted
    /// emission via the bus; this micro-test pins the pure helper
    /// `is_retry_dispatch` so a regression on the prior-count math
    /// surfaces here, not via a slower e2e test.
    #[test]
    fn is_retry_dispatch_counts_prior_targets() {
        let history: Vec<String> = vec![];
        assert_eq!(is_retry_dispatch("agent:scout", &history), None);
        let history = vec!["agent:scout".to_string()];
        assert_eq!(is_retry_dispatch("agent:scout", &history), Some(2));
        let history =
            vec!["agent:scout".to_string(), "agent:scout".to_string()];
        assert_eq!(is_retry_dispatch("agent:scout", &history), Some(3));
        let history = vec!["agent:planner".to_string()];
        assert_eq!(is_retry_dispatch("agent:scout", &history), None);
    }

    /// Smoke for the verdict reject capture used by JobOutcome.last_verdict
    /// — pin that a rejected reviewer verdict, followed by a Failed
    /// JobFinished, surfaces the verdict on the outcome.
    #[tokio::test]
    async fn build_outcome_surfaces_last_rejected_verdict_on_failed() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let job_id = "j-verd-fail";
        let _ = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:user",
                "agent:coordinator",
                "started",
                None,
                MailboxEvent::JobStarted {
                    job_id: job_id.into(),
                    workspace_id: "default".into(),
                    goal: "g".into(),
                },
            )
            .await;
        let _ = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:backend-reviewer",
                "agent:coordinator",
                "rev",
                None,
                MailboxEvent::AgentResult {
                    job_id: job_id.into(),
                    agent_id: "backend-reviewer".into(),
                    assistant_text:
                        r#"{"approved":false,"issues":[],"summary":"bad"}"#
                            .into(),
                    total_cost_usd: 0.01,
                    turn_count: 1,
                },
            )
            .await;
        let _ = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:coordinator",
                "agent:user",
                "fin",
                None,
                MailboxEvent::JobFinished {
                    job_id: job_id.into(),
                    outcome: "failed".into(),
                    summary: "reviewer rejected".into(),
                },
            )
            .await;
        let outcome = JobProjector::build_outcome(&bus, &pool, job_id)
            .await
            .expect("build_outcome");
        assert_eq!(outcome.final_state, JobState::Failed);
        let v: &Verdict = outcome
            .last_verdict
            .as_ref()
            .expect("verdict surfaced on Failed");
        assert!(!v.approved);
        assert_eq!(v.summary, "bad");
    }
}
