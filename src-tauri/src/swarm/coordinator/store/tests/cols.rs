//! JSON column codecs + goal truncation: `truncate_chars` plus the
//! verdict/decision round-trips (registry → SQLite → store reload)
//! that pin the `cols` serializer/deserializer fidelity.

use super::super::cols::truncate_chars;
use super::super::get_job_detail;
use super::fixture_job;
use crate::swarm::coordinator::job::{JobState, StageResult};
use crate::swarm::coordinator::JobRegistry;
use crate::test_support::fresh_pool;

/// `truncate_chars` short-circuits when the input fits.
#[test]
fn truncate_chars_passthrough() {
    let s = "abc";
    assert_eq!(truncate_chars(s, 10), "abc");
}

/// `truncate_chars` cuts on character boundaries.
#[test]
fn truncate_chars_cuts_on_codepoint() {
    let s = "abçdeş";
    let t = truncate_chars(s, 3);
    assert_eq!(t.chars().count(), 3);
    assert_eq!(t, "abç");
}

/// WP-W3-12d — a Failed job with a populated `last_verdict`
/// round-trips through the registry → SQLite → store reload.
/// The Verdict must reappear with bit-for-bit fidelity (issue
/// list, severities, summary). Per-stage verdicts on Review /
/// Test stages also survive.
#[tokio::test]
async fn verdict_persists_across_app_restart() {
    use crate::swarm::coordinator::verdict::{
        Verdict, VerdictIssue, VerdictSeverity,
    };
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace(
        "ws-verdict",
        fixture_job("j-verdict", "g", 0),
    )
    .await
    .expect("acquire");

    // Push a Review stage with a populated Verdict, then mark
    // the job Failed with `last_verdict` set — mirrors the
    // FSM's `finalize_failed_with_verdict` shape.
    let approved_review = Verdict {
        approved: true,
        issues: Vec::new(),
        summary: "looks fine".to_string(),
    };
    let rejected_test = Verdict {
        approved: false,
        issues: vec![VerdictIssue {
            severity: VerdictSeverity::High,
            file: Some("tests/foo.rs".to_string()),
            line: Some(7),
            message: "test_bar fails".to_string(),
        }],
        summary: "1 failure".to_string(),
    };
    let approved_review_clone = approved_review.clone();
    let rejected_test_clone = rejected_test.clone();
    reg.update("j-verdict", |j| {
        j.stages.push(StageResult {
            state: JobState::Review,
            specialist_id: "backend-reviewer".into(),
            assistant_text: "ok".into(),
            session_id: "s-r".into(),
            total_cost_usd: 0.001,
            duration_ms: 12,
            verdict: Some(approved_review_clone),
            coordinator_decision: None,
        });
        j.state = JobState::Failed;
        j.last_verdict = Some(rejected_test_clone);
    })
    .await
    .expect("update");

    // Reload through the read path — get_job_detail must surface
    // both the per-stage verdict and the job-level last_verdict.
    let detail = get_job_detail(&pool, "j-verdict")
        .await
        .expect("query")
        .expect("Some");
    assert_eq!(detail.state, JobState::Failed);
    assert_eq!(detail.last_verdict.as_ref(), Some(&rejected_test));
    assert_eq!(detail.stages.len(), 1);
    assert_eq!(
        detail.stages[0].verdict.as_ref(),
        Some(&approved_review)
    );
    assert_eq!(detail.last_error, None);
}

