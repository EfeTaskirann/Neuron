//! `SwarmJobEvent` — the per-job lifecycle event streamed to
//! `swarm:job:{job_id}:event` (WP-W3-12c).

use serde::Serialize;
use specta::Type;

use super::model::{JobOutcome, StageResult};
use super::state::JobState;
use crate::swarm::coordinator::decision::CoordinatorDecision;
use crate::swarm::coordinator::verdict::Verdict;

/// Per-job lifecycle event streamed to `swarm:job:{job_id}:event`.
///
/// One event name carries every transition in the FSM via a
/// `kind` tag (matches W3-06's `runs:{id}:span` pattern). Frontend
/// subscribers register one listener per job and switch on `kind`.
///
/// Order on the happy path:
///
///   `started → stage_started(scout) → stage_completed(scout)
///           → stage_started(plan)  → stage_completed(plan)
///           → stage_started(build) → stage_completed(build)
///           → finished`
///
/// On a stage error: `stage_started(stage) → finished` (no
/// `stage_completed` for the failing stage).
///
/// On cancellation: `… → stage_started(stage) → cancelled → finished`.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmJobEvent {
    /// Fires once at FSM start, after the workspace lock is
    /// acquired and the cancel notify is registered, before any
    /// stage spawns.
    Started {
        job_id: String,
        workspace_id: String,
        goal: String,
        created_at_ms: i64,
    },
    /// Fires before every stage's `transport.invoke` is awaited.
    /// `state` is the upcoming lifecycle stage (Scout / Plan /
    /// Build); `prompt_preview` is the first 200 *chars* of the
    /// rendered prompt (char-bounded so multi-byte Turkish text
    /// is never split mid-codepoint).
    StageStarted {
        job_id: String,
        state: JobState,
        specialist_id: String,
        prompt_preview: String,
    },
    /// Fires after a stage's `StageResult` is built and pushed to
    /// the registry, on the success path only.
    StageCompleted {
        job_id: String,
        stage: StageResult,
    },
    /// Fires once at the FSM tail, regardless of outcome
    /// (Done / Failed / Cancelled). `outcome.final_state` is one
    /// of `Done` or `Failed`; cancelled jobs ride the `Failed`
    /// path with `last_error = Some("cancelled by user")`.
    Finished {
        job_id: String,
        outcome: JobOutcome,
    },
    /// Fires when the FSM observes the cancel `Notify` mid-stage,
    /// before the job is finalized as `Failed`. The next event on
    /// this channel is always `Finished`.
    Cancelled {
        job_id: String,
        cancelled_during: JobState,
    },
    /// Fires once per Verdict-rejected retry attempt (W3-12e). The
    /// FSM emits this event AFTER incrementing `Job.retry_count`
    /// and BEFORE re-entering the Plan stage of the next attempt.
    ///
    /// Field semantics:
    ///
    /// - `attempt` is **1-indexed** so the first retry is "attempt 2"
    ///   — the UI renders this as `Attempt {attempt} of {max_retries
    ///   + 1}`.
    /// - `max_retries` is the budget cap (currently 2); included on
    ///   the wire so the UI doesn't have to import the const.
    /// - `triggered_by` is the rejecting gate (`Review` or `Test`).
    /// - `verdict` is the rejecting Verdict — same value the FSM
    ///   stamps onto `Job.last_verdict` before looping back.
    ///
    /// No `Cancelled` or `Finished` event fires on the retry
    /// transition; the job is still running, just looping back.
    /// Subsequent `StageStarted` / `StageCompleted` events on this
    /// channel belong to the new attempt.
    RetryStarted {
        job_id: String,
        attempt: u32,
        max_retries: u32,
        triggered_by: JobState,
        verdict: Verdict,
    },
    /// Fires once per job after the Classify stage's `StageCompleted`
    /// (W3-12f), carrying the parsed `CoordinatorDecision`. The next
    /// event on this channel is either a `StageStarted(Plan)` (when
    /// `route == ExecutePlan`) or a `Finished` (when `route ==
    /// ResearchOnly`, since the FSM short-circuits to Done).
    ///
    /// Optional for cache shape — the same decision rides along on
    /// the prior `StageCompleted`'s `stage.coordinator_decision`
    /// field, so frontend reducers may treat this event as a no-op
    /// (the W3-14 UI uses it to render the route pill before the
    /// next stage starts).
    DecisionMade {
        job_id: String,
        decision: CoordinatorDecision,
    },
}
