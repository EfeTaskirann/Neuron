//! `CoordinatorBrain` — mailbox-driven dispatch loop (WP-W5-03).
//!
//! Replaces the FSM's deterministic stage iteration (Scout → Classify
//! → Plan → Build×N → Review×N → Test) with a Coordinator-driven
//! dispatch loop:
//!
//! 1. The brain renders an initial prompt from the job's goal and
//!    feeds it to the Coordinator persona session.
//! 2. The Coordinator emits one structured JSON [`BrainAction`] per
//!    turn: `dispatch` (route a sub-task to a specialist),
//!    `finish` (terminate the job), `ask_user` (escalate),
//!    `help_outcome` (resolve a specialist's blocker).
//! 3. For `Dispatch`, the brain emits `MailboxEvent::TaskDispatch`
//!    onto the W5-01 bus. The W5-02 [`MailboxAgentDispatcher`]s pick
//!    up the event, invoke the specialist, and emit
//!    `MailboxEvent::AgentResult` back. For `HelpOutcome`, the brain
//!    emits `MailboxEvent::CoordinatorHelpOutcome` so the originating
//!    specialist's dispatcher (W5-02 + with_help_loop branch) can
//!    feed the answer back to the specialist's session as the next
//!    turn's user message.
//! 4. The brain awaits the next mailbox envelope (`AgentResult`,
//!    `AgentHelpRequest`, or `JobCancel`), renders the next turn's
//!    prompt from it, and loops back to step 2.
//!
//! Termination guards:
//! - `max_dispatches` (default [`DEFAULT_MAX_DISPATCHES`] = 30, env
//!   override `NEURON_BRAIN_MAX_DISPATCHES`) — counts only `Dispatch`
//!   actions, NOT `HelpOutcome`. Past the cap the brain emits
//!   `JobFinished { outcome: "failed", summary: "exceeded max
//!   dispatches" }` and exits.
//! - `JobCancel` — emits `JobFinished { outcome: "failed", summary:
//!   "cancelled by user" }` and exits.
//! - Coordinator session crash — emits `JobFinished { outcome:
//!   "failed", summary: "<error>" }` and exits.
//! - Unknown / malformed action JSON — same as session crash:
//!   `JobFinished { outcome: "failed", summary: "<parse error>" }`.
//!
//! ## Module layout
//!
//! Split from the original monolithic `brain.rs` (WP-W5-03) into a
//! package; behaviour is byte-for-byte preserved:
//!
//! - [`action`] — [`BrainAction`] union + the 4-step
//!   [`parse_brain_action`] parser + [`resolve_max_dispatches`].
//! - [`invoker`] — [`CoordinatorInvoker`] test seam +
//!   [`SwarmRegistryCoordinatorInvoker`] production impl.
//! - [`events`] — the mailbox `recv`-vs-`cancel` select loop.
//! - [`prompt`] — per-turn prompt rendering + summary truncation.
//! - [`finish`] — terminal `JobFinished` emitters.
//! - `mod` (this file) — [`CoordinatorBrain`] dispatch loop +
//!   [`BrainRunResult`].
//!
//! ## Mocking the registry
//!
//! Tests that exercise the brain run loop without spawning real
//! `claude` subprocesses use the [`CoordinatorInvoker`] trait. The
//! production impl forwards to
//! `SwarmAgentRegistry::acquire_and_invoke_turn` with `agent_id =
//! "coordinator"`; mock impls return canned `InvokeResult`s that
//! drive the brain through scripted action sequences. Same shape as
//! the W5-02 [`super::AgentInvoker`] trait — one method, returning
//! `impl Future`, no `async-trait` dep (Charter §"no new deps").
//!
//! ## Out of scope (per WP §"Out of scope")
//!
//! - FSM teardown (W5-06)
//! - Job state derivation from mailbox / UI plumbing (W5-04 — for
//!   now build a stub [`JobOutcome`] inline from emitted events)
//! - Cancel + workspace lock migration (W5-05)
//! - Reviewer/Tester help-via-Verdict
//! - Multi-job concurrency
//! - Brain memory beyond persistent session

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Runtime};
use tokio::sync::Notify;

