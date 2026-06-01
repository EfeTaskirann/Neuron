//! Unit + integration tests for the projector. Moved verbatim from
//! the monolithic `projector.rs` `#[cfg(test)] mod tests` block; the
//! imports below replace the names the in-file `use super::*` used to
//! pull from the (now split) top-level module.

use super::helpers::{agent_id_to_job_state, is_retry_dispatch};
use crate::events;
use crate::swarm::coordinator::{Job, JobState};
use crate::swarm::mailbox_bus::MailboxEvent;

    use super::*;
    use crate::swarm::coordinator::Verdict;
    use crate::test_support::mock_app_with_pool;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tauri::Listener;

    /// One captured `swarm:job:{id}:event` payload. `kind` is the
    /// top-level `kind` tag from `SwarmJobEvent`'s
    /// `#[serde(tag = "kind")]`; `json` is the full payload Value
    /// so individual assertions can dig into per-variant fields.
    /// `SwarmJobEvent` is `Serialize`-only (no Deserialize), which
    /// is why we capture as `Value` rather than the typed enum.
    #[derive(Debug, Clone)]
    struct CapturedEvent {
        kind: String,
        json: serde_json::Value,
    }

    /// Capture every `swarm:job:{job_id}:event` payload in
    /// chronological order. Mirrors the FSM tests' `capture_events`
    /// helper but scoped to this module so we don't reach into
    /// `coordinator`.
    fn install_event_capturer<R: Runtime>(
        app: &tauri::App<R>,
        job_id: &str,
    ) -> Arc<StdMutex<Vec<CapturedEvent>>> {
        let captured: Arc<StdMutex<Vec<CapturedEvent>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_w = Arc::clone(&captured);
        app.listen(events::swarm_job_event(job_id), move |event| {
            let payload = event.payload().to_string();
            if let Ok(value) =
                serde_json::from_str::<serde_json::Value>(&payload)
            {
                let kind = value
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                captured_w
                    .lock()
                    .expect("lock")
                    .push(CapturedEvent { kind, json: value });
            }
        });
        captured
    }

    /// Wait until the captured events match a predicate, polling
    /// on 20 ms ticks. Bounded so a regression surfaces as a test
    /// failure rather than a hang.
    async fn drain_until<F: Fn(&[CapturedEvent]) -> bool>(
        captured: &Arc<StdMutex<Vec<CapturedEvent>>>,
        pred: F,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let snap = captured.lock().expect("lock").clone();
            if pred(&snap) {
                return;
            }
            if std::time::Instant::now() > deadline {
                let snap = captured.lock().expect("lock").clone();
                panic!(
                    "timeout waiting for predicate; captured kinds: {:?}",
                    snap.iter().map(|e| e.kind.clone()).collect::<Vec<_>>()
                );
            }
        }
    }

    /// Migration 0011 adds `source` column with default 'fsm';
    /// W3-shape Job JSON without `source` deserialises with
    /// `source == "fsm"` (gotcha #3).
    #[tokio::test]
    async fn migration_0011_round_trip() {
        let (_, pool, _dir) = mock_app_with_pool().await;
        // Direct INSERT of a row with no `source` column reference
        // → migration default applies.
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("j-mig-0011")
        .bind("ws")
        .bind("g")
        .bind(0_i64)
        .bind("done")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("seed");
        let source: String = sqlx::query_scalar(
            "SELECT source FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-mig-0011")
        .fetch_one(&pool)
        .await
        .expect("read source");
        assert_eq!(source, "fsm", "default backfill is 'fsm'");

        // Insert a row with explicit 'brain'.
        sqlx::query(
            "INSERT INTO swarm_jobs (id, workspace_id, goal, created_at_ms, state, retry_count, source) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("j-brain-0011")
        .bind("ws")
        .bind("g")
        .bind(0_i64)
        .bind("done")
        .bind(0_i64)
        .bind("brain")
        .execute(&pool)
        .await
        .expect("seed brain");
        let brain_source: String = sqlx::query_scalar(
            "SELECT source FROM swarm_jobs WHERE id = ?",
        )
        .bind("j-brain-0011")
        .fetch_one(&pool)
        .await
        .expect("read brain source");
        assert_eq!(brain_source, "brain");

        // Job struct deserialises from a W3-vintage JSON (no
        // `source` key) and surfaces 'fsm' via the serde default.
        let w3_shape = r#"{"id":"j-old","goal":"g","createdAtMs":0,"state":"init","retryCount":0,"stages":[],"lastError":null}"#;
        let job: Job = serde_json::from_str(w3_shape).expect("parse W3 JSON");
        assert_eq!(job.source, "fsm", "default_source=='fsm'");
    }

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
