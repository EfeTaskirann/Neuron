//! Mailbox event-loop plumbing for the brain: the
//! [`LoopEventOutcome`] result, the `recv`-vs-`cancel` select, and
//! the per-job relevance filter.
//!
//! Split out of the monolithic `brain.rs` (WP-W5-03). Behaviour is
//! unchanged — the brain's `run_with_max` loop calls
//! [`wait_for_loop_event`] after every `Dispatch`.

use tokio::sync::{broadcast, Notify};

use crate::swarm::mailbox_bus::{MailboxEnvelope, MailboxEvent};

/// Outcome of one `wait_for_loop_event` iteration.
pub(super) enum LoopEventOutcome {
    Event(MailboxEnvelope),
    Cancelled,
    Closed,
}

/// Drain mailbox envelopes until we see one that's relevant to the
/// brain's loop (matching `job_id`, kind in {AgentResult,
/// AgentHelpRequest, JobCancel}). Filters out the brain's own
/// emits (TaskDispatch / JobStarted / CoordinatorHelpOutcome /
/// JobFinished) and events for OTHER jobs in the same workspace.
///
/// `cancel.notified()` races against `recv()` so a user-driven
/// cancel truncates the wait promptly.
pub(super) async fn wait_for_loop_event(
    receiver: &mut broadcast::Receiver<MailboxEnvelope>,
    job_id: &str,
    cancel: &Notify,
) -> LoopEventOutcome {
    loop {
        tokio::select! {
            biased;
            _ = cancel.notified() => return LoopEventOutcome::Cancelled,
            recv_result = receiver.recv() => {
                match recv_result {
                    Ok(envelope) => {
                        if envelope_is_relevant(&envelope, job_id) {
                            return LoopEventOutcome::Event(envelope);
                        }
                        // Otherwise loop and wait for the next event.
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            job_id = %job_id,
                            skipped = skipped,
                            "coordinator brain: broadcast receiver lagged; \
                             SQL log is source of truth — events skipped \
                             will surface via projector replay"
                        );
                        // Continue — the receiver auto-resyncs to the
                        // newest event after a lag.
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return LoopEventOutcome::Closed;
                    }
                }
            }
        }
    }
}

/// Whether `envelope` is one the brain's loop should consume.
/// Returns true for AgentResult / AgentHelpRequest / JobCancel
/// matching `job_id`; false for everything else (including the
/// brain's own emits).
fn envelope_is_relevant(envelope: &MailboxEnvelope, job_id: &str) -> bool {
    match &envelope.event {
        MailboxEvent::AgentResult { job_id: ev_job_id, .. }
        | MailboxEvent::AgentHelpRequest { job_id: ev_job_id, .. }
        | MailboxEvent::JobCancel { job_id: ev_job_id } => {
            ev_job_id == job_id
        }
        _ => false,
    }
}