use crate::error::AppError;
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEvent};

mod action;
mod events;
mod finish;
mod invoker;
mod prompt;

#[cfg(test)]
mod tests;

pub use action::{
    parse_brain_action, resolve_max_dispatches, BrainAction,
    DEFAULT_MAX_DISPATCHES,
};
pub use invoker::{CoordinatorInvoker, SwarmRegistryCoordinatorInvoker};

use events::{wait_for_loop_event, LoopEventOutcome};
use finish::{finish_with_cancel, finish_with_failure};
use prompt::{
    render_after_help_outcome, render_initial_prompt, render_next_turn,
    truncate_for_summary,
};

/// Per-turn timeout passed through to
/// [`CoordinatorInvoker::invoke_coordinator_turn`]. 60s mirrors the
/// FSM's `SWARM_STAGE_TIMEOUT_DEFAULT` and absorbs Windows AV
/// cold-start cost on the first turn.
const BRAIN_TURN_TIMEOUT_SECS: u64 = 60;

// ---------------------------------------------------------------------
// CoordinatorBrain — the dispatch loop
// ---------------------------------------------------------------------

/// Result the brain returns to its caller (`swarm:run_job_v2` IPC).
/// Carries the `outcome` string from the terminating
/// `JobFinished` event so the IPC can build a stub `JobOutcome`
/// without re-querying the bus.
#[derive(Debug, Clone, PartialEq)]
pub struct BrainRunResult {
    pub job_id: String,
    /// `"done" | "failed" | "ask_user"`. The IPC maps this to a
    /// `JobState` (Done for `done`, Failed for everything else).
    pub outcome: String,
    pub summary: String,
}

/// The dispatch loop. Drives the Coordinator session until a
/// terminating action (`Finish` / `AskUser`) lands or a guard
/// trips (max-dispatch cap, JobCancel, session crash).
///
/// `cancel` is the per-job cancel `Notify` — also carried into
/// every `invoke_coordinator_turn` call so a user-driven cancel
/// truncates the in-flight turn promptly.
pub struct CoordinatorBrain;

impl CoordinatorBrain {
    /// Run the brain to completion. See module docs for the loop
    /// semantics and termination guards.
    ///
    /// `app`, `workspace_id`, `job_id`, `goal` are forwarded to
    /// every emitted [`MailboxEvent`] (the W5-04 projector reads
    /// `job_id` to thread events back to the right job).
    /// `invoker` is the test-seam over the Coordinator session;
    /// `bus` is the W5-01 mailbox event bus; `cancel` is the
    /// per-job cancel notify.
    pub async fn run<R, I>(
        app: AppHandle<R>,
        workspace_id: String,
        job_id: String,
        goal: String,
        invoker: Arc<I>,
        bus: Arc<MailboxBus>,
        cancel: Arc<Notify>,
    ) -> Result<BrainRunResult, AppError>
    where
        R: Runtime,
        I: CoordinatorInvoker,
    {
        let max_dispatches = resolve_max_dispatches();
        Self::run_with_max(
            app,
            workspace_id,
            job_id,
            goal,
            invoker,
            bus,
            cancel,
            max_dispatches,
        )
        .await
    }

