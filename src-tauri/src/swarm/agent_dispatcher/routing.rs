//! Target parsing + the dispatcher's main `select!` loop.
//!
//! [`run_loop`] owns the biased `select!` over shutdown / broadcast
//! delivery and fans each routed `TaskDispatch` out into its own
//! child task (so the loop keeps draining cancel events while an
//! invoke is in flight). [`handle_envelope`] does the per-envelope
//! routing decision and spawns the invoke child via
//! [`super::invoke::drive_invoke`].

use std::sync::Arc;

use tauri::{AppHandle, Runtime};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::task::JoinHandle;

use crate::swarm::mailbox_bus::{
    MailboxBus, MailboxEnvelope, MailboxEvent,
};

use super::invoke::drive_invoke;
use super::invoker::AgentInvoker;
use super::InvokeSlot;

/// Strip the `agent:` prefix from a `MailboxEvent::TaskDispatch`
/// `target`. Returns `Some(<id>)` for `agent:<id>` (with non-empty
/// id), `None` otherwise. The agent-id is whatever follows the
/// `agent:` prefix verbatim — no further validation; the registry's
/// own `acquire_and_invoke_turn` rejects empty / whitespace ids
/// downstream so a malformed value lands as `InvalidInput` not a
/// silent skip.
pub fn parse_agent_target(target: &str) -> Option<&str> {
    let stripped = target.strip_prefix("agent:")?;
    if stripped.is_empty() {
        None
    } else {
        Some(stripped)
    }
}

/// Internal: the main loop spawns a child task per dispatch so the
/// loop can continue draining cancel events while the invoke is in
/// flight. The child task drives invoke + emit and clears the
/// `current_invoke` slot on completion.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_loop<R: Runtime, I: AgentInvoker>(
    app: AppHandle<R>,
    workspace_id: String,
    agent_id: String,
    invoker: Arc<I>,
    bus: Arc<MailboxBus>,
    mut receiver: broadcast::Receiver<MailboxEnvelope>,
    shutdown: Arc<Notify>,
    current_invoke: Arc<Mutex<Option<InvokeSlot>>>,
) {
    // We track outstanding invoke tasks so we can join them on
    // shutdown. Per WP §"Out of scope (multi-job-per-workspace)"
    // a single agent only sees one in-flight invoke at a time
    // today (the IPC + W5-05 lock enforces that), but we don't
    // assume that here — multiple TaskDispatch events targeting
    // this agent will fan out into parallel child tasks. The
    // `current_invoke` slot only tracks the *most recent* dispatch
    // for cancel routing (W5-05 hardens this).
    let mut invoke_tasks: Vec<JoinHandle<()>> = Vec::new();

    loop {
        // Reap any finished child tasks so the Vec doesn't grow
        // unbounded on a long-lived dispatcher.
        invoke_tasks.retain(|h| !h.is_finished());

        tokio::select! {
            biased;
            // Shutdown wins over event delivery — explicit so app
            // close drains promptly even when a burst of events is
            // queued.
            _ = shutdown.notified() => {
                tracing::debug!(
                    workspace_id = %workspace_id,
                    agent_id = %agent_id,
                    "agent dispatcher: shutdown signal received"
                );
                break;
            }
            recv_result = receiver.recv() => {
                match recv_result {
                    Ok(envelope) => {
                        if let Some(handle) = handle_envelope(
                            &app,
                            &workspace_id,
                            &agent_id,
                            &invoker,
                            &bus,
                            &current_invoke,
                            envelope,
                        ).await {
                            invoke_tasks.push(handle);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            workspace_id = %workspace_id,
                            agent_id = %agent_id,
                            skipped = skipped,
                            "agent dispatcher: broadcast receiver lagged; \
                             SQL log is source of truth — events skipped \
                             will surface via projector replay"
                        );
                        // Continue — the receiver auto-resyncs to the
                        // newest event after a lag.
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!(
                            workspace_id = %workspace_id,
                            agent_id = %agent_id,
                            "agent dispatcher: broadcast channel closed; \
                             exiting loop"
                        );
                        break;
                    }
                }
            }
        }
    }

    // Drain outstanding invoke tasks — shutdown should have
    // signalled cancel on the in-flight slot, so each task's
    // invoker.invoke_turn returns promptly. Best-effort: if any
    // panicked, ignore the join error.
    for handle in invoke_tasks {
        let _ = handle.await;
    }
}

