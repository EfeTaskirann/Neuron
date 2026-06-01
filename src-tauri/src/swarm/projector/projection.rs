//! Projector state machine: the per-workspace mailbox consumer loop
//! plus per-event handlers that synthesise `SwarmJobEvent`s and
//! write through to `swarm_jobs` / `swarm_stages`. The wire-shape
//! FSM lives here; the `JobProjector` task in the parent module
//! drives it. Extracted verbatim from `projector.rs` (WP-W5-04).

use std::collections::HashMap;
use std::sync::Arc;

use tauri::{AppHandle, Runtime};
use tokio::sync::{broadcast, Notify};

use crate::db::DbPool;
use crate::swarm::coordinator::{
    parse_verdict, Job, JobOutcome, JobState, StageResult, SwarmJobEvent,
    Verdict,
};
use crate::swarm::mailbox_bus::{MailboxEnvelope, MailboxEvent};

use super::helpers::{agent_id_to_job_state, emit_event, is_retry_dispatch};
use super::persistence::{
    persist_stage, update_job_cancelled, update_job_finished,
    upsert_brain_job_row,
};

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

pub(super) struct ProjectorState {
    workspace_id: String,
    jobs: HashMap<String, ProjectorJobEntry>,
}

impl ProjectorState {
    pub(super) fn new(workspace_id: String) -> Self {
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
pub(super) async fn run_loop<R: Runtime>(
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
