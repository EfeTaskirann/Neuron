//! Projector event-emission integration tests: drive the FSM via the
//! mailbox bus and assert the `swarm:job:{id}:event` payloads (and the
//! `swarm_jobs` / `swarm_stages` write-through) the projector emits.

use super::super::*;
use super::{drain_until, install_event_capturer};
use crate::swarm::mailbox_bus::MailboxEvent;
use crate::test_support::mock_app_with_pool;
use std::time::Duration;

#[tokio::test]
async fn projector_emits_started_on_job_started() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let captured = install_event_capturer(&app, "j-started");
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );

    // Sleep one tick to let `subscribe` complete inside the
    // task before we emit (otherwise the broadcast send would
    // race the recv on slow runners).
    tokio::time::sleep(Duration::from_millis(20)).await;

    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-started".into(),
            workspace_id: "default".into(),
            goal: "test goal".into(),
        },
    )
    .await
    .expect("emit");

    drain_until(&captured, |evts| {
        evts.iter().any(|e| e.kind == "started")
    })
    .await;

    let snap = captured.lock().expect("lock").clone();
    let started = snap
        .iter()
        .find(|e| e.kind == "started")
        .expect("started present");
    assert_eq!(
        started.json.get("job_id").and_then(|v| v.as_str()),
        Some("j-started")
    );
    assert_eq!(
        started.json.get("workspace_id").and_then(|v| v.as_str()),
        Some("default")
    );
    assert_eq!(
        started.json.get("goal").and_then(|v| v.as_str()),
        Some("test goal")
    );
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_emits_stage_started_on_task_dispatch() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let captured = install_event_capturer(&app, "j-disp");
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-disp".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("emit");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:scout",
        "dispatch",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-disp".into(),
            target: "agent:scout".into(),
            prompt: "investigate".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit dispatch");
    drain_until(&captured, |evts| {
        evts.iter().any(|e| e.kind == "stage_started")
    })
    .await;
    let snap = captured.lock().expect("lock").clone();
    let ev = snap
        .iter()
        .find(|e| e.kind == "stage_started")
        .expect("StageStarted present");
    assert_eq!(
        ev.json.get("job_id").and_then(|v| v.as_str()),
        Some("j-disp")
    );
    assert_eq!(
        ev.json.get("state").and_then(|v| v.as_str()),
        Some("scout")
    );
    assert_eq!(
        ev.json.get("specialist_id").and_then(|v| v.as_str()),
        Some("scout")
    );
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_emits_stage_completed_on_agent_result() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let captured = install_event_capturer(&app, "j-res");
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-res".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
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
            job_id: "j-res".into(),
            agent_id: "scout".into(),
            assistant_text: "found auth.rs".into(),
            total_cost_usd: 0.05,
            turn_count: 1,
        },
    )
    .await
    .expect("emit result");
    drain_until(&captured, |evts| {
        evts.iter().any(|e| e.kind == "stage_completed")
    })
    .await;
    let snap = captured.lock().expect("lock").clone();
    let ev = snap
        .iter()
        .find(|e| e.kind == "stage_completed")
        .expect("StageCompleted present");
    assert_eq!(
        ev.json.get("job_id").and_then(|v| v.as_str()),
        Some("j-res")
    );
    let stage = ev.json.get("stage").expect("stage embedded");
    assert_eq!(
        stage.get("state").and_then(|v| v.as_str()),
        Some("scout")
    );
    assert_eq!(
        stage.get("specialistId").and_then(|v| v.as_str()),
        Some("scout")
    );
    assert_eq!(
        stage.get("assistantText").and_then(|v| v.as_str()),
        Some("found auth.rs")
    );
    let cost = stage
        .get("totalCostUsd")
        .and_then(|v| v.as_f64())
        .expect("cost number");
    assert!((cost - 0.05).abs() < f64::EPSILON);
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_inserts_swarm_jobs_row_with_brain_source() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-brainrow".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("emit");

    // Poll the SQL row with a deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT state, source FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-brainrow")
        .fetch_optional(&pool)
        .await
        .expect("query");
        if let Some((state, source)) = row {
            assert_eq!(state, "init");
            assert_eq!(source, "brain");
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("swarm_jobs row never landed");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_inserts_swarm_stages_row() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-stagerow".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("emit started");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:scout",
        "agent:coordinator",
        "result",
        None,
        MailboxEvent::AgentResult {
            job_id: "j-stagerow".into(),
            agent_id: "planner".into(),
            assistant_text: "plan".into(),
            total_cost_usd: 0.10,
            turn_count: 2,
        },
    )
    .await
    .expect("emit result");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_stages WHERE job_id = ?",
        )
        .bind("j-stagerow")
        .fetch_one(&pool)
        .await
        .expect("count");
        if count >= 1 {
            let (state_s, specialist_s): (String, String) =
                sqlx::query_as(
                    "SELECT state, specialist_id FROM swarm_stages WHERE job_id = ? AND idx = 0",
                )
                .bind("j-stagerow")
                .fetch_one(&pool)
                .await
                .expect("row");
            assert_eq!(state_s, "plan");
            assert_eq!(specialist_s, "planner");
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("swarm_stages row never landed");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_emits_retry_attempt_on_repeated_dispatch() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let captured = install_event_capturer(&app, "j-retry");
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-retry".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("emit");
    // First dispatch — no retry.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:planner",
        "first",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-retry".into(),
            target: "agent:planner".into(),
            prompt: "plan v1".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit");
    // Wait for the first StageStarted to land so the
    // projector's history is committed before the second
    // dispatch (otherwise the broadcast race could see both
    // dispatches before the first updates history).
    drain_until(&captured, |evts| {
        evts.iter().filter(|e| e.kind == "stage_started").count() >= 1
    })
    .await;
    // Second dispatch — same target → retry.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:planner",
        "second",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-retry".into(),
            target: "agent:planner".into(),
            prompt: "plan v2".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit");
    drain_until(&captured, |evts| {
        evts.iter().any(|e| e.kind == "retry_started")
    })
    .await;
    let snap = captured.lock().expect("lock").clone();
    let ev = snap
        .iter()
        .find(|e| e.kind == "retry_started")
        .expect("RetryStarted present");
    assert_eq!(
        ev.json.get("attempt").and_then(|v| v.as_u64()),
        Some(2),
        "second dispatch is attempt 2"
    );
    assert_eq!(
        ev.json.get("triggered_by").and_then(|v| v.as_str()),
        Some("plan")
    );
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_does_not_emit_retry_on_first_dispatch() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let captured = install_event_capturer(&app, "j-noretry");
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-noretry".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("emit");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:scout",
        "first",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-noretry".into(),
            target: "agent:scout".into(),
            prompt: "go".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit");
    drain_until(&captured, |evts| {
        evts.iter().any(|e| e.kind == "stage_started")
    })
    .await;
    // Settle a beat so any (incorrect) RetryStarted would land.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let snap = captured.lock().expect("lock").clone();
    let retry_count = snap.iter().filter(|e| e.kind == "retry_started").count();
    assert_eq!(retry_count, 0, "first dispatch must not emit RetryStarted");
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_emits_finished_on_job_finished_done() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let captured = install_event_capturer(&app, "j-fin-done");
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-fin-done".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("emit started");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:user",
        "finished",
        None,
        MailboxEvent::JobFinished {
            job_id: "j-fin-done".into(),
            outcome: "done".into(),
            summary: "ok".into(),
        },
    )
    .await
    .expect("emit finished");
    drain_until(&captured, |evts| evts.iter().any(|e| e.kind == "finished"))
        .await;
    let snap = captured.lock().expect("lock").clone();
    let ev = snap
        .iter()
        .find(|e| e.kind == "finished")
        .expect("Finished present");
    let outcome = ev.json.get("outcome").expect("outcome");
    assert_eq!(
        outcome.get("finalState").and_then(|v| v.as_str()),
        Some("done")
    );
    assert!(
        outcome.get("lastError").map(|v| v.is_null()).unwrap_or(true),
        "last_error null on done"
    );
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_emits_finished_on_job_finished_failed() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let captured = install_event_capturer(&app, "j-fin-failed");
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-fin-failed".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
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
            job_id: "j-fin-failed".into(),
            outcome: "failed".into(),
            summary: "exceeded max dispatches".into(),
        },
    )
    .await
    .expect("emit");
    drain_until(&captured, |evts| evts.iter().any(|e| e.kind == "finished"))
        .await;
    let snap = captured.lock().expect("lock").clone();
    let ev = snap
        .iter()
        .find(|e| e.kind == "finished")
        .expect("Finished present");
    let outcome = ev.json.get("outcome").expect("outcome");
    assert_eq!(
        outcome.get("finalState").and_then(|v| v.as_str()),
        Some("failed")
    );
    assert_eq!(
        outcome.get("lastError").and_then(|v| v.as_str()),
        Some("exceeded max dispatches")
    );
    projector.shutdown().await;
}

