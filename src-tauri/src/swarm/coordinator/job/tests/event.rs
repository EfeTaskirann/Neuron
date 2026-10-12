//! `SwarmJobEvent` wire-shape pin — the per-job lifecycle event
//! streamed to `swarm:job:{job_id}:event`.

use super::super::{JobState, SwarmJobEvent};
use crate::swarm::coordinator::verdict::Verdict;

/// `SwarmJobEvent::RetryStarted` serializes to the documented
/// snake_case wire shape with all fields present at the top level.
#[test]
fn swarm_job_event_retry_started_serializes() {
    let evt = SwarmJobEvent::RetryStarted {
        job_id: "j-1".into(),
        attempt: 2,
        max_retries: 2,
        triggered_by: JobState::Review,
        verdict: Verdict {
            approved: false,
            issues: Vec::new(),
            summary: "rejected".into(),
        },
    };
    let json = serde_json::to_value(&evt).expect("serialize");
    assert_eq!(
        json.get("kind").and_then(|v| v.as_str()),
        Some("retry_started")
    );
    assert_eq!(json.get("attempt").and_then(|v| v.as_u64()), Some(2));
    assert_eq!(
        json.get("max_retries").and_then(|v| v.as_u64()),
        Some(2)
    );
    assert_eq!(
        json.get("triggered_by").and_then(|v| v.as_str()),
        Some("review"),
        "triggered_by uses JobState's snake_case wire shape"
    );
    let verdict = json.get("verdict").expect("verdict embedded");
    assert_eq!(
        verdict.get("approved").and_then(|v| v.as_bool()),
        Some(false)
    );
}
