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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use tauri::{AppHandle, Runtime};
use tokio::sync::{broadcast, Notify};

use crate::error::AppError;
use crate::swarm::agent_registry::SwarmAgentRegistry;
use crate::swarm::mailbox_bus::{
    MailboxBus, MailboxEnvelope, MailboxEvent,
};
use crate::swarm::transport::InvokeResult;

/// Default cap on `Dispatch` actions per job. Past this many
/// dispatches the brain bails with `JobFinished {outcome:"failed",
/// summary:"exceeded max dispatches"}`. 30 is generous: the
/// FSM's worst-case ExecutePlan + 2 retries chain reaches ~9 stages
/// (Scout / Classify / Plan / Build / Review / Test, plus 2× retry
/// rounds of Plan-Build-Review-Test), and the brain has additional
/// degrees of freedom (parallel build dispatches, reviewer rounds)
/// so 30 leaves headroom without making a runaway loop unbounded.
pub const DEFAULT_MAX_DISPATCHES: u32 = 30;

/// Env override for [`DEFAULT_MAX_DISPATCHES`]. Same reading rules
/// as the existing `NEURON_SWARM_AGENT_TURN_CAP` knob: numeric > 0
/// wins; non-numeric / zero falls back to default with a warn log.
const MAX_DISPATCHES_ENV: &str = "NEURON_BRAIN_MAX_DISPATCHES";

/// Per-turn timeout passed through to
/// [`CoordinatorInvoker::invoke_coordinator_turn`]. 60s mirrors the
/// FSM's `SWARM_STAGE_TIMEOUT_DEFAULT` and absorbs Windows AV
/// cold-start cost on the first turn.
const BRAIN_TURN_TIMEOUT_SECS: u64 = 60;

/// Maximum bytes of `assistant_text` scanned for a brain-action
/// JSON block. Defends against an adversarial reply mostly composed
/// of garbage with a tiny JSON block hidden in the middle. 16 KiB
/// matches the W4-05 `HELP_REQUEST_SCAN_CAP`.
const BRAIN_ACTION_SCAN_CAP: usize = 16 * 1024;

// ---------------------------------------------------------------------
// BrainAction — discriminated-union of every action the persona may emit
// ---------------------------------------------------------------------

/// One Coordinator-emitted action. Tagged on `action`; field names
/// stay snake_case (matching the W5-01 `MailboxEvent` precedent).
///
/// `body_json` is `String`-typed (not `serde_json::Value`) because
/// `Value` does not implement `specta::Type`. The string carries
/// the serialised JSON payload of a `CoordinatorHelpOutcome`; the
/// W5-02 dispatcher (with_help_loop branch) parses it back via
/// `serde_json::from_str` before feeding to the specialist.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BrainAction {
    /// Route a sub-task to a specialist. `target` is `agent:<id>`
    /// per the W5-01 namespacing convention (NOT `<id>` alone —
    /// the dispatcher's `parse_agent_target` strips the prefix).
    /// `with_help_loop` defaults to `false` — opt-in per dispatch.
    Dispatch {
        target: String,
        prompt: String,
        #[serde(default)]
        with_help_loop: bool,
    },
    /// Terminate the job. `outcome` is `"done" | "failed"`; any
    /// other string is normalised to `"failed"` by the brain
    /// before emitting `JobFinished` (matching the W3-12d
    /// "outcome must be one of {done, failed}" hygiene rule).
    Finish {
        outcome: String,
        summary: String,
    },
    /// Surface a question to the user. The orchestrator chat panel
    /// (W5-04+) listens for `JobFinished { outcome: "ask_user" }`
    /// and renders the question; for W5-03 the brain emits
    /// `JobFinished` with `summary` carrying the question text.
    AskUser {
        question: String,
    },
    /// Resolve a specialist's `AgentHelpRequest`. `target` is
    /// `agent:<id>` of the specialist being answered; `body_json`
    /// is a serialised
    /// `swarm::help_request::CoordinatorHelpOutcome`. The brain
    /// emits `MailboxEvent::CoordinatorHelpOutcome` and continues
    /// the dispatch loop — `HelpOutcome` does NOT count toward
    /// the `max_dispatches` cap.
    HelpOutcome {
        target: String,
        body_json: String,
    },
}

// ---------------------------------------------------------------------
// parse_brain_action — defense-in-depth 4-step parser
// ---------------------------------------------------------------------

/// Parse a [`BrainAction`] from the Coordinator's `assistant_text`.
/// Returns `Err(AppError::SwarmInvoke)` when no valid action JSON
/// is present — unlike `parse_help_request` which returns `None`,
/// the brain MUST decide on every turn so missing JSON is a hard
/// error.
///
/// 4-step strategy (matches W3-12d Verdict / W3-12f Decision /
/// W4-05 HelpRequest):
///   1. Whole-text JSON
///   2. ```json (or ```) fence strip
///   3. First balanced `{...}` substring
///   4. Bail with structured error
pub fn parse_brain_action(
    assistant_text: &str,
) -> Result<BrainAction, AppError> {
    let truncated = if assistant_text.len() > BRAIN_ACTION_SCAN_CAP {
        &assistant_text[..BRAIN_ACTION_SCAN_CAP]
    } else {
        assistant_text
    };

    // 1. Whole-text JSON.
    if let Some(action) = try_parse_brain_action(truncated.trim()) {
        return Ok(action);
    }
    // 2. ```json fence strip.
    if let Some(fenced) = strip_fence(truncated) {
        if let Some(action) = try_parse_brain_action(fenced.trim()) {
            return Ok(action);
        }
    }
    // 3. First balanced {...}.
    if let Some(balanced) = first_balanced_object(truncated) {
        if let Some(action) = try_parse_brain_action(balanced) {
            return Ok(action);
        }
    }
    // 4. Bail.
    Err(AppError::SwarmInvoke(format!(
        "brain action JSON not found in coordinator reply (first 200 chars: {})",
        truncated.chars().take(200).collect::<String>()
    )))
}

