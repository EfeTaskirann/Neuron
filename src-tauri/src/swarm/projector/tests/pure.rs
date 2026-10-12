//! Pure-helper micro-tests: the `agent_id -> JobState` mapping table
//! and the `is_retry_dispatch` prior-count math.

use super::super::helpers::{agent_id_to_job_state, is_retry_dispatch};
use crate::swarm::coordinator::JobState;

#[test]
fn projector_maps_agent_id_to_correct_job_state() {
    // Pin every row of the W5-04 §5 mapping table.
    assert_eq!(agent_id_to_job_state("scout"), JobState::Scout);
    assert_eq!(
        agent_id_to_job_state("coordinator"),
        JobState::Classify
    );
    assert_eq!(agent_id_to_job_state("planner"), JobState::Plan);
    assert_eq!(
        agent_id_to_job_state("backend-builder"),
        JobState::Build
    );
    assert_eq!(
        agent_id_to_job_state("frontend-builder"),
        JobState::Build
    );
    assert_eq!(
        agent_id_to_job_state("backend-reviewer"),
        JobState::Review
    );
    assert_eq!(
        agent_id_to_job_state("frontend-reviewer"),
        JobState::Review
    );
    assert_eq!(
        agent_id_to_job_state("integration-tester"),
        JobState::Test
    );
    // Defensive default for unknown ids.
    assert_eq!(
        agent_id_to_job_state("future-persona-id"),
        JobState::Build
    );
}

/// The projector_emits_retry_attempt test covers RetryStarted
/// emission via the bus; this micro-test pins the pure helper
/// `is_retry_dispatch` so a regression on the prior-count math
/// surfaces here, not via a slower e2e test.
#[test]
fn is_retry_dispatch_counts_prior_targets() {
    let history: Vec<String> = vec![];
    assert_eq!(is_retry_dispatch("agent:scout", &history), None);
    let history = vec!["agent:scout".to_string()];
    assert_eq!(is_retry_dispatch("agent:scout", &history), Some(2));
    let history =
        vec!["agent:scout".to_string(), "agent:scout".to_string()];
    assert_eq!(is_retry_dispatch("agent:scout", &history), Some(3));
    let history = vec!["agent:planner".to_string()];
    assert_eq!(is_retry_dispatch("agent:scout", &history), None);
}
