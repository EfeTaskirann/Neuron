//! The invoke + emit cycle.
//!
//! [`drive_invoke`] runs one invoke (plain or help-loop) for a
//! routed dispatch, clears the cancel slot, and emits the
//! `AgentResult` (success OR failure) back onto the bus.
//! [`run_invoke_with_help_loop`] implements the W5-03 help loop:
//! parse `neuron_help`, emit `AgentHelpRequest`, await the
//! matching `CoordinatorHelpOutcome`, feed it back — bounded by
//! `MAX_HELP_ROUNDS`.

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Runtime};
use tokio::sync::{broadcast, Mutex, Notify};

use crate::error::AppError;
use crate::swarm::mailbox_bus::{
    MailboxBus, MailboxEnvelope, MailboxEvent,
};
use crate::swarm::transport::InvokeResult;

use super::config::{
    dispatch_timeout, HELP_OUTCOME_TIMEOUT_SECS, MAX_HELP_ROUNDS,
};
use super::invoker::AgentInvoker;
use super::InvokeSlot;

/// Run one invoke + emit cycle. Spawned as a child of the main
/// loop so the loop can continue selecting on cancel events. The
/// `current_invoke` slot is cleared at the end so future cancels
/// don't land on a stale Notify.
///
/// When `with_help_loop` is true and the specialist's
/// `assistant_text` carries a `neuron_help` JSON block, the
/// dispatcher emits `MailboxEvent::AgentHelpRequest` and awaits a
/// matching `MailboxEvent::CoordinatorHelpOutcome` (filter by
/// `target_agent_id == agent_id`) on `help_receiver`. The outcome's
/// `outcome_json` is parsed back into a `CoordinatorHelpOutcome`
/// and the corresponding follow-up message is fed to the
/// specialist as the next turn's user_message. Loop bounded by
/// `MAX_HELP_ROUNDS` (3); past the cap the prior assistant_text is
/// emitted as the AgentResult unchanged so the brain can decide
/// what to do next.
#[allow(clippy::too_many_arguments)]
pub(super) async fn drive_invoke<R: Runtime, I: AgentInvoker>(
    app: AppHandle<R>,
    workspace_id: String,
    agent_id: String,
    job_id: String,
    prompt: String,
    dispatch_id: i64,
    with_help_loop: bool,
    help_receiver: Option<broadcast::Receiver<MailboxEnvelope>>,
    cancel: Arc<Notify>,
    invoker: Arc<I>,
    bus: Arc<MailboxBus>,
    current_invoke: Arc<Mutex<Option<InvokeSlot>>>,
) {
    let outcome = if with_help_loop {
        // Cooperate with the help loop. The receiver is always
        // Some(_) when with_help_loop is true (set by the caller).
        let receiver = help_receiver.expect(
            "with_help_loop=true requires a help_receiver from the caller",
        );
        run_invoke_with_help_loop(
            &app,
            &workspace_id,
            &agent_id,
            &job_id,
            &prompt,
            dispatch_id,
            receiver,
            Arc::clone(&cancel),
            &invoker,
            &bus,
        )
        .await
    } else {
        // Plain invoke — single turn, no help-loop branch.
        invoker
            .invoke_turn(
                &workspace_id,
                &agent_id,
                &prompt,
                dispatch_timeout(),
                Arc::clone(&cancel),
            )
            .await
    };

    // Clear the slot BEFORE emitting the result so a concurrent
    // JobCancel doesn't land a notify_one on a Notify nobody is
    // listening on. (Benign even if it does — Notify::notify_one
    // without a waiter just sets a permit consumed by the next
    // .notified().) We compare-and-swap on the job_id so a *later*
    // dispatch (from a multi-job future) doesn't accidentally have
    // its slot cleared by an earlier task's completion.
    {
        let mut slot = current_invoke.lock().await;
        if let Some(s) = slot.as_ref() {
            if s.job_id == job_id {
                *slot = None;
            }
        }
    }

    // AgentResult ALWAYS emitted — even on invoke failure. Failures
    // land as `assistant_text: "error: <msg>"`, `total_cost_usd:
    // 0.0`, `turn_count: 0`. Keeps the projector stream uniform.
    let (assistant_text, total_cost_usd, turn_count) = match outcome {
        Ok(result) => (
            result.assistant_text,
            result.total_cost_usd,
            result.turn_count,
        ),
        Err(err) => {
            tracing::warn!(
                workspace_id = %workspace_id,
                agent_id = %agent_id,
                job_id = %job_id,
                error = %err.message(),
                "agent dispatcher: invoke failed; emitting \
                 error AgentResult"
            );
            (
                format!("error: {}", err.message()),
                0.0_f64,
                0_u32,
            )
        }
    };

    let summary = format!(
        "agent {agent_id} result for job {job_id} \
         ({turn_count} turns, ${total_cost_usd:.4})"
    );
    let from_pane = format!("agent:{agent_id}");
    let to_pane = "agent:coordinator".to_string();
    let event_emit = MailboxEvent::AgentResult {
        job_id: job_id.clone(),
        agent_id: agent_id.clone(),
        assistant_text,
        total_cost_usd,
        turn_count,
    };
    if let Err(e) = bus
        .emit_typed(
            &app,
            &workspace_id,
            &from_pane,
            &to_pane,
            &summary,
            Some(dispatch_id),
            event_emit,
        )
        .await
    {
        tracing::warn!(
            workspace_id = %workspace_id,
            agent_id = %agent_id,
            error = %e.message(),
            "agent dispatcher: failed to emit AgentResult; \
             SQL log is source of truth, projector replay can \
             still see the dispatch row"
        );
    }
}