/// WP-W3-12f — a Classify stage with a populated
/// `coordinator_decision` round-trips through the registry →
/// SQLite → store reload. The decision must reappear with
/// bit-for-bit fidelity (route + reasoning).
#[tokio::test]
async fn coordinator_decision_persists_across_app_restart() {
    use crate::swarm::coordinator::decision::{
        CoordinatorDecision, CoordinatorRoute,
    };
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace(
        "ws-decision",
        fixture_job("j-decision", "g", 0),
    )
    .await
    .expect("acquire");

    let decision = CoordinatorDecision {
        route: CoordinatorRoute::ResearchOnly,
        // W3-12g: scope is required on the wire shape; persisted
        // legacy rows default to Backend via serde.
        scope: crate::swarm::coordinator::decision::CoordinatorScope::Backend,
        reasoning: "explain-only goal; Scout findings cover it".into(),
    };
    let decision_clone = decision.clone();
    reg.update("j-decision", |j| {
        // First a Scout stage with no decision (canonical shape).
        j.stages.push(StageResult {
            state: JobState::Scout,
            specialist_id: "scout".into(),
            assistant_text: "scout findings".into(),
            session_id: "s-sc".into(),
            total_cost_usd: 0.001,
            duration_ms: 10,
            verdict: None,
            coordinator_decision: None,
        });
        // Then a Classify stage with the decision stamped on.
        j.stages.push(StageResult {
            state: JobState::Classify,
            specialist_id: "coordinator".into(),
            assistant_text: serde_json::to_string(&decision_clone)
                .unwrap(),
            session_id: "s-cls".into(),
            total_cost_usd: 0.001,
            duration_ms: 5,
            verdict: None,
            coordinator_decision: Some(decision_clone),
        });
        j.state = JobState::Done;
    })
    .await
    .expect("update");

    let detail = get_job_detail(&pool, "j-decision")
        .await
        .expect("query")
        .expect("Some");
    assert_eq!(detail.stages.len(), 2);
    assert!(detail.stages[0].coordinator_decision.is_none());
    assert_eq!(
        detail.stages[1].coordinator_decision.as_ref(),
        Some(&decision)
    );
    assert_eq!(detail.stages[1].state, JobState::Classify);
}

/// WP-W3-12g — `CoordinatorDecision.scope` round-trips through
/// SQLite (route + scope + reasoning all bit-for-bit). Sister
/// test to `coordinator_decision_persists_across_app_restart`
/// pinning the new `scope` field specifically — guards against
/// future serializer drift dropping the field on the wire.
#[tokio::test]
async fn coordinator_decision_round_trips_through_sqlite_with_scope() {
    use crate::swarm::coordinator::decision::{
        CoordinatorDecision, CoordinatorRoute, CoordinatorScope,
    };
    let (pool, _dir) = fresh_pool().await;
    let reg = JobRegistry::with_pool(pool.clone());
    reg.try_acquire_workspace(
        "ws-scope",
        fixture_job("j-scope", "g", 0),
    )
    .await
    .expect("acquire");

    let decision = CoordinatorDecision {
        route: CoordinatorRoute::ExecutePlan,
        scope: CoordinatorScope::Frontend,
        reasoning: "frontend goal; execute via FE chain".into(),
    };
    let decision_clone = decision.clone();
    reg.update("j-scope", |j| {
        j.stages.push(StageResult {
            state: JobState::Classify,
            specialist_id: "coordinator".into(),
            assistant_text: serde_json::to_string(&decision_clone)
                .unwrap(),
            session_id: "s-cls".into(),
            total_cost_usd: 0.001,
            duration_ms: 5,
            verdict: None,
            coordinator_decision: Some(decision_clone),
        });
        j.state = JobState::Done;
    })
    .await
    .expect("update");

    let detail = get_job_detail(&pool, "j-scope")
        .await
        .expect("query")
        .expect("Some");
    let reloaded = detail.stages[0]
        .coordinator_decision
        .as_ref()
        .expect("decision present after reload");
    // Field-by-field assertion so a regression on any of the
    // three points the failure at the right line.
    assert_eq!(reloaded.route, CoordinatorRoute::ExecutePlan);
    assert_eq!(reloaded.scope, CoordinatorScope::Frontend);
    assert_eq!(
        reloaded.reasoning,
        "frontend goal; execute via FE chain"
    );
    // Sanity: the entire struct round-trips by value too.
    assert_eq!(reloaded, &decision);
}
