//! `Job::last_rejecting_gate` derivation tests (WP-W3-12e).
//!
//! `stage_with_verdict` is local to this suite; `fixture_job` is the
//! shared helper from the package test root.

use super::super::{JobState, StageResult};
use super::fixture_job;
use crate::swarm::coordinator::verdict::Verdict;

fn stage_with_verdict(state: JobState, approved: bool) -> StageResult {
    StageResult {
        state,
        specialist_id: format!("{state:?}").to_lowercase(),
        assistant_text: "x".into(),
        session_id: "sess".into(),
        total_cost_usd: 0.0,
        duration_ms: 0,
        verdict: Some(Verdict {
            approved,
            issues: Vec::new(),
            summary: "s".into(),
        }),
        coordinator_decision: None,
    }
}

/// Empty stages → no rejecting gate.
#[test]
fn last_rejecting_gate_empty_stages_returns_none() {
    let job = fixture_job("j-no-stages");
    assert!(job.last_rejecting_gate().is_none());
}

/// All-approved chain → no rejecting gate.
#[test]
fn last_rejecting_gate_all_approved_returns_none() {
    let mut job = fixture_job("j-all-ok");
    job.stages.push(stage_with_verdict(JobState::Review, true));
    job.stages.push(stage_with_verdict(JobState::Test, true));
    assert!(job.last_rejecting_gate().is_none());
}

/// Reviewer rejected on the most recent attempt → returns Review.
#[test]
fn last_rejecting_gate_returns_review_when_review_rejected() {
    let mut job = fixture_job("j-rev");
    job.stages.push(stage_with_verdict(JobState::Review, false));
    assert_eq!(job.last_rejecting_gate(), Some(JobState::Review));
}

/// Tester rejected after Reviewer approved → returns Test (the
/// most recent rejecting gate, NOT the most recent gate overall).
#[test]
fn last_rejecting_gate_returns_test_when_test_rejected() {
    let mut job = fixture_job("j-test");
    job.stages.push(stage_with_verdict(JobState::Review, true));
    job.stages.push(stage_with_verdict(JobState::Test, false));
    assert_eq!(job.last_rejecting_gate(), Some(JobState::Test));
}

/// Stages without verdicts (Scout/Plan/Build) are skipped.
#[test]
fn last_rejecting_gate_skips_non_verdict_stages() {
    let mut job = fixture_job("j-mix");
    // A Scout stage with `verdict=None` must not throw the helper
    // off; only Review/Test entries with rejected verdicts count.
    job.stages.push(StageResult {
        state: JobState::Scout,
        specialist_id: "scout".into(),
        assistant_text: "sc".into(),
        session_id: "s".into(),
        total_cost_usd: 0.0,
        duration_ms: 0,
        verdict: None,
        coordinator_decision: None,
    });
    job.stages.push(stage_with_verdict(JobState::Review, false));
    assert_eq!(job.last_rejecting_gate(), Some(JobState::Review));
}

/// Newest rejection wins — even if an earlier Review rejected,
/// the most recent rejecting gate is the one returned. This
/// matches the retry loop's intent: label the gate that just
/// triggered the upcoming retry, not an older one.
#[test]
fn last_rejecting_gate_returns_newest_rejection() {
    let mut job = fixture_job("j-newest");
    job.stages.push(stage_with_verdict(JobState::Review, false));
    // Retry round: Reviewer approved this time, Tester rejected.
    job.stages.push(stage_with_verdict(JobState::Plan, true)); // no verdict shape; helper ignores
    job.stages.push(stage_with_verdict(JobState::Review, true));
    job.stages.push(stage_with_verdict(JobState::Test, false));
    assert_eq!(job.last_rejecting_gate(), Some(JobState::Test));
}
