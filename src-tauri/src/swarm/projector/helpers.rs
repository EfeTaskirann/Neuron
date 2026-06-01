//! Pure projector helpers shared across the submodules: `agent_id`
//! → [`JobState`] mapping, retry detection, mailbox-event job-id
//! extraction, and the `SwarmJobEvent` emit shim. Extracted from the
//! monolithic `projector.rs` (WP-W5-04) — behaviour verbatim.

use tauri::{AppHandle, Emitter, Runtime};

use crate::events;
use crate::swarm::coordinator::{JobState, SwarmJobEvent};
use crate::swarm::mailbox_bus::MailboxEvent;

/// Emit one [`SwarmJobEvent`] on the `swarm:job:{job_id}:event`
/// channel. Errors are swallowed with a structured warning —
/// matches the FSM's `emit_swarm_event` policy (the IPC return
/// value is the source of truth, the event is a wake-up
/// optimisation for live UIs).
pub(super) fn emit_event<R: Runtime>(
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

/// Map an `agent_id` (post-`agent:` prefix strip) to a [`JobState`]
/// per the W5-04 contract §5 table. Returns `Build` for unknown
/// agents — defensive default; future personas (post-W5) need an
/// explicit row in this table or they'll show as "Build" in the UI.
pub(super) fn agent_id_to_job_state(agent_id: &str) -> JobState {
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
pub(super) fn event_job_id(event: &MailboxEvent) -> Option<&str> {
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
