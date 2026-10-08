//! Projector state machine: the per-workspace mailbox consumer loop
//! plus the per-job bookkeeping it threads through the event
//! handlers. The wire-shape FSM's driver (`run_loop`) lives here;
//! the individual `MailboxEvent` → `SwarmJobEvent` handlers (and the
//! `handle_envelope` dispatcher) live in [`handlers`]. The
//! `JobProjector` task in the parent module drives `run_loop`.
//! Extracted verbatim from `projector.rs` (WP-W5-04).

use std::collections::HashMap;
use std::sync::Arc;

use tauri::{AppHandle, Runtime};
use tokio::sync::{broadcast, Notify};

use crate::db::DbPool;
use crate::swarm::coordinator::{StageResult, Verdict};
use crate::swarm::mailbox_bus::MailboxEnvelope;

mod handlers;

use handlers::handle_envelope;

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
pub(super) struct ProjectorJobEntry {
    #[allow(dead_code)]
    pub(super) workspace_id: String,
    #[allow(dead_code)]
    pub(super) goal: String,
    /// Chronological list of dispatch targets — used by the retry
    /// detector (`is_retry_dispatch` walks history newest-first).
    pub(super) dispatch_history: Vec<String>,
    /// Idx of the next stage row in `swarm_stages`. Increments on
    /// every `AgentResult` we persist. 0-based to match the
    /// existing `insert_stage(idx)` contract.
    pub(super) next_stage_idx: u32,
    /// Accumulated stage results — used by `JobOutcome` aggregation
    /// at JobFinished time.
    pub(super) stages: Vec<StageResult>,
    /// Wall-clock created_at_ms — needed for the `Started` event.
    #[allow(dead_code)]
    pub(super) created_at_ms: i64,
    /// Most recent rejected verdict, if any — flows into
    /// `JobOutcome.last_verdict` on a Failed termination.
    pub(super) last_rejected_verdict: Option<Verdict>,
}

pub(super) struct ProjectorState {
    workspace_id: String,
    pub(super) jobs: HashMap<String, ProjectorJobEntry>,
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