/// Run the help-loop branch of `drive_invoke`. Returns the final
/// `InvokeResult` (success path) once the specialist replies
/// without a `neuron_help` block, OR after `MAX_HELP_ROUNDS`
/// iterations whichever lands first. Errors propagate verbatim.
#[allow(clippy::too_many_arguments)]
async fn run_invoke_with_help_loop<R: Runtime, I: AgentInvoker>(
    app: &AppHandle<R>,
    workspace_id: &str,
    agent_id: &str,
    job_id: &str,
    prompt: &str,
    dispatch_id: i64,
    mut receiver: broadcast::Receiver<MailboxEnvelope>,
    cancel: Arc<Notify>,
    invoker: &Arc<I>,
    bus: &Arc<MailboxBus>,
) -> Result<InvokeResult, AppError> {
    let mut current_user_message = prompt.to_string();
    for round in 0..MAX_HELP_ROUNDS {
        let result = invoker
            .invoke_turn(
                workspace_id,
                agent_id,
                &current_user_message,
                dispatch_timeout(),
                Arc::clone(&cancel),
            )
            .await?;

        // Did the specialist emit a neuron_help block?
        let help = match crate::swarm::help_request::parse_help_request(
            &result.assistant_text,
        ) {
            Some(h) => h,
            None => return Ok(result),
        };

        tracing::debug!(
            workspace_id = %workspace_id,
            agent_id = %agent_id,
            job_id = %job_id,
            round,
            reason = %help.reason,
            "agent dispatcher: help-loop hit; emitting AgentHelpRequest"
        );

        // Emit AgentHelpRequest. parent_id chains to the dispatch
        // row's autoincrement id so the projector's reply-to
        // chain stays uniform.
        let from_pane = format!("agent:{agent_id}");
        let to_pane = "agent:coordinator".to_string();
        let summary = format!(
            "help request from {agent_id}: {} ({})",
            help.question, help.reason
        );
        let env = bus
            .emit_typed(
                app,
                workspace_id,
                &from_pane,
                &to_pane,
                &summary,
                Some(dispatch_id),
                MailboxEvent::AgentHelpRequest {
                    job_id: job_id.to_string(),
                    agent_id: agent_id.to_string(),
                    reason: help.reason.clone(),
                    question: help.question.clone(),
                },
            )
            .await?;
        let _help_request_id = env.id;

        // Wait for the matching CoordinatorHelpOutcome.
        let outcome = match wait_for_help_outcome(
            &mut receiver,
            agent_id,
            job_id,
            Arc::clone(&cancel),
        )
        .await
        {
            HelpOutcomeResult::Outcome(s) => s,
            HelpOutcomeResult::Cancelled => {
                return Err(AppError::Cancelled(
                    "help-loop cancelled by user".into(),
                ));
            }
            HelpOutcomeResult::Timeout => {
                tracing::warn!(
                    workspace_id = %workspace_id,
                    agent_id = %agent_id,
                    job_id = %job_id,
                    "agent dispatcher: help-outcome timeout; \
                     surfacing prior assistant_text as AgentResult"
                );
                return Ok(result);
            }
            HelpOutcomeResult::Closed => {
                return Err(AppError::Internal(
                    "mailbox channel closed while awaiting \
                     CoordinatorHelpOutcome"
                        .into(),
                ));
            }
        };

        // Translate the outcome into the next turn's user message.
        // Mirrors agent_registry.rs::acquire_and_invoke_turn_with_help
        // semantics so the W4-05 substrate behaves identically across
        // both paths.
        use crate::swarm::help_request::CoordinatorHelpOutcome;
        let parsed: CoordinatorHelpOutcome =
            serde_json::from_str(&outcome).map_err(|e| {
                AppError::SwarmInvoke(format!(
                    "CoordinatorHelpOutcome JSON parse error: {e}"
                ))
            })?;
        current_user_message = match parsed {
            CoordinatorHelpOutcome::DirectAnswer { answer } => {
                format!(
                    "Coordinator says: {answer}\n\n\
                     Now resume your task with this answer in context."
                )
            }
            CoordinatorHelpOutcome::AskBack {
                followup_question,
            } => {
                format!(
                    "Coordinator asks for more info: {followup_question}\n\n\
                     Reply with the requested detail (or, if you can't,\n\
                     emit another `neuron_help` block with a refined question)."
                )
            }
            CoordinatorHelpOutcome::Escalate { user_question } => {
                return Err(AppError::SwarmInvoke(format!(
                    "escalated to user: {user_question}"
                )));
            }
        };
    }

    // Cap reached — do one more invoke so the specialist sees the
    // final coordinator answer (avoids dropping the last round's
    // help response on the floor). If it STILL emits a help block,
    // we surface its raw assistant_text including the unanswered
    // help block so the brain sees the cap-exceeded outcome.
    let final_result = invoker
        .invoke_turn(
            workspace_id,
            agent_id,
            &current_user_message,
            dispatch_timeout(),
            cancel,
        )
        .await?;
    Ok(final_result)
}

