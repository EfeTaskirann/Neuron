//! `JobState` — the swarm job lifecycle enum (WP-W3-12a §2).
//!
//! Pure type: no registry, no IO. Crosses the IPC boundary as part
//! of `Job` / `JobSummary` / `JobDetail` and carries both the
//! JS-facing camelCase wire repr (via specta) and the
//! `swarm_jobs.state` snake_case DB repr (via [`JobState::as_db_str`]
//! / [`JobState::from_db_str`]).

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::AppError;

/// Lifecycle states of a swarm job. Per WP §2:
///
/// - `Init` — newly minted, before the first transition fires.
/// - `Scout` — read-only investigation stage.
/// - `Classify` — single-shot Coordinator brain decision (W3-12f);
///   sits between Scout and Plan so the FSM can short-circuit on
///   research-only goals. The variant is reachable on every job;
///   ResearchOnly takes the Done short-circuit, ExecutePlan falls
///   through to Plan.
/// - `Plan` / `Build` — the next two happy-path stages on the
///   ExecutePlan branch.
/// - `Review` / `Test` — Verdict-gated quality stages (W3-12d).
/// - `Done` / `Failed` — terminal.
///
/// `Hash` + `Eq` are derived so the FSM can build small lookup
/// tables keyed on the state if it ever needs to (W3-12d).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Init,
    Scout,
    /// Coordinator brain routing decision (W3-12f). Single-shot; the
    /// FSM enters this state once per job between Scout and Plan.
    Classify,
    Plan,
    Build,
    /// Verdict gate (W3-12d). FSM enters this state after Build on
    /// the ExecutePlan branch.
    Review,
    /// Verdict gate (W3-12d). FSM enters this state after a
    /// Review-approved verdict.
    Test,
    Done,
    /// Terminal failure state. Carries the last error in
    /// `Job.last_error`.
    Failed,
}

impl JobState {
    /// Stable string used in the `swarm_jobs.state` column. The
    /// repr matches `serde(rename_all = "snake_case")` on the
    /// JS-facing wire enum so DB values and frontend values agree
    /// (which simplifies the W3-14 hook).
    pub fn as_db_str(&self) -> &'static str {
        match self {
            JobState::Init => "init",
            JobState::Scout => "scout",
            JobState::Classify => "classify",
            JobState::Plan => "plan",
            JobState::Build => "build",
            JobState::Review => "review",
            JobState::Test => "test",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }

    /// Inverse of [`Self::as_db_str`]. Unknown discriminants surface
    /// as `AppError::Internal` so a corrupted DB never silently maps
    /// to a default state.
    pub fn from_db_str(s: &str) -> Result<Self, AppError> {
        match s {
            "init" => Ok(JobState::Init),
            "scout" => Ok(JobState::Scout),
            "classify" => Ok(JobState::Classify),
            "plan" => Ok(JobState::Plan),
            "build" => Ok(JobState::Build),
            "review" => Ok(JobState::Review),
            "test" => Ok(JobState::Test),
            "done" => Ok(JobState::Done),
            "failed" => Ok(JobState::Failed),
            other => Err(AppError::Internal(format!(
                "unknown swarm job state in DB: {other}"
            ))),
        }
    }

    /// Whether this state is terminal (`Done` or `Failed`).
    /// Used by the recovery sweep to leave already-finalized rows
    /// alone.
    pub fn is_terminal(&self) -> bool {
        matches!(self, JobState::Done | JobState::Failed)
    }
}