/// Helper: inner parse of a candidate JSON fragment as a
/// `BrainAction`. Returns `None` on parse failure so the caller
/// can fall through to the next strategy.
fn try_parse_brain_action(s: &str) -> Option<BrainAction> {
    // Pre-validate the JSON shape so we can distinguish "valid JSON
    // but unknown discriminator" (caller's bug — we surface a
    // SwarmInvoke for it on the bail path) from "non-JSON garbage"
    // (parser fall-through).
    let v: Value = serde_json::from_str(s).ok()?;
    serde_json::from_value::<BrainAction>(v).ok()
}

/// Strip the FIRST ```json ... ``` (or ```...```) fence in `s` and
/// return the inner contents. Mirrors `help_request::strip_fence`
/// — duplicated here rather than re-exported to keep the module's
/// dependencies minimal (brain doesn't need anything else from
/// help_request).
fn strip_fence(s: &str) -> Option<&str> {
    let start_idx = s.find("```")?;
    let after_open = &s[start_idx + 3..];
    let after_lang = match after_open.find('\n') {
        Some(n) => &after_open[n + 1..],
        None => after_open,
    };
    let close_idx = after_lang.find("```")?;
    Some(&after_lang[..close_idx])
}

/// Walk `s` and return the FIRST balanced `{...}` substring,
/// counting braces and accounting for string boundaries. Same
/// implementation as `help_request::first_balanced_object`,
/// duplicated for the same isolation reason as `strip_fence`.
fn first_balanced_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut start: Option<usize> = None;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if start.is_none() {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s_idx) = start {
                        return std::str::from_utf8(&bytes[s_idx..=i]).ok();
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Resolve the per-process max-dispatches cap. Same env-reading
/// pattern as `commands/swarm.rs::stage_timeout`: numeric > 0 wins;
/// non-numeric / zero falls back to default with a warn log.
pub fn resolve_max_dispatches() -> u32 {
    match std::env::var(MAX_DISPATCHES_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u32>()
        {
            Ok(0) => {
                tracing::warn!(
                    %MAX_DISPATCHES_ENV,
                    "value `0` is not a valid max-dispatches cap; \
                     falling back to default"
                );
                DEFAULT_MAX_DISPATCHES
            }
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    %MAX_DISPATCHES_ENV,
                    raw = %raw,
                    error = %e,
                    "max-dispatches override is not a non-negative \
                     integer; using default"
                );
                DEFAULT_MAX_DISPATCHES
            }
        },
        _ => DEFAULT_MAX_DISPATCHES,
    }
}

// ---------------------------------------------------------------------
// CoordinatorInvoker — test-injection seam over the registry
// ---------------------------------------------------------------------

/// Test-injection seam over `SwarmAgentRegistry::acquire_and_invoke_turn`
/// for the `coordinator` agent specifically. Production impl
/// delegates straight through; mock impls return canned action
/// sequences that drive the brain through scripted scenarios.
///
/// Same shape as [`super::AgentInvoker`]: one method, returning
/// `impl Future`, no `async-trait` dep (Charter §"no new deps").
pub trait CoordinatorInvoker: Send + Sync + 'static {
    /// Invoke one turn against the workspace's `coordinator`
    /// session. The brain calls this at the start of every loop
    /// iteration; the returned `assistant_text` is fed into
    /// [`parse_brain_action`].
    fn invoke_coordinator_turn(
        &self,
        workspace_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send;
}

/// Production impl: forwards to
/// `SwarmAgentRegistry::acquire_and_invoke_turn` with
/// `agent_id = "coordinator"`. Mirrors the W5-02
/// `SwarmAgentRegistryInvoker` shape so the construction sites in
/// `commands::swarm` can compose both with the same handle.
pub struct SwarmRegistryCoordinatorInvoker<R: Runtime> {
    app: AppHandle<R>,
    registry: Arc<SwarmAgentRegistry>,
}

impl<R: Runtime> SwarmRegistryCoordinatorInvoker<R> {
    pub fn new(
        app: AppHandle<R>,
        registry: Arc<SwarmAgentRegistry>,
    ) -> Self {
        Self { app, registry }
    }
}