    /// Same as [`Self::run`] but takes an explicit `max_dispatches`
    /// cap — used by tests to pin the cap without depending on the
    /// env var.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_max<R, I>(
        app: AppHandle<R>,
        workspace_id: String,
        job_id: String,
        goal: String,
        invoker: Arc<I>,
        bus: Arc<MailboxBus>,
        cancel: Arc<Notify>,
        max_dispatches: u32,
    ) -> Result<BrainRunResult, AppError>
    where
        R: Runtime,
        I: CoordinatorInvoker,
    {
        // Subscribe to the bus BEFORE emitting JobStarted so we
        // never miss a result (the dispatcher could in theory be
        // fast enough to emit AgentResult before our subscribe
        // call returns; the bus's per-workspace channel is
        // lazy-created so we want our receiver registered first).
        let mut receiver = bus.subscribe(&workspace_id).await;

        // Emit JobStarted as the bookend event. The brain's loop
        // will consume the receiver only for AgentResult /
        // AgentHelpRequest / JobCancel events — JobStarted is for
        // downstream subscribers (W5-04 projector). Our own
        // receiver may pick it up too; we filter by job_id and
        // ignore it explicitly in `wait_for_loop_event`.
        bus.emit_typed(
            &app,
            &workspace_id,
            "agent:user",
            "agent:coordinator",
            &format!("job started: {}", truncate_for_summary(&goal)),
            None,
            MailboxEvent::JobStarted {
                job_id: job_id.clone(),
                workspace_id: workspace_id.clone(),
                goal: goal.clone(),
            },
        )
        .await?;

        // Initial prompt — the goal verbatim. Subsequent turns
        // render based on the consumed mailbox event (see
        // `render_next_turn`).
        let mut next_prompt = render_initial_prompt(&goal);
        let mut dispatch_count: u32 = 0;
        // Tracks the most recent dispatch row's id for parent_id
        // chaining. The brain wires each dispatch's id back to its
        // ensuing AgentResult (via the bus's parent_id field) but
        // also chains AgentResult -> next Dispatch so the projector
        // sees a uniform reply-to chain across rounds.
        let mut last_envelope_id: Option<i64> = None;

        loop {
            // --- 1. Invoke the coordinator session ---
            let invoke_outcome = invoker
                .invoke_coordinator_turn(
                    &workspace_id,
                    &next_prompt,
                    Duration::from_secs(BRAIN_TURN_TIMEOUT_SECS),
                    Arc::clone(&cancel),
                )
                .await;

            let assistant_text = match invoke_outcome {
                Ok(r) => r.assistant_text,
                Err(AppError::Cancelled(_)) => {
                    return finish_with_cancel(
                        &app,
                        &bus,
                        &workspace_id,
                        &job_id,
                    )
                    .await;
                }
                Err(other) => {
                    let summary =
                        format!("coordinator session error: {}", other.message());
                    return finish_with_failure(
                        &app,
                        &bus,
                        &workspace_id,
                        &job_id,
                        &summary,
                    )
                    .await;
                }
            };

            // --- 2. Parse the action. Parse failures terminate as failed. ---
            let action = match parse_brain_action(&assistant_text) {
                Ok(a) => a,
                Err(e) => {
                    let summary =
                        format!("brain action parse error: {}", e.message());
                    return finish_with_failure(
                        &app,
                        &bus,
                        &workspace_id,
                        &job_id,
                        &summary,
                    )
                    .await;
                }
            };

            // --- 3. Dispatch on the variant ---
            match action {
                BrainAction::Dispatch {
                    target,
                    prompt,
                    with_help_loop,
                } => {
                    dispatch_count += 1;
                    if dispatch_count > max_dispatches {
                        return finish_with_failure(
                            &app,
                            &bus,
                            &workspace_id,
                            &job_id,
                            "exceeded max dispatches",
                        )
                        .await;
                    }

                    let summary = format!(
                        "dispatch #{dispatch_count} {target}: {}",
                        truncate_for_summary(&prompt)
                    );
                    // Dispatch's parent_id chains to the most
                    // recent envelope (initial: None; later:
                    // AgentResult / CoordinatorHelpOutcome). The
                    // wait-for-event branch below will overwrite
                    // `last_envelope_id` with the consumed
                    // envelope's id once the agent replies.
                    let _env = bus
                        .emit_typed(
                            &app,
                            &workspace_id,
                            "agent:coordinator",
                            &target,
                            &summary,
                            last_envelope_id,
                            MailboxEvent::TaskDispatch {
                                job_id: job_id.clone(),
                                target: target.clone(),
                                prompt: prompt.clone(),
                                with_help_loop,
                            },
                        )
                        .await?;
                }
                BrainAction::Finish { outcome, summary } => {
                    // Normalise "outcome" — only "done" stays as
                    // "done"; everything else becomes "failed".
                    let normalised = if outcome == "done" {
                        "done".to_string()
                    } else {
                        "failed".to_string()
                    };
                    bus.emit_typed(
                        &app,
                        &workspace_id,
                        "agent:coordinator",
                        "agent:user",
                        &format!("job finished ({normalised}): {}", truncate_for_summary(&summary)),
                        last_envelope_id,
                        MailboxEvent::JobFinished {
                            job_id: job_id.clone(),
                            outcome: normalised.clone(),
                            summary: summary.clone(),
                        },
                    )
                    .await?;
                    return Ok(BrainRunResult {
                        job_id,
                        outcome: normalised,
                        summary,
                    });
                }
                BrainAction::AskUser { question } => {
                    bus.emit_typed(
                        &app,
                        &workspace_id,
                        "agent:coordinator",
                        "agent:user",
                        &format!(
                            "job ask_user: {}",
                            truncate_for_summary(&question)
                        ),
                        last_envelope_id,
                        MailboxEvent::JobFinished {
                            job_id: job_id.clone(),
                            outcome: "ask_user".to_string(),
                            summary: question.clone(),
                        },
                    )
                    .await?;
                    return Ok(BrainRunResult {
                        job_id,
                        outcome: "ask_user".to_string(),
                        summary: question,
                    });
                }
                BrainAction::HelpOutcome { target, body_json } => {
                    // Strip the "agent:" prefix to match the
                    // dispatcher's parse_agent_target convention —
                    // the CoordinatorHelpOutcome event's
                    // target_agent_id is the BARE id (so the
                    // dispatcher can compare it with its own
                    // agent_id directly).
                    let target_agent_id = target
                        .strip_prefix("agent:")
                        .unwrap_or(&target)
                        .to_string();
                    let summary = format!(
                        "help_outcome -> {target_agent_id}: {}",
                        truncate_for_summary(&body_json)
                    );
                    let env = bus
                        .emit_typed(
                            &app,
                            &workspace_id,
                            "agent:coordinator",
                            &target,
                            &summary,
                            last_envelope_id,
                            MailboxEvent::CoordinatorHelpOutcome {
                                job_id: job_id.clone(),
                                target_agent_id,
                                outcome_json: body_json,
                            },
                        )
                        .await?;
                    last_envelope_id = Some(env.id);
                    // HelpOutcome does NOT count toward the
                    // max-dispatch cap (per WP §"Termination
                    // guards"). Re-invoke the coordinator
                    // immediately with a "your help_outcome was
                    // delivered" prompt so it can decide on the
                    // next dispatch.
                    next_prompt = render_after_help_outcome();
                    continue;
                }
            }

            // --- 4. Wait for the next mailbox event (only on Dispatch) ---
            // Finish/AskUser already returned; HelpOutcome `continue`d.
            let envelope = match wait_for_loop_event(
                &mut receiver,
                &job_id,
                &cancel,
            )
            .await
            {
                LoopEventOutcome::Event(env) => env,
                LoopEventOutcome::Cancelled => {
                    return finish_with_cancel(
                        &app,
                        &bus,
                        &workspace_id,
                        &job_id,
                    )
                    .await;
                }
                LoopEventOutcome::Closed => {
                    return finish_with_failure(
                        &app,
                        &bus,
                        &workspace_id,
                        &job_id,
                        "mailbox channel closed unexpectedly",
                    )
                    .await;
                }
            };

            last_envelope_id = Some(envelope.id);
            next_prompt = render_next_turn(&envelope.event);
        }
    }
}