#[tokio::test]
async fn projector_emits_cancelled_then_finished_on_job_cancel() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let captured = install_event_capturer(&app, "j-cancel");
    let projector = JobProjector::spawn(
        app.handle().clone(),
        "default".into(),
        Arc::clone(&bus),
        pool.clone(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-cancel".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("emit");
    // Mid-stage dispatch so cancelled_during is non-default.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:scout",
        "dispatch",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-cancel".into(),
            target: "agent:scout".into(),
            prompt: "go".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit");
    drain_until(&captured, |evts| {
        evts.iter().any(|e| e.kind == "stage_started")
    })
    .await;
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:coordinator",
        "cancel",
        None,
        MailboxEvent::JobCancel {
            job_id: "j-cancel".into(),
        },
    )
    .await
    .expect("emit cancel");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:user",
        "finished",
        None,
        MailboxEvent::JobFinished {
            job_id: "j-cancel".into(),
            outcome: "failed".into(),
            summary: "cancelled by user".into(),
        },
    )
    .await
    .expect("emit");
    drain_until(&captured, |evts| {
        let has_cancel = evts.iter().any(|e| e.kind == "cancelled");
        let has_finished = evts.iter().any(|e| e.kind == "finished");
        has_cancel && has_finished
    })
    .await;
    let snap = captured.lock().expect("lock").clone();
    let cancel_idx = snap
        .iter()
        .position(|e| e.kind == "cancelled")
        .expect("Cancelled present");
    let finished_idx = snap
        .iter()
        .position(|e| e.kind == "finished")
        .expect("Finished present");
    assert!(
        cancel_idx < finished_idx,
        "Cancelled must precede Finished: kinds={:?}",
        snap.iter().map(|e| e.kind.clone()).collect::<Vec<_>>()
    );
    let cancel = &snap[cancel_idx];
    assert_eq!(
        cancel.json.get("cancelled_during").and_then(|v| v.as_str()),
        Some("scout")
    );
    projector.shutdown().await;
}