impl<R: Runtime> CoordinatorInvoker for SwarmRegistryCoordinatorInvoker<R> {
    fn invoke_coordinator_turn(
        &self,
        workspace_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send {
        let registry = Arc::clone(&self.registry);
        let app = self.app.clone();
        let workspace_id = workspace_id.to_string();
        let user_message = user_message.to_string();
        async move {
            registry
                .acquire_and_invoke_turn(
                    &app,
                    &workspace_id,
                    "coordinator",
                    &user_message,
                    timeout,
                    cancel,
                )
                .await
        }
    }
}

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

/// Outcome of one `wait_for_loop_event` iteration.
enum LoopEventOutcome {
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
async fn wait_for_loop_event(
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

/// Initial prompt rendered from the user's goal. The body is
/// Turkish (matching the Coordinator persona's working language)
/// so the persona-tuned prompt stays one-language.
fn render_initial_prompt(goal: &str) -> String {
    format!(
        "GOAL: {goal}\n\n\
         Sen Coordinator brain'sin (W5-03 dispatch protocol). Bu \
         hedefi tamamlamak için adım adım dispatch kararları ver. \
         Her turn'da TAM OLARAK bir JSON action emit et:\n\n\
         - `dispatch` — bir specialist'e (scout, planner, backend-builder, \
           backend-reviewer, frontend-builder, frontend-reviewer, \
           integration-tester) sub-task gönder. Builder'lar için Plan \
           çıktısını prompt'ta paylaş; reviewer/tester'lar JSON Verdict \
           emit edecek (sen okuyup karar verirsin).\n\
         - `finish` — iş bittiğinde `outcome: \"done\"` veya \
           `outcome: \"failed\"` ile sonlandır.\n\
         - `ask_user` — son çare: kullanıcıdan açıklama gerektiğinde.\n\
         - `help_outcome` — bir specialist'in `neuron_help` block'una \
           cevap olarak (target = \"agent:<id>\", body_json = \
           CoordinatorHelpOutcome JSON).\n\n\
         OUTPUT CONTRACT — yalnızca tek bir JSON object çıkar:\n\
         ```json\n\
         {{\"action\": \"dispatch\", \"target\": \"agent:scout\", \"prompt\": \"...\", \"with_help_loop\": false}}\n\
         ```",
    )
}

/// Render the next turn's prompt from the consumed mailbox event.
fn render_next_turn(event: &MailboxEvent) -> String {
    match event {
        MailboxEvent::AgentResult {
            agent_id,
            assistant_text,
            total_cost_usd,
            turn_count,
            ..
        } => {
            format!(
                "Specialist `{agent_id}` finished a task ({turn_count} turns, \
                 ${total_cost_usd:.4}).\n\n\
                 RESULT:\n{assistant_text}\n\n\
                 Bir sonraki action'ı emit et (dispatch / finish / ask_user / help_outcome)."
            )
        }
        MailboxEvent::AgentHelpRequest {
            agent_id,
            reason,
            question,
            ..
        } => {
            format!(
                "Specialist `{agent_id}` bir blocker'a takıldı ve yardım \
                 istiyor.\n\nREASON: {reason}\nQUESTION: {question}\n\n\
                 `help_outcome` action'ı emit et — target = \"agent:{agent_id}\", \
                 body_json = serialised CoordinatorHelpOutcome \
                 (`{{\"action\":\"direct_answer\",\"answer\":\"...\"}}` veya \
                 `{{\"action\":\"ask_back\",\"followup_question\":\"...\"}}` veya \
                 `{{\"action\":\"escalate\",\"user_question\":\"...\"}}`)."
            )
        }
        MailboxEvent::JobCancel { .. } => {
            // Cancel is handled by the loop's select branch, not by
            // re-rendering. Defensive: if this somehow lands as a
            // "next turn" event, stop the brain with a cancel-shaped
            // prompt (the loop's select will catch the actual cancel
            // signal on the next iteration).
            "Job cancelled by user. Emit `finish` with outcome \"failed\".".into()
        }
        // The brain filters out other variants in
        // `envelope_is_relevant`; defensive default.
        other => {
            format!(
                "Unexpected mailbox event reached the brain loop \
                 (kind={}). Emit a `finish` action with outcome \
                 \"failed\" and a summary.",
                other.kind_str()
            )
        }
    }
}

/// Prompt rendered after a `help_outcome` is emitted. Re-asks the
/// coordinator for the next dispatch so the loop continues.
fn render_after_help_outcome() -> String {
    "help_outcome was delivered to the specialist. \
     Bir sonraki action'ı emit et — specialist'in cevabı \
     ileride bir AgentResult olarak gelecek; şimdi paralel \
     bir dispatch atabilirsin VEYA `finish` ile bitirebilirsin."
        .into()
}

/// Truncate a string for summary fields — same shape as the
/// `commands::swarm::truncate_for_summary` helper.
fn truncate_for_summary(s: &str) -> String {
    const CAP: usize = 80;
    if s.chars().count() <= CAP {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(CAP).collect();
        format!("{truncated}…")
    }
}

/// Emit `JobFinished { outcome: "failed", summary: "cancelled by user" }`
/// and return the corresponding [`BrainRunResult`]. Used by both
/// the cancel branch in the invoke and the cancel branch in the
/// wait-for-event step.
async fn finish_with_cancel<R: Runtime>(
    app: &AppHandle<R>,
    bus: &MailboxBus,
    workspace_id: &str,
    job_id: &str,
) -> Result<BrainRunResult, AppError> {
    let summary = "cancelled by user".to_string();
    bus.emit_typed(
        app,
        workspace_id,
        "agent:coordinator",
        "agent:user",
        &format!("job finished (failed): {summary}"),
        None,
        MailboxEvent::JobFinished {
            job_id: job_id.to_string(),
            outcome: "failed".to_string(),
            summary: summary.clone(),
        },
    )
    .await?;
    Ok(BrainRunResult {
        job_id: job_id.to_string(),
        outcome: "failed".to_string(),
        summary,
    })
}

/// Emit `JobFinished { outcome: "failed", summary }` and return the
/// corresponding [`BrainRunResult`]. Used for parse failures,
/// session crashes, and the max-dispatch cap.
async fn finish_with_failure<R: Runtime>(
    app: &AppHandle<R>,
    bus: &MailboxBus,
    workspace_id: &str,
    job_id: &str,
    summary: &str,
) -> Result<BrainRunResult, AppError> {
    bus.emit_typed(
        app,
        workspace_id,
        "agent:coordinator",
        "agent:user",
        &format!("job finished (failed): {summary}"),
        None,
        MailboxEvent::JobFinished {
            job_id: job_id.to_string(),
            outcome: "failed".to_string(),
            summary: summary.to_string(),
        },
    )
    .await?;
    Ok(BrainRunResult {
        job_id: job_id.to_string(),
        outcome: "failed".to_string(),
        summary: summary.to_string(),
    })
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration as StdDuration;
    use tokio::sync::Mutex;
    use tokio::time::timeout;

    // ----------------------------------------------------------------
    // Parser tests (≥ 8)
    // ----------------------------------------------------------------

    #[test]
    fn parse_dispatch_action_basic() {
        let text = r#"{"action":"dispatch","target":"agent:scout","prompt":"go","with_help_loop":true}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Dispatch {
                target,
                prompt,
                with_help_loop,
            } => {
                assert_eq!(target, "agent:scout");
                assert_eq!(prompt, "go");
                assert!(with_help_loop);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn parse_dispatch_action_with_default_help_loop() {
        // `with_help_loop` omitted — serde default = false.
        let text = r#"{"action":"dispatch","target":"agent:planner","prompt":"plan it"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Dispatch {
                with_help_loop, ..
            } => {
                assert!(!with_help_loop);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn parse_finish_action_done() {
        let text = r#"{"action":"finish","outcome":"done","summary":"all approved"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Finish { outcome, summary } => {
                assert_eq!(outcome, "done");
                assert_eq!(summary, "all approved");
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn parse_finish_action_failed() {
        let text = r#"{"action":"finish","outcome":"failed","summary":"reviewer rejected"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Finish { outcome, .. } => {
                assert_eq!(outcome, "failed");
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn parse_ask_user_action() {
        let text = r#"{"action":"ask_user","question":"OAuth or API key?"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::AskUser { question } => {
                assert_eq!(question, "OAuth or API key?");
            }
            other => panic!("expected AskUser, got {other:?}"),
        }
    }

    #[test]
    fn parse_help_outcome_action_direct_answer() {
        // body_json carries a serialised CoordinatorHelpOutcome
        // (DirectAnswer variant). Must NOT be parsed back as a
        // BrainAction itself — the brain treats body_json as opaque.
        let text = r#"{"action":"help_outcome","target":"agent:scout","body_json":"{\"action\":\"direct_answer\",\"answer\":\"use OAuth\"}"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::HelpOutcome { target, body_json } => {
                assert_eq!(target, "agent:scout");
                assert!(body_json.contains("direct_answer"));
                assert!(body_json.contains("use OAuth"));
            }
            other => panic!("expected HelpOutcome, got {other:?}"),
        }
    }

    #[test]
    fn parse_help_outcome_action_ask_back() {
        let text = r#"{"action":"help_outcome","target":"agent:planner","body_json":"{\"action\":\"ask_back\",\"followup_question\":\"which file?\"}"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::HelpOutcome { body_json, .. } => {
                assert!(body_json.contains("ask_back"));
                assert!(body_json.contains("which file?"));
            }
            other => panic!("expected HelpOutcome, got {other:?}"),
        }
    }

