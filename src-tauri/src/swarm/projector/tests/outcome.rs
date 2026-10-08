//! `JobProjector::build_outcome` aggregation + `get_brain_job_detail`
//! legacy-shape reload tests. These seed the event chain directly via
//! the bus (no running projector) and exercise pure reconstruction.

use super::super::*;
use crate::swarm::coordinator::{JobState, Verdict};
use crate::swarm::mailbox_bus::MailboxEvent;
use crate::test_support::mock_app_with_pool;

#[tokio::test]
async fn build_outcome_walks_event_log_correctly() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    // Seed a complete event chain via the bus directly. No
    // projector needed — `build_outcome` is pure aggregation.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-bo".into(),
            workspace_id: "default".into(),
            goal: "build the thing".into(),
        },
    )
    .await
    .expect("emit");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:scout",
        "agent:coordinator",
        "result",
        None,
        MailboxEvent::AgentResult {
            job_id: "j-bo".into(),
            agent_id: "scout".into(),
            assistant_text: "found".into(),
            total_cost_usd: 0.10,
            turn_count: 1,
        },
    )
    .await
    .expect("emit");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:planner",
        "agent:coordinator",
        "result",
        None,
        MailboxEvent::AgentResult {
            job_id: "j-bo".into(),
            agent_id: "planner".into(),
            assistant_text: "plan".into(),
            total_cost_usd: 0.20,
            turn_count: 1,
        },
    )
    .await
    .expect("emit");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:user",
        "finished",
        None,
        MailboxEvent::JobFinished {
            job_id: "j-bo".into(),
            outcome: "done".into(),
            summary: "ok".into(),
        },
    )
    .await
    .expect("emit");

    let outcome = JobProjector::build_outcome(&bus, &pool, "j-bo")
        .await
        .expect("build_outcome");
    assert_eq!(outcome.job_id, "j-bo");
    assert_eq!(outcome.final_state, JobState::Done);
    assert_eq!(outcome.stages.len(), 2);
    assert_eq!(outcome.stages[0].state, JobState::Scout);
    assert_eq!(outcome.stages[1].state, JobState::Plan);
    assert!(
        (outcome.total_cost_usd - 0.30).abs() < 1e-9,
        "cost summed: {}",
        outcome.total_cost_usd
    );
    assert!(outcome.last_error.is_none());
    assert!(outcome.last_verdict.is_none());
}

#[tokio::test]
async fn build_outcome_handles_brain_driven_job_with_retry() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    // A retry-shaped chain: planner → reviewer (rejected) →
    // planner (retry) → reviewer (approved) → finished.
    let job_id = "j-retry-bo";
    let dispatches: &[(MailboxEvent, &str)] = &[
        (
            MailboxEvent::JobStarted {
                job_id: job_id.into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
            "started",
        ),
        (
            MailboxEvent::AgentResult {
                job_id: job_id.into(),
                agent_id: "planner".into(),
                assistant_text: "plan v1".into(),
                total_cost_usd: 0.05,
                turn_count: 1,
            },
            "plan-v1",
        ),
        (
            MailboxEvent::AgentResult {
                job_id: job_id.into(),
                agent_id: "backend-reviewer".into(),
                assistant_text:
                    r#"{"approved":false,"issues":[],"summary":"plan rough"}"#
                        .into(),
                total_cost_usd: 0.02,
                turn_count: 1,
            },
            "rev-1",
        ),
        (
            MailboxEvent::AgentResult {
                job_id: job_id.into(),
                agent_id: "planner".into(),
                assistant_text: "plan v2".into(),
                total_cost_usd: 0.05,
                turn_count: 1,
            },
            "plan-v2",
        ),
        (
            MailboxEvent::AgentResult {
                job_id: job_id.into(),
                agent_id: "backend-reviewer".into(),
                assistant_text:
                    r#"{"approved":true,"issues":[],"summary":"good"}"#.into(),
                total_cost_usd: 0.02,
                turn_count: 1,
            },
            "rev-2",
        ),
        (
            MailboxEvent::JobFinished {
                job_id: job_id.into(),
                outcome: "done".into(),
                summary: "ok".into(),
            },
            "fin",
        ),
    ];
    for (ev, sum) in dispatches {
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            sum,
            None,
            ev.clone(),
        )
        .await
        .expect("emit");
    }
    let outcome = JobProjector::build_outcome(&bus, &pool, job_id)
        .await
        .expect("build_outcome");
    assert_eq!(outcome.final_state, JobState::Done);
    assert_eq!(outcome.stages.len(), 4);
    // last stage is the approved reviewer.
    let last = outcome.stages.last().expect("non-empty");
    assert_eq!(last.state, JobState::Review);
    assert!(last.verdict.as_ref().expect("approved verdict").approved);
    assert!(outcome.last_verdict.is_none(), "Done outcome must clear last_verdict");
}

