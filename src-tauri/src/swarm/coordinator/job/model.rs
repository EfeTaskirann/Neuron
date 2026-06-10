//! Job domain-model wire types (WP-W3-12a §2 / WP-W3-12b §4).
//!
//! `Job`, `StageResult`, and `JobOutcome` cross the IPC boundary as
//! the FSM's contract with the frontend; `JobSummary` / `JobDetail`
//! are the slim / full history wire-shapes the `swarm:list_jobs` /
//! `swarm:get_job` IPCs return. All pure data — the stateful
//! [`super::registry::JobRegistry`] owns them, and
//! [`super::event::SwarmJobEvent`] streams them.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::state::JobState;
use crate::swarm::coordinator::decision::CoordinatorDecision;
use crate::swarm::coordinator::verdict::Verdict;

/// Output of one completed dispatch / stage. Append-only — the
/// W5-04 projector pushes one entry per `MailboxEvent::AgentResult`
/// the brain consumed (success path), and the brain decides
/// whether to retry, escalate, or finish on a result the persona
/// classifies as failure. Pre-W5-06 this was populated by the
/// FSM's per-stage `transport.invoke` await; the data shape
/// stays the same so the frontend's `JobOutcome` reducer keeps
/// working.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StageResult {
    /// Which lifecycle stage produced this result. One of
    /// `Scout` / `Plan` / `Build` in 12a.
    pub state: JobState,
    /// `Profile.id` of the specialist that ran the stage —
    /// `"scout"` / `"planner"` / `"backend-builder"`.
    pub specialist_id: String,
    /// The specialist's final assistant text (the `result` event's
    /// `result` field; running deltas already concatenated).
    pub assistant_text: String,
    /// Subprocess session id (`system.init` event). Useful for
    /// W3-12b's chat-history persistence.
    pub session_id: String,
    /// Cost reported by the `claude` `result.success` event. Sums
    /// into `JobOutcome.total_cost_usd`.
    pub total_cost_usd: f64,
    /// Wall-clock duration of this stage's invoke, measured around
    /// the `transport.invoke` await. Sums into
    /// `JobOutcome.total_duration_ms`.
    pub duration_ms: u64,
    /// Parsed Verdict (W3-12d). Populated only for the `Review`
    /// and `Test` stages — Scout / Plan / Build leave this `None`.
    /// `serde(default)` lets older persisted JSON (no `verdict`
    /// key) deserialize unchanged.
    #[serde(default)]
    pub verdict: Option<Verdict>,
    /// Parsed Coordinator brain decision (W3-12f). Populated only
    /// for the `Classify` stage — every other stage leaves this
    /// `None`. `serde(default)` lets older persisted JSON (no
    /// `coordinator_decision` key) deserialize unchanged.
    #[serde(default)]
    pub coordinator_decision: Option<CoordinatorDecision>,
}

/// One in-flight (or completed) swarm job. The registry indexes by
/// `id`; lookup is exposed via `JobRegistry::get`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// ULID with `j-` prefix per ADR-0007 (e.g. `j-01H8...`).
    pub id: String,
    /// User-supplied free-text goal driving the chain.
    pub goal: String,
    /// Unix epoch milliseconds at job creation (per Charter
    /// timestamp invariant: `_ms` suffix → milliseconds).
    pub created_at_ms: i64,
    /// Current lifecycle state.
    pub state: JobState,
    /// Wired but unused in 12a; W3-12d's Verdict-gated retry loop
    /// reads this to decide whether to short-circuit on persistent
    /// failures past `MAX_RETRIES`.
    pub retry_count: u32,
    /// Append-only list of completed stages. One entry per
    /// successful stage; on failure the failing stage is NOT
    /// appended (its error rides in `last_error`).
    pub stages: Vec<StageResult>,
    /// Populated when `state == Failed`. None on the happy path.
    pub last_error: Option<String>,
    /// Parsed Verdict (W3-12d). Populated only when the FSM
    /// finalized the job as Failed because a Reviewer or Tester
    /// returned `approved=false`. The Verdict IS the structured
    /// error, so on this branch `last_error` stays `None`.
    #[serde(default)]
    pub last_verdict: Option<Verdict>,
    /// W5-04 / W5-06: which executor produced this job —
    /// `"fsm"` is preserved for backwards-compat reads of older
    /// W3-vintage rows; `"brain"` is the W5-03 mailbox-driven
    /// `CoordinatorBrain` path that W5-06's `swarm:run_job`
    /// always writes. Defaults to `"fsm"` on deserialise so
    /// older persisted JSON without the key round-trips
    /// unchanged.
    #[serde(default = "Job::default_source")]
    pub source: String,
}