    #[test]
    fn parse_help_outcome_action_escalate() {
        let text = r#"{"action":"help_outcome","target":"agent:backend-builder","body_json":"{\"action\":\"escalate\",\"user_question\":\"OAuth or API key?\"}"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::HelpOutcome { body_json, .. } => {
                assert!(body_json.contains("escalate"));
            }
            other => panic!("expected HelpOutcome, got {other:?}"),
        }
    }

    #[test]
    fn parse_handles_fenced_json_block() {
        let text = "Some preamble.\n\n```json\n{\"action\":\"finish\",\"outcome\":\"done\",\"summary\":\"all good\"}\n```\n\nTrailing.";
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Finish { outcome, .. } => {
                assert_eq!(outcome, "done");
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn parse_handles_first_balanced_object() {
        // Realistic LLM dump — prose around the JSON, no fence.
        let text = r#"OK. {"action":"dispatch","target":"agent:scout","prompt":"investigate"} let's go."#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Dispatch { target, .. } => {
                assert_eq!(target, "agent:scout");
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_unknown_action() {
        let text = r#"{"action":"do_a_thing","target":"agent:scout"}"#;
        let err = parse_brain_action(text)
            .expect_err("unknown action rejected");
        assert_eq!(err.kind(), "swarm_invoke");
    }

    #[test]
    fn parse_rejects_malformed_json() {
        let err = parse_brain_action("Just prose, no JSON at all.")
            .expect_err("missing JSON rejected");
        assert_eq!(err.kind(), "swarm_invoke");

        let err = parse_brain_action("not even close")
            .expect_err("garbage rejected");
        assert_eq!(err.kind(), "swarm_invoke");
    }

    #[test]
    fn resolve_max_dispatches_default() {
        let prior = std::env::var(MAX_DISPATCHES_ENV).ok();
        std::env::remove_var(MAX_DISPATCHES_ENV);
        assert_eq!(resolve_max_dispatches(), DEFAULT_MAX_DISPATCHES);
        if let Some(v) = prior {
            std::env::set_var(MAX_DISPATCHES_ENV, v);
        }
    }

    // ----------------------------------------------------------------
    // Mock invoker for run-loop tests.
    // ----------------------------------------------------------------

    /// Drives the brain through a scripted sequence of
    /// `assistant_text` replies. Each call to
    /// `invoke_coordinator_turn` pops the next reply from the
    /// front of the script.
    struct ScriptedCoordinatorInvoker {
        script: Arc<Mutex<Vec<MockReply>>>,
        calls: Arc<StdMutex<Vec<String>>>,
    }

    enum MockReply {
        AssistantText(String),
        Error(AppError),
        /// Reserved for tests that block the coordinator turn until
        /// a `cancel` signal fires — none of the W5-03 tests
        /// currently use this branch (the cancel test fires after
        /// the brain enters the wait-for-event step), but keep it
        /// available for the integration-smoke shape.
        #[allow(dead_code)]
        WaitForCancel,
    }

    impl ScriptedCoordinatorInvoker {
        fn new(replies: Vec<MockReply>) -> Self {
            Self {
                script: Arc::new(Mutex::new(replies)),
                calls: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CoordinatorInvoker for ScriptedCoordinatorInvoker {
        fn invoke_coordinator_turn(
            &self,
            _workspace_id: &str,
            user_message: &str,
            _timeout: Duration,
            cancel: Arc<Notify>,
        ) -> impl std::future::Future<
            Output = Result<InvokeResult, AppError>,
        > + Send {
            self.calls.lock().unwrap().push(user_message.to_string());
            let script = Arc::clone(&self.script);
            async move {
                let mut script = script.lock().await;
                if script.is_empty() {
                    return Err(AppError::Internal(
                        "scripted invoker exhausted".into(),
                    ));
                }
                let reply = script.remove(0);
                drop(script);
                match reply {
                    MockReply::AssistantText(text) => Ok(InvokeResult {
                        session_id: "mock-session".to_string(),
                        assistant_text: text,
                        total_cost_usd: 0.01,
                        turn_count: 1,
                    }),
                    MockReply::Error(e) => Err(e),
                    MockReply::WaitForCancel => {
                        cancel.notified().await;
                        Err(AppError::Cancelled(
                            "cancelled by test".into(),
                        ))
                    }
                }
            }
        }
    }

    // ----------------------------------------------------------------
    // Helpers for run-loop tests.
    // ----------------------------------------------------------------

    /// Helper: build a mailbox bus and the supporting glue for a
    /// brain test. Tests own the brain spawn itself so they can
    /// pin `max_dispatches` without rebuilding the harness.
    /// `_invoker` and `_max_dispatches` are accepted (and unused)
    /// for call-site readability — the same shape stays uniform
    /// across every test.
    async fn setup_brain<I: CoordinatorInvoker>(
        _invoker: Arc<I>,
        _max_dispatches: u32,
    ) -> (
        tauri::App<tauri::test::MockRuntime>,
        Arc<MailboxBus>,
        Arc<Notify>,
        String,
        String,
        tempfile::TempDir,
    ) {
        let (app, pool, dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool));
        let cancel = Arc::new(Notify::new());
        let workspace_id = "default".to_string();
        let job_id = "j-test".to_string();
        (app, bus, cancel, workspace_id, job_id, dir)
    }

    /// Wait for an envelope matching `predicate` to appear on the
    /// bus's persisted log. Bounded soft-timeout so tests fail fast
    /// rather than hanging on a stuck brain.
    async fn wait_for_envelope_log<F>(
        bus: &MailboxBus,
        kind: &str,
        predicate: F,
    ) -> Option<MailboxEnvelope>
    where
        F: Fn(&MailboxEnvelope) -> bool,
    {
        let deadline =
            std::time::Instant::now() + StdDuration::from_secs(5);
        loop {
            let rows = bus
                .list_typed(Some(kind), None, Some(500))
                .await
                .expect("list ok");
            if let Some(env) =
                rows.into_iter().find(|env| predicate(env))
            {
                return Some(env);
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    }

    // ----------------------------------------------------------------
    // Run-loop tests (≥ 12)
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn brain_emits_first_dispatch_after_job_started() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"investigate auth.rs","with_help_loop":false}"#
                    .to_string(),
            ),
            // Stop the loop after the dispatch by returning a
            // finish on the next call (the test will inject an
            // AgentResult so the brain reaches the second turn).
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"ok"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let ws_for_brain = ws.clone();
        let job_for_brain = job_id.clone();
        let invoker_for_brain = Arc::clone(&invoker);
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws_for_brain,
                job_for_brain,
                "test goal".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        // 1. JobStarted appears.
        let started = wait_for_envelope_log(&bus, "job_started", |e| {
            matches!(&e.event, MailboxEvent::JobStarted { job_id: jid, .. } if jid == &job_id)
        })
        .await
        .expect("JobStarted emitted");
        match &started.event {
            MailboxEvent::JobStarted { goal, .. } => {
                assert_eq!(goal, "test goal");
            }
            _ => panic!("unexpected"),
        }

        // 2. The first dispatch lands.
        let dispatch = wait_for_envelope_log(&bus, "task_dispatch", |e| {
            matches!(&e.event, MailboxEvent::TaskDispatch { job_id: jid, target, .. } if jid == &job_id && target == "agent:scout")
        })
        .await
        .expect("first dispatch emitted");
        match &dispatch.event {
            MailboxEvent::TaskDispatch { prompt, .. } => {
                assert_eq!(prompt, "investigate auth.rs");
            }
            _ => panic!("unexpected"),
        }

        // 3. Inject an AgentResult so the brain reaches its second turn.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "scout result",
            Some(dispatch.id),
            MailboxEvent::AgentResult {
                job_id: job_id.clone(),
                agent_id: "scout".to_string(),
                assistant_text: "I found three matches.".to_string(),
                total_cost_usd: 0.012,
                turn_count: 1,
            },
        )
        .await
        .expect("emit AgentResult");

        // 4. The brain emits Finish and returns.
        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain result");
        assert_eq!(result.outcome, "done");
        assert_eq!(result.job_id, job_id);

        // The second invoke saw the AgentResult-shaped prompt.
        let calls = invoker.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("test goal"));
        assert!(calls[1].contains("I found three matches."));
    }

    #[tokio::test]
    async fn brain_consumes_agent_result_emits_next_dispatch() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"first"}"#
                    .to_string(),
            ),
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:planner","prompt":"second based on scout"}"#
                    .to_string(),
            ),
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"all done"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id2 = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id2,
                "test goal".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        // First dispatch.
        let scout_dispatch = wait_for_envelope_log(&bus, "task_dispatch", |e| {
            matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:scout")
        })
        .await
        .expect("first dispatch");

        // Inject scout result.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "scout result",
            Some(scout_dispatch.id),
            MailboxEvent::AgentResult {
                job_id: job_id.clone(),
                agent_id: "scout".to_string(),
                assistant_text: "Scout findings here.".to_string(),
                total_cost_usd: 0.01,
                turn_count: 1,
            },
        )
        .await
        .expect("emit");

        // Second dispatch (planner).
        let planner_dispatch = wait_for_envelope_log(&bus, "task_dispatch", |e| {
            matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:planner")
        })
        .await
        .expect("planner dispatch");

        // Inject planner result.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:planner",
            "agent:coordinator",
            "planner result",
            Some(planner_dispatch.id),
            MailboxEvent::AgentResult {
                job_id: job_id.clone(),
                agent_id: "planner".to_string(),
                assistant_text: "Plan here.".to_string(),
                total_cost_usd: 0.01,
                turn_count: 1,
            },
        )
        .await
        .expect("emit");

        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "done");
    }

    #[tokio::test]
    async fn brain_emits_finish_done_terminates_loop() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"trivial goal"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            job_id.clone(),
            "trivial".to_string(),
            invoker,
            Arc::clone(&bus),
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "done");
        assert_eq!(result.summary, "trivial goal");

        // JobFinished was emitted.
        let finished = wait_for_envelope_log(&bus, "job_finished", |e| {
            matches!(&e.event, MailboxEvent::JobFinished { job_id: jid, outcome, .. } if jid == &job_id && outcome == "done")
        })
        .await
        .expect("JobFinished emitted");
        match &finished.event {
            MailboxEvent::JobFinished { outcome, .. } => {
                assert_eq!(outcome, "done");
            }
            _ => panic!("unexpected"),
        }
    }

    #[tokio::test]
    async fn brain_emits_finish_failed_terminates_loop() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"failed","summary":"reviewer rejected"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, _job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            "j-fail".to_string(),
            "trivial".to_string(),
            invoker,
            bus,
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "failed");
        assert_eq!(result.summary, "reviewer rejected");
    }

    #[tokio::test]
    async fn brain_emits_ask_user_terminates_with_ask_user_outcome() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"ask_user","question":"OAuth or API key?"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            job_id.clone(),
            "ambiguous".to_string(),
            invoker,
            Arc::clone(&bus),
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "ask_user");
        assert_eq!(result.summary, "OAuth or API key?");

        // JobFinished was emitted with outcome="ask_user".
        let finished = wait_for_envelope_log(&bus, "job_finished", |e| {
            matches!(&e.event, MailboxEvent::JobFinished { job_id: jid, outcome, .. } if jid == &job_id && outcome == "ask_user")
        })
        .await
        .expect("JobFinished ask_user emitted");
        match &finished.event {
            MailboxEvent::JobFinished { outcome, summary, .. } => {
                assert_eq!(outcome, "ask_user");
                assert_eq!(summary, "OAuth or API key?");
            }
            _ => panic!("unexpected"),
        }
    }

    #[tokio::test]
    async fn brain_consumes_help_request_emits_help_outcome() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            // Turn 1: Dispatch a task with help-loop on.
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:backend-builder","prompt":"build it","with_help_loop":true}"#
                    .to_string(),
            ),
            // Turn 2: AgentHelpRequest comes in; emit help_outcome.
            MockReply::AssistantText(
                r#"{"action":"help_outcome","target":"agent:backend-builder","body_json":"{\"action\":\"direct_answer\",\"answer\":\"User.id\"}"}"#
                    .to_string(),
            ),
            // Turn 3: After help_outcome the brain re-asks; emit finish.
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"helped through"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test goal".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        // Wait for the first dispatch.
        let dispatch = wait_for_envelope_log(&bus, "task_dispatch", |e| {
            matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:backend-builder")
        })
        .await
        .expect("first dispatch");

        // Inject AgentHelpRequest from the specialist.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:backend-builder",
            "agent:coordinator",
            "help",
            Some(dispatch.id),
            MailboxEvent::AgentHelpRequest {
                job_id: job_id.clone(),
                agent_id: "backend-builder".to_string(),
                reason: "Plan step ambiguous".to_string(),
                question: "Which struct field carries the user id?".to_string(),
            },
        )
        .await
        .expect("emit help req");

        // Wait for the brain to emit CoordinatorHelpOutcome.
        let outcome_env = wait_for_envelope_log(
            &bus,
            "coordinator_help_outcome",
            |e| {
                matches!(
                    &e.event,
                    MailboxEvent::CoordinatorHelpOutcome { target_agent_id, .. }
                    if target_agent_id == "backend-builder"
                )
            },
        )
        .await
        .expect("CoordinatorHelpOutcome emitted");
        match &outcome_env.event {
            MailboxEvent::CoordinatorHelpOutcome {
                outcome_json,
                target_agent_id,
                ..
            } => {
                assert_eq!(target_agent_id, "backend-builder");
                assert!(outcome_json.contains("direct_answer"));
                assert!(outcome_json.contains("User.id"));
            }
            _ => panic!("unexpected"),
        }

        // Brain finishes after the next finish action — no more
        // events needed because help_outcome doesn't wait for a
        // follow-up event.
        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "done");

        // Three turns total — each line in the script consumed.
        let calls = invoker.calls();
        assert_eq!(calls.len(), 3);
        // Turn 2 saw the AgentHelpRequest-shaped prompt.
        assert!(calls[1].contains("blocker"));
        assert!(calls[1].contains("Plan step ambiguous"));
    }

    #[tokio::test]
    async fn brain_max_dispatches_cap_terminates_with_failed() {
        // Script returns dispatches forever — past the cap the
        // brain bails.
        let mut script = Vec::new();
        for _ in 0..10 {
            script.push(MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"loop"}"#
                    .to_string(),
            ));
        }
        let invoker =
            Arc::new(ScriptedCoordinatorInvoker::new(script));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 2).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                2, // max_dispatches=2
            )
            .await
        });

        // Inject results so the brain reaches the next dispatch
        // round each time. We need 2 results for the 2 dispatches
        // before the cap trips on dispatch #3.
        for i in 0..2 {
            let dispatch = wait_for_envelope_log(
                &bus,
                "task_dispatch",
                |e| {
                    if let MailboxEvent::TaskDispatch { .. } = &e.event {
                        // Match by id ordering — the i'th dispatch.
                        true
                    } else {
                        false
                    }
                },
            )
            .await
            .expect("dispatch emitted");

            // Wait until we have at least i+1 dispatches.
            loop {
                let rows = bus
                    .list_typed(Some("task_dispatch"), None, None)
                    .await
                    .expect("list");
                if rows.len() >= i + 1 {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(20)).await;
            }
            let _ = dispatch;

            // Pull all dispatches and use the i'th one.
            let dispatches = bus
                .list_typed(Some("task_dispatch"), None, None)
                .await
                .expect("list");
            let parent = dispatches[i].id;

            bus.emit_typed(
                app.handle(),
                "default",
                "agent:scout",
                "agent:coordinator",
                "result",
                Some(parent),
                MailboxEvent::AgentResult {
                    job_id: job_id.clone(),
                    agent_id: "scout".to_string(),
                    assistant_text: "result".to_string(),
                    total_cost_usd: 0.01,
                    turn_count: 1,
                },
            )
            .await
            .expect("emit");
        }

        let result = timeout(StdDuration::from_secs(10), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "failed");
        assert!(
            result.summary.contains("max dispatches"),
            "summary: {}",
            result.summary
        );
    }

    #[tokio::test]
    async fn brain_cancel_mid_loop_terminates_with_failed() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            // First turn: emit a dispatch so the brain enters the
            // wait-for-event branch.
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"long"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        // Wait for the first dispatch (so the brain is in
        // wait-for-event).
        wait_for_envelope_log(&bus, "task_dispatch", |_| true)
            .await
            .expect("dispatch emitted");

        // Signal cancel.
        cancel.notify_waiters();

        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "failed");
        assert_eq!(result.summary, "cancelled by user");
    }

    #[tokio::test]
    async fn brain_handles_coordinator_session_crash() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::Error(AppError::SwarmInvoke(
                "subprocess died".into(),
            )),
        ]));
        let (app, bus, cancel, ws, _job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            "j-crash".to_string(),
            "test".to_string(),
            invoker,
            bus,
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "failed");
        assert!(
            result.summary.contains("subprocess died"),
            "summary: {}",
            result.summary
        );
    }

    #[tokio::test]
    async fn brain_resumes_loop_after_help_outcome() {
        // help_outcome must NOT count as a dispatch — verify by
        // running with cap=1 and threading through one help_outcome
        // and one dispatch + finish.
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            // Turn 1: dispatch (counts as 1)
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"go","with_help_loop":true}"#
                    .to_string(),
            ),
            // Turn 2: help_outcome (does NOT count)
            MockReply::AssistantText(
                r#"{"action":"help_outcome","target":"agent:scout","body_json":"{\"action\":\"direct_answer\",\"answer\":\"x\"}"}"#
                    .to_string(),
            ),
            // Turn 3: finish — cap=1 still respected because
            // help_outcome was not counted.
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"ok"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 1).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                1,
            )
            .await
        });

        // Wait for the first dispatch and inject AgentHelpRequest.
        let dispatch = wait_for_envelope_log(&bus, "task_dispatch", |_| true)
            .await
            .expect("dispatch emitted");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "help",
            Some(dispatch.id),
            MailboxEvent::AgentHelpRequest {
                job_id: job_id.clone(),
                agent_id: "scout".to_string(),
                reason: "blocked".to_string(),
                question: "?".to_string(),
            },
        )
        .await
        .expect("emit help");

        // Brain emits help_outcome, then re-invokes (no event needed)
        // and emits finish.
        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "done");

        // Verify the cap was NOT tripped: 1 dispatch only.
        let dispatches = bus
            .list_typed(Some("task_dispatch"), None, None)
            .await
            .expect("list");
        assert_eq!(dispatches.len(), 1);
        // help_outcome was emitted.
        let help_outcomes = bus
            .list_typed(Some("coordinator_help_outcome"), None, None)
            .await
            .expect("list");
        assert_eq!(help_outcomes.len(), 1);
    }

    #[tokio::test]
    async fn brain_emits_dispatch_with_correct_parent_id_chain() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"first"}"#
                    .to_string(),
            ),
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:planner","prompt":"second"}"#
                    .to_string(),
            ),
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"ok"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        let scout_dispatch =
            wait_for_envelope_log(&bus, "task_dispatch", |e| {
                matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:scout")
            })
            .await
            .expect("scout dispatch");

        // Inject scout result.
        let scout_result_env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:scout",
                "agent:coordinator",
                "result",
                Some(scout_dispatch.id),
                MailboxEvent::AgentResult {
                    job_id: job_id.clone(),
                    agent_id: "scout".to_string(),
                    assistant_text: "found".to_string(),
                    total_cost_usd: 0.01,
                    turn_count: 1,
                },
            )
            .await
            .expect("emit");

        // The next dispatch should chain its parent_id to the
        // scout-result envelope's id.
        let planner_dispatch =
            wait_for_envelope_log(&bus, "task_dispatch", |e| {
                matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:planner")
            })
            .await
            .expect("planner dispatch");
        assert_eq!(planner_dispatch.parent_id, Some(scout_result_env.id));

        // Inject planner result so brain finishes.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:planner",
            "agent:coordinator",
            "result",
            Some(planner_dispatch.id),
            MailboxEvent::AgentResult {
                job_id: job_id.clone(),
                agent_id: "planner".to_string(),
                assistant_text: "plan".to_string(),
                total_cost_usd: 0.01,
                turn_count: 1,
            },
        )
        .await
        .expect("emit");

        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "done");
    }

    #[tokio::test]
    async fn brain_finish_outcome_other_than_done_or_failed_normalised_to_failed() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"weird","summary":"strange"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, _job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            "j-norm".to_string(),
            "test".to_string(),
            invoker,
            Arc::clone(&bus),
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "failed", "outcome normalised to failed");
        assert_eq!(result.summary, "strange");

        // The emitted JobFinished also reflects the normalised outcome.
        let finished = wait_for_envelope_log(&bus, "job_finished", |_| true)
            .await
            .expect("JobFinished");
        match &finished.event {
            MailboxEvent::JobFinished { outcome, .. } => {
                assert_eq!(outcome, "failed");
            }
            _ => panic!("unexpected"),
        }
    }

    #[tokio::test]
    async fn brain_handles_parse_error_as_failed() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText("Just garbage no JSON.".to_string()),
        ]));
        let (app, bus, cancel, ws, _job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            "j-parse".to_string(),
            "test".to_string(),
            invoker,
            bus,
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "failed");
        assert!(
            result.summary.contains("parse error"),
            "summary: {}",
            result.summary
        );
    }
}
