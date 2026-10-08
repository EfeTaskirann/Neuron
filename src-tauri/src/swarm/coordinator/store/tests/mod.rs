//! Store persistence tests, split along the production seams.
//!
//! Each submodule mirrors one production file in the `store` package:
//!
//! - [`write`] — `JobRegistry`-driven INSERT/UPDATE of `swarm_jobs` /
//!   `swarm_stages` and the workspace-lock bookkeeping.
//! - [`read`] — `list_jobs` filtering/limit/order + `get_job_detail`.
//! - [`recovery`] — the `recover_orphans` process-start sweep.
//! - [`cols`] — `truncate_chars` + verdict/decision JSON-column
//!   codec round-trips (registry → SQLite → store reload).
//! - [`migration`] — schema-pragma probes (0006/0007/0008) so future
//!   migration drift surfaces here rather than mid-write.
//!
//! Shared fixtures (`fixture_job` / `fixture_stage`) live here; the
//! write/read/cols suites build `Job`/`StageResult` values from them.

use crate::swarm::coordinator::job::{Job, JobState, StageResult};

mod cols;
mod migration;
mod read;
mod recovery;
mod write;

pub(super) fn fixture_job(id: &str, goal: &str, created_at_ms: i64) -> Job {
    Job {
        id: id.to_string(),
        goal: goal.to_string(),
        created_at_ms,
        state: JobState::Init,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    }
}

pub(super) fn fixture_stage(state: JobState, cost: f64, dur: u64) -> StageResult {
    StageResult {
        state,
        specialist_id: format!("{state:?}").to_lowercase(),
        assistant_text: format!("text-{state:?}"),
        session_id: format!("sess-{state:?}"),
        total_cost_usd: cost,
        duration_ms: dur,
        verdict: None,
        coordinator_decision: None,
    }
}