/// Wait for a `CoordinatorHelpOutcome` envelope whose
/// `target_agent_id` matches `agent_id` AND `job_id` matches.
enum HelpOutcomeResult {
    Outcome(String),
    Cancelled,
    Timeout,
    Closed,
}

async fn wait_for_help_outcome(
    receiver: &mut broadcast::Receiver<MailboxEnvelope>,
    agent_id: &str,
    job_id: &str,
    cancel: Arc<Notify>,
) -> HelpOutcomeResult {
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(HELP_OUTCOME_TIMEOUT_SECS);
    loop {
        tokio::select! {
            biased;
            _ = cancel.notified() => return HelpOutcomeResult::Cancelled,
            _ = tokio::time::sleep_until(deadline) => return HelpOutcomeResult::Timeout,
            recv_result = receiver.recv() => {
                match recv_result {
                    Ok(envelope) => {
                        if let MailboxEvent::CoordinatorHelpOutcome {
                            job_id: ev_job_id,
                            target_agent_id,
                            outcome_json,
                        } = &envelope.event
                        {
                            if target_agent_id == agent_id && ev_job_id == job_id {
                                return HelpOutcomeResult::Outcome(
                                    outcome_json.clone(),
                                );
                            }
                        }
                        // Other envelopes — keep listening.
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Continue — broadcast auto-resyncs after lag.
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return HelpOutcomeResult::Closed;
                    }
                }
            }
        }
    }
}