/// Returns `Some(JoinHandle)` if a TaskDispatch was routed for
/// this dispatcher (so the caller can track it for graceful
/// drain on shutdown); `None` for all other event kinds and
/// for non-matching dispatches.
#[allow(clippy::too_many_arguments)]
async fn handle_envelope<R: Runtime, I: AgentInvoker>(
    app: &AppHandle<R>,
    workspace_id: &str,
    agent_id: &str,
    invoker: &Arc<I>,
    bus: &Arc<MailboxBus>,
    current_invoke: &Arc<Mutex<Option<InvokeSlot>>>,
    envelope: MailboxEnvelope,
) -> Option<JoinHandle<()>> {
    match &envelope.event {
        MailboxEvent::TaskDispatch {
            job_id,
            target,
            prompt,
            with_help_loop,
        } => {
            // Route — does this dispatch target *us*?
            let target_id = match parse_agent_target(target) {
                Some(id) => id,
                None => {
                    tracing::trace!(
                        workspace_id = %workspace_id,
                        agent_id = %agent_id,
                        target = %target,
                        "agent dispatcher: dispatch target lacks \
                         `agent:` prefix; ignoring"
                    );
                    return None;
                }
            };
            if target_id != agent_id {
                tracing::trace!(
                    workspace_id = %workspace_id,
                    agent_id = %agent_id,
                    target = %target,
                    "agent dispatcher: dispatch targets a different \
                     agent; ignoring"
                );
                return None;
            }

            // Set up the invoke slot so a later `JobCancel` can
            // reach the in-flight Notify.
            let cancel = Arc::new(Notify::new());
            {
                let mut slot = current_invoke.lock().await;
                *slot = Some(InvokeSlot {
                    job_id: job_id.clone(),
                    cancel: Arc::clone(&cancel),
                });
            }

            let job_id_for_emit = job_id.clone();
            let prompt_for_invoke = prompt.clone();
            let dispatch_id = envelope.id;
            let with_help_loop = *with_help_loop;

            tracing::debug!(
                workspace_id = %workspace_id,
                agent_id = %agent_id,
                job_id = %job_id_for_emit,
                dispatch_id,
                with_help_loop,
                "agent dispatcher: spawning invoke task"
            );

            // Spawn the invoke into its own task so the main loop
            // can continue selecting on cancel events.
            let invoker_for_task = Arc::clone(invoker);
            let bus_for_task = Arc::clone(bus);
            let current_invoke_for_task = Arc::clone(current_invoke);
            let app_for_task = app.clone();
            let workspace_id_for_task = workspace_id.to_string();
            let agent_id_for_task = agent_id.to_string();
            let job_id_for_task = job_id_for_emit.clone();

            // The help-loop branch needs a SEPARATE bus receiver so
            // the dispatcher's main-loop receiver isn't drained by
            // the invoke task. Subscribe up front (cheap; lazy
            // channel reuse) and pass it along — only used if
            // with_help_loop is true.
            let help_receiver = if with_help_loop {
                Some(bus.subscribe(workspace_id).await)
            } else {
                None
            };

            let handle = tokio::spawn(async move {
                drive_invoke(
                    app_for_task,
                    workspace_id_for_task,
                    agent_id_for_task,
                    job_id_for_task,
                    prompt_for_invoke,
                    dispatch_id,
                    with_help_loop,
                    help_receiver,
                    cancel,
                    invoker_for_task,
                    bus_for_task,
                    current_invoke_for_task,
                )
                .await;
            });
            Some(handle)
        }
        MailboxEvent::JobCancel { job_id } => {
            // Look up the in-flight slot. Race window between this
            // lock and the invoke task clearing the slot is benign
            // (documented in module docs).
            let slot = current_invoke.lock().await;
            match slot.as_ref() {
                Some(s) if s.job_id == *job_id => {
                    tracing::debug!(
                        workspace_id = %workspace_id,
                        agent_id = %agent_id,
                        job_id = %job_id,
                        "agent dispatcher: cancelling in-flight turn"
                    );
                    s.cancel.notify_one();
                }
                Some(other) => {
                    tracing::trace!(
                        workspace_id = %workspace_id,
                        agent_id = %agent_id,
                        cancel_job = %job_id,
                        in_flight_job = %other.job_id,
                        "agent dispatcher: JobCancel for a different \
                         job; ignoring"
                    );
                }
                None => {
                    tracing::trace!(
                        workspace_id = %workspace_id,
                        agent_id = %agent_id,
                        job_id = %job_id,
                        "agent dispatcher: JobCancel arrived but no \
                         turn is in flight; ignoring"
                    );
                }
            }
            None
        }
        // Other event kinds are not the dispatcher's concern.
        _ => {
            tracing::trace!(
                workspace_id = %workspace_id,
                agent_id = %agent_id,
                kind = %envelope.event.kind_str(),
                "agent dispatcher: ignoring non-dispatch event kind"
            );
            None
        }
    }
}