/// `swarm:get_job` semantics — a brain-driven job's row + stages
/// reload through the legacy `JobDetail` shape.
#[tokio::test]
async fn swarm_get_job_returns_brain_driven_job_in_legacy_shape() {
    let (_, pool, _dir) = mock_app_with_pool().await;
    // Seed a brain-driven row directly via SQL so the test
    // doesn't depend on the projector running.
    sqlx::query(
        "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("j-brainshape")
    .bind("default")
    .bind("brain-shaped goal")
    .bind(123_i64)
    .bind("done")
    .bind(0_i64)
    .bind("brain")
    .execute(&pool)
    .await
    .expect("seed");
    sqlx::query(
        "INSERT INTO swarm_stages (job_id, idx, state, specialist_id, assistant_text, session_id, total_cost_usd, duration_ms, created_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("j-brainshape")
    .bind(0_i64)
    .bind("scout")
    .bind("scout")
    .bind("found")
    .bind("")
    .bind(0.05)
    .bind(0_i64)
    .bind(123_i64)
    .execute(&pool)
    .await
    .expect("seed stage");

    let detail = get_brain_job_detail(&pool, "j-brainshape")
        .await
        .expect("query")
        .expect("Some");
    assert_eq!(detail.id, "j-brainshape");
    assert_eq!(detail.workspace_id, "default");
    assert_eq!(detail.source, "brain");
    assert_eq!(detail.state, JobState::Done);
    assert_eq!(detail.stages.len(), 1);
    assert_eq!(detail.stages[0].state, JobState::Scout);
    assert_eq!(detail.stages[0].specialist_id, "scout");
}

/// Smoke for the verdict reject capture used by JobOutcome.last_verdict
/// — pin that a rejected reviewer verdict, followed by a Failed
/// JobFinished, surfaces the verdict on the outcome.
#[tokio::test]
async fn build_outcome_surfaces_last_rejected_verdict_on_failed() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let job_id = "j-verd-fail";
    let _ = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:coordinator",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: job_id.into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await;
    let _ = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:backend-reviewer",
            "agent:coordinator",
            "rev",
            None,
            MailboxEvent::AgentResult {
                job_id: job_id.into(),
                agent_id: "backend-reviewer".into(),
                assistant_text:
                    r#"{"approved":false,"issues":[],"summary":"bad"}"#
                        .into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            },
        )
        .await;
    let _ = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:user",
            "fin",
            None,
            MailboxEvent::JobFinished {
                job_id: job_id.into(),
                outcome: "failed".into(),
                summary: "reviewer rejected".into(),
            },
        )
        .await;
    let outcome = JobProjector::build_outcome(&bus, &pool, job_id)
        .await
        .expect("build_outcome");
    assert_eq!(outcome.final_state, JobState::Failed);
    let v: &Verdict = outcome
        .last_verdict
        .as_ref()
        .expect("verdict surfaced on Failed");
    assert!(!v.approved);
    assert_eq!(v.summary, "bad");
}

