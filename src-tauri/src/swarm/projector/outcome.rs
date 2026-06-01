//! `JobProjector::build_outcome` aggregation — walks a job's mailbox
//! event log (chronological) and totalises the final [`JobOutcome`].
//! One builder per `build_outcome` call (WP-W5-04). Extracted
//! verbatim from `projector.rs`.

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::{
    parse_verdict, JobOutcome, JobState, StageResult, Verdict,
};
use crate::swarm::mailbox_bus::{MailboxEnvelope, MailboxEvent};

use super::helpers::agent_id_to_job_state;

/// Walks the event log (chronological order) and accumulates the
/// fields needed for [`JobOutcome`]. Stateless across jobs — one
/// builder per `build_outcome` call.
pub(super) struct OutcomeBuilder {
    job_id: String,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    final_state: JobState,
    last_error: Option<String>,
    last_rejected_verdict: Option<Verdict>,
    stages: Vec<StageResult>,
    /// Track agent_id → number of prior dispatches (for retry
    /// observation, but `build_outcome` doesn't emit retries — it
    /// just totalises stages).
    _dispatch_history: Vec<String>,
}

impl OutcomeBuilder {
    pub(super) fn new(job_id: String) -> Self {
        Self {
            job_id,
            started_at_ms: 0,
            finished_at_ms: None,
            final_state: JobState::Failed,
            last_error: Some("no JobFinished event in log".into()),
            last_rejected_verdict: None,
            stages: Vec::new(),
            _dispatch_history: Vec::new(),
        }
    }

    pub(super) fn observe(&mut self, env: &MailboxEnvelope) {
        match &env.event {
            MailboxEvent::JobStarted { .. } => {
                self.started_at_ms = env.ts * 1_000;
            }
            MailboxEvent::TaskDispatch { target, .. } => {
                self._dispatch_history.push(target.clone());
            }
            MailboxEvent::AgentResult {
                agent_id,
                assistant_text,
                total_cost_usd,
                ..
            } => {
                let job_state = agent_id_to_job_state(agent_id);
                let verdict = if matches!(
                    job_state,
                    JobState::Review | JobState::Test
                ) {
                    parse_verdict(assistant_text).ok()
                } else {
                    None
                };
                if let Some(v) = &verdict {
                    if v.rejected() {
                        self.last_rejected_verdict = Some(v.clone());
                    }
                }
                self.stages.push(StageResult {
                    state: job_state,
                    specialist_id: agent_id.clone(),
                    assistant_text: assistant_text.clone(),
                    session_id: String::new(),
                    total_cost_usd: *total_cost_usd,
                    duration_ms: 0,
                    verdict,
                    coordinator_decision: None,
                });
            }
            MailboxEvent::JobFinished {
                outcome, summary, ..
            } => {
                self.final_state = if outcome == "done" {
                    JobState::Done
                } else {
                    JobState::Failed
                };
                self.last_error = if self.final_state == JobState::Failed {
                    Some(summary.clone())
                } else {
                    None
                };
                self.finished_at_ms = Some(env.ts * 1_000);
            }
            MailboxEvent::JobCancel { .. } => {
                self.final_state = JobState::Failed;
                self.last_error = Some("cancelled by user".into());
            }
            // help requests / outcomes / notes don't shape the
            // outcome aggregate.
            _ => {}
        }
    }

    pub(super) async fn finish(self, _pool: &DbPool) -> Result<JobOutcome, AppError> {
        let total_cost_usd: f64 =
            self.stages.iter().map(|s| s.total_cost_usd).sum();
        let total_duration_ms = self
            .finished_at_ms
            .map(|f| (f - self.started_at_ms).max(0) as u64)
            .unwrap_or(0);
        let last_verdict = if self.final_state == JobState::Failed {
            self.last_rejected_verdict
        } else {
            None
        };
        Ok(JobOutcome {
            job_id: self.job_id,
            final_state: self.final_state,
            stages: self.stages,
            last_error: self.last_error,
            total_cost_usd,
            total_duration_ms,
            last_verdict,
        })
    }
}