impl Job {
    /// Default `source` value for backwards-compat deserialisation
    /// (W5-04). Older persisted JSON written before W5-04 lacks the
    /// `source` key; serde substitutes this default so the whole
    /// `Job` parses cleanly without a migration step on every read.
    pub fn default_source() -> String {
        "fsm".into()
    }

    /// Walk `stages` newest-first looking for the most recent
    /// Review/Test entry whose `verdict` came back rejected. Used by
    /// the W3-12e retry loop to label the prior gate ("Reviewer" /
    /// "IntegrationTester") in the retry-Plan prompt.
    ///
    /// Derived rather than stored so the Plan-on-retry path doesn't
    /// require a new SQL column or a parallel field that can drift
    /// out of sync with the persisted `stages` rows. `last_verdict`
    /// alone tells *what* was rejected; this helper tells *which
    /// gate* did the rejecting.
    pub fn last_rejecting_gate(&self) -> Option<JobState> {
        for stage in self.stages.iter().rev() {
            if !matches!(stage.state, JobState::Review | JobState::Test) {
                continue;
            }
            if let Some(verdict) = &stage.verdict {
                if verdict.rejected() {
                    return Some(stage.state);
                }
            }
        }
        None
    }
}

/// Final outcome returned by `swarm:run_job`. Mirrors `Job` minus
/// the lifecycle bookkeeping fields the IPC caller doesn't need
/// (no `state` mid-run, no `created_at_ms` since the wall-clock
/// data is encoded in `total_duration_ms`).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JobOutcome {
    pub job_id: String,
    /// Always `Done` or `Failed` — the FSM never returns mid-state.
    pub final_state: JobState,
    pub stages: Vec<StageResult>,
    /// `Some` on `Failed`, `None` on `Done`.
    pub last_error: Option<String>,
    /// Sum of `StageResult.total_cost_usd` across `stages`.
    pub total_cost_usd: f64,
    /// Sum of `StageResult.duration_ms` across `stages`.
    pub total_duration_ms: u64,
    /// Parsed Verdict (W3-12d). Populated when the FSM finalized
    /// the job as Failed because a Reviewer or Tester verdict came
    /// back rejected. `None` on the happy path and on stage-error
    /// failures.
    #[serde(default)]
    pub last_verdict: Option<Verdict>,
}

/// Slim wire-shape returned by `swarm:list_jobs` (WP-W3-12b §4).
/// Drops the per-stage `assistant_text` / `session_id` payload so
/// the recent-jobs panel can render N jobs without N × per-stage
/// payload bloat.
///
/// `goal` is **char**-truncated to 200 chars at the SQL helper
/// layer (not byte-truncated — Turkish characters!) so the IPC
/// always returns a renderable string of bounded size.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub id: String,
    pub workspace_id: String,
    pub goal: String,
    pub created_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub state: JobState,
    pub stage_count: u32,
    pub total_cost_usd: f64,
    pub last_error: Option<String>,
    /// W5-04: which executor produced this job — `"fsm"` for the
    /// W3 FSM path, `"brain"` for the W5-03 brain-driven path.
    /// Read from `swarm_jobs.source` (see migration 0011).
    /// `serde(default)` lets older persisted JSON without the key
    /// round-trip cleanly.
    #[serde(default = "Job::default_source")]
    pub source: String,
}

/// Full job-detail wire-shape returned by `swarm:get_job` (WP-W3-12b §4).
/// Same fields as `Job` plus the aggregated `total_cost_usd` /
/// `total_duration_ms` and `finished_at_ms` pulled from the DB row.
///
/// `Job` itself stays internal to the FSM so the in-memory
/// bookkeeping fields (`retry_count` for W3-12d, etc.) don't have
/// to ship to the wire before they have a frontend consumer.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JobDetail {
    pub id: String,
    pub workspace_id: String,
    pub goal: String,
    pub created_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub state: JobState,
    pub retry_count: u32,
    pub stages: Vec<StageResult>,
    pub last_error: Option<String>,
    pub total_cost_usd: f64,
    pub total_duration_ms: u64,
    /// Parsed Verdict (W3-12d). Mirrors `Job.last_verdict` —
    /// populated only when the FSM finalized the job as Failed
    /// because a Reviewer or Tester verdict came back rejected.
    #[serde(default)]
    pub last_verdict: Option<Verdict>,
    /// W5-04: which executor produced this job — `"fsm"` for the
    /// W3 FSM path, `"brain"` for the W5-03 brain-driven path.
    /// Read from `swarm_jobs.source` (see migration 0011).
    /// `serde(default)` lets older persisted JSON without the key
    /// round-trip cleanly.
    #[serde(default = "Job::default_source")]
    pub source: String,
}

