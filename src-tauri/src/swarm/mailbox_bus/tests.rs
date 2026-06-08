//! Unit tests for the `mailbox_bus` package — migration round-trip,
//! `MailboxEvent` serde round-trip, `MailboxBus` subscribe / emit /
//! list surface, the WP-W5-05 workspace-busy guard, and the
//! shutdown cancel fan-out.
//!
//! Tests stayed in one module (rather than fanning out per
//! submodule) because they share the `sample_events` /
//! `seed_swarm_job` helpers and reach every type through the
//! package re-exports, so `use super::*` resolves the same as the
//! pre-split single-file version.

use super::*;
use crate::db::DbPool;
use crate::error::AppError;
use crate::models::MailboxEntry;
use crate::test_support::mock_app_with_pool;
use std::sync::{Arc, Mutex};
use tauri::Listener;

// -----------------------------------------------------------------
// 1. Migration round-trip
// -----------------------------------------------------------------

/// Acceptance: migration 0010 lands the three new columns with
/// defaults; existing-row backfill works; new rows can carry
/// non-default values.
#[tokio::test]
async fn migration_0010_round_trip() {
    let (_, pool, _dir) = mock_app_with_pool().await;

    // Insert a legacy-shape row (no kind / parent_id /
    // payload_json supplied). Defaults must apply.
    sqlx::query(
        "INSERT INTO mailbox (ts, from_pane, to_pane, type, summary) \
         VALUES (100, 'pane:p1', 'pane:p2', 'task:done', 'legacy')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (kind, parent_id, payload_json): (String, Option<i64>, String) =
        sqlx::query_as(
            "SELECT kind, parent_id, payload_json FROM mailbox WHERE summary='legacy'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind, "note");
    assert_eq!(parent_id, None);
    assert_eq!(payload_json, "{}");

    // Insert a new-shape row with non-default values.
    sqlx::query(
        "INSERT INTO mailbox \
           (ts, from_pane, to_pane, type, summary, kind, parent_id, payload_json) \
         VALUES (200, 'agent:scout', 'agent:planner', 'task_dispatch', \
                 'dispatched', 'task_dispatch', 1, '{\"kind\":\"task_dispatch\",\"job_id\":\"j-1\",\"target\":\"agent:scout\",\"prompt\":\"go\",\"with_help_loop\":true}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (kind2, parent2, payload2): (String, Option<i64>, String) =
        sqlx::query_as(
            "SELECT kind, parent_id, payload_json FROM mailbox WHERE summary='dispatched'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind2, "task_dispatch");
    assert_eq!(parent2, Some(1));
    assert!(payload2.contains("\"job_id\":\"j-1\""));
}

// -----------------------------------------------------------------
// 2. MailboxEvent round-trip
// -----------------------------------------------------------------

fn sample_events() -> Vec<MailboxEvent> {
    vec![
        MailboxEvent::TaskDispatch {
            job_id: "j-1".into(),
            target: "agent:scout".into(),
            prompt: "Investigate auth.rs".into(),
            with_help_loop: true,
        },
        MailboxEvent::AgentResult {
            job_id: "j-1".into(),
            agent_id: "scout".into(),
            assistant_text: "Found three matches.".into(),
            total_cost_usd: 0.012_5,
            turn_count: 3,
        },
        MailboxEvent::AgentHelpRequest {
            job_id: "j-1".into(),
            agent_id: "backend-builder".into(),
            reason: "Plan step ambiguous".into(),
            question: "Which struct field carries the user id?".into(),
        },
        MailboxEvent::CoordinatorHelpOutcome {
            job_id: "j-1".into(),
            target_agent_id: "backend-builder".into(),
            outcome_json: r#"{"action":"direct_answer","answer":"User.id"}"#.into(),
        },
        MailboxEvent::JobStarted {
            job_id: "j-1".into(),
            workspace_id: "default".into(),
            goal: "Refactor auth".into(),
        },
        MailboxEvent::JobFinished {
            job_id: "j-1".into(),
            outcome: "done".into(),
            summary: "All approved.".into(),
        },
        MailboxEvent::JobCancel {
            job_id: "j-1".into(),
        },
        MailboxEvent::Note,
    ]
}

#[test]
fn mailbox_event_kind_str_round_trip() {
    for event in sample_events() {
        let kind = event.kind_str();
        let payload_json = serde_json::to_string(&event).unwrap();
        let restored =
            MailboxEvent::from_row_parts(kind, &payload_json).unwrap();
        assert_eq!(restored, event, "round-trip drift on {kind}");
    }
}

#[test]
fn mailbox_event_from_row_parts_handles_each_variant() {
    // Eight variants — one fixture each. The kind_str_round_trip
    // test already covers serde-emitted JSON; this one
    // additionally checks hand-written JSON shapes that the
    // frontend might emit through the IPC.
    let cases: &[(&str, &str, fn(&MailboxEvent) -> bool)] = &[
        (
            "task_dispatch",
            r#"{"kind":"task_dispatch","job_id":"j-1","target":"agent:scout","prompt":"go","with_help_loop":false}"#,
            |e| matches!(e, MailboxEvent::TaskDispatch { with_help_loop: false, .. }),
        ),
        (
            "agent_result",
            r#"{"kind":"agent_result","job_id":"j-1","agent_id":"scout","assistant_text":"done","total_cost_usd":0.5,"turn_count":2}"#,
            |e| matches!(e, MailboxEvent::AgentResult { turn_count: 2, .. }),
        ),
        (
            "agent_help_request",
            r#"{"kind":"agent_help_request","job_id":"j-1","agent_id":"x","reason":"r","question":"q"}"#,
            |e| matches!(e, MailboxEvent::AgentHelpRequest { .. }),
        ),
        (
            "coordinator_help_outcome",
            r#"{"kind":"coordinator_help_outcome","job_id":"j-1","target_agent_id":"x","outcome_json":"{\"action\":\"direct_answer\",\"answer\":\"a\"}"}"#,
            |e| matches!(e, MailboxEvent::CoordinatorHelpOutcome { .. }),
        ),
        (
            "job_started",
            r#"{"kind":"job_started","job_id":"j-1","workspace_id":"default","goal":"g"}"#,
            |e| matches!(e, MailboxEvent::JobStarted { .. }),
        ),
        (
            "job_finished",
            r#"{"kind":"job_finished","job_id":"j-1","outcome":"done","summary":"s"}"#,
            |e| matches!(e, MailboxEvent::JobFinished { .. }),
        ),
        (
            "job_cancel",
            r#"{"kind":"job_cancel","job_id":"j-1"}"#,
            |e| matches!(e, MailboxEvent::JobCancel { .. }),
        ),
        ("note", r#"{}"#, |e| matches!(e, MailboxEvent::Note)),
    ];
    for (kind, payload, predicate) in cases {
        let event = MailboxEvent::from_row_parts(kind, payload)
            .expect(&format!("parse {kind}"));
        assert!(predicate(&event), "predicate failed for kind={kind}");
    }
}

#[test]
fn mailbox_event_from_row_parts_rejects_malformed_payload() {
    // 1) non-JSON garbage
    let err = MailboxEvent::from_row_parts(
        "task_dispatch",
        "not even close to json",
    )
    .unwrap_err();
    assert_eq!(err.kind(), "internal");

    // 2) JSON object missing required fields
    let err = MailboxEvent::from_row_parts(
        "task_dispatch",
        r#"{"kind":"task_dispatch","target":"x"}"#,
    )
    .unwrap_err();
    assert_eq!(err.kind(), "internal");

    // 3) JSON array (wrong shape entirely)
    let err = MailboxEvent::from_row_parts(
        "task_dispatch",
        r#"["task_dispatch"]"#,
    )
    .unwrap_err();
    assert_eq!(err.kind(), "internal");

    // 4) Unknown variant kind
    let err = MailboxEvent::from_row_parts(
        "totally_made_up",
        r#"{"kind":"totally_made_up"}"#,
    )
    .unwrap_err();
    assert_eq!(err.kind(), "internal");
}

#[test]
fn mailbox_event_from_row_parts_handles_empty_payload_with_kind() {
    // Legacy 'note' row with payload_json='{}' (the migration
    // 0010 default) should round-trip to MailboxEvent::Note via
    // the kind splice.
    let event =
        MailboxEvent::from_row_parts("note", "{}").unwrap();
    assert_eq!(event, MailboxEvent::Note);

    // Whitespace also handled.
    let event =
        MailboxEvent::from_row_parts("note", "  ").unwrap();
    assert_eq!(event, MailboxEvent::Note);
}

// -----------------------------------------------------------------
// 3. MailboxBus subscribe / channel lifecycle
// -----------------------------------------------------------------

#[tokio::test]
async fn mailbox_bus_subscribe_creates_channel_on_first_call() {
    let (_, pool, _dir) = mock_app_with_pool().await;
    let bus = MailboxBus::new(pool);
    assert_eq!(bus.channel_count().await, 0);
    let _rx = bus.subscribe("default").await;
    assert_eq!(bus.channel_count().await, 1);
    // Different workspace — separate channel.
    let _rx2 = bus.subscribe("other").await;
    assert_eq!(bus.channel_count().await, 2);
}

#[tokio::test]
async fn mailbox_bus_subscribe_shares_channel_across_calls() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = MailboxBus::new(pool);
    let mut rx1 = bus.subscribe("default").await;
    let mut rx2 = bus.subscribe("default").await;
    assert_eq!(bus.channel_count().await, 1);

    // One emit; both subscribers receive.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:scout",
        "agent:planner",
        "kicked off",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-1".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("emit");

    let env1 = rx1.recv().await.expect("rx1 recv");
    let env2 = rx2.recv().await.expect("rx2 recv");
    assert_eq!(env1.id, env2.id);
    assert!(matches!(env1.event, MailboxEvent::JobStarted { .. }));
}

// -----------------------------------------------------------------
// 4. emit_typed end-to-end
// -----------------------------------------------------------------

#[tokio::test]
async fn mailbox_bus_emit_persists_row() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = MailboxBus::new(pool.clone());
    let env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:planner",
            "summary text",
            Some(42),
            MailboxEvent::TaskDispatch {
                job_id: "j-1".into(),
                target: "agent:planner".into(),
                prompt: "do the thing".into(),
                with_help_loop: true,
            },
        )
        .await
        .expect("emit");

    let (kind, parent, payload, summary): (
        String,
        Option<i64>,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT kind, parent_id, payload_json, summary FROM mailbox WHERE rowid=?",
    )
    .bind(env.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kind, "task_dispatch");
    assert_eq!(parent, Some(42));
    assert_eq!(summary, "summary text");
    assert!(payload.contains("\"target\":\"agent:planner\""));
}

#[tokio::test]
async fn mailbox_bus_emit_broadcasts_envelope() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = MailboxBus::new(pool);
    let mut rx = bus.subscribe("default").await;

    bus.emit_typed(
        app.handle(),
        "default",
        "agent:scout",
        "agent:planner",
        "broadcasted",
        None,
        MailboxEvent::JobCancel {
            job_id: "j-1".into(),
        },
    )
    .await
    .expect("emit");

    let env = rx.recv().await.expect("rx recv");
    assert_eq!(env.from_pane, "agent:scout");
    assert!(matches!(env.event, MailboxEvent::JobCancel { .. }));
}

#[tokio::test]
async fn mailbox_bus_emit_swallows_broadcast_send_error_on_no_subscribers() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = MailboxBus::new(pool);
    // No subscribers — emit must succeed.
    let result = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:planner",
            "no listeners",
            None,
            MailboxEvent::Note,
        )
        .await;
    assert!(result.is_ok(), "emit failed without subscribers: {result:?}");
}

#[tokio::test]
async fn mailbox_bus_emit_fires_legacy_mailbox_new_event() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let captured: Arc<Mutex<Option<String>>> =
        Arc::new(Mutex::new(None));
    let captured_w = Arc::clone(&captured);
    app.listen("mailbox:new", move |event| {
        *captured_w.lock().unwrap() = Some(event.payload().to_string());
    });

    let bus = MailboxBus::new(pool);
    let env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:planner",
            "back-compat",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-1".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");

    // Drive runtime briefly so the listener picks up the event.
    tokio::task::yield_now().await;

    let payload = captured
        .lock()
        .unwrap()
        .clone()
        .expect("legacy mailbox:new event was not delivered");
    let parsed: MailboxEntry = serde_json::from_str(&payload)
        .expect("parse legacy MailboxEntry");
    assert_eq!(parsed.id, env.id);
    assert_eq!(parsed.from_pane, "agent:scout");
    assert_eq!(parsed.entry_type, "job_started");
}

#[tokio::test]
async fn mailbox_bus_emit_validates_inputs() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = MailboxBus::new(pool);

    let err = bus
        .emit_typed(
            app.handle(),
            "",
            "agent:scout",
            "agent:planner",
            "",
            None,
            MailboxEvent::Note,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");

    let err = bus
        .emit_typed(
            app.handle(),
            "default",
            "",
            "agent:planner",
            "",
            None,
            MailboxEvent::Note,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");

    let err = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "",
            "",
            None,
            MailboxEvent::Note,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");
}

// -----------------------------------------------------------------
// 5. list_typed
// -----------------------------------------------------------------

#[tokio::test]
async fn mailbox_list_typed_filters_by_kind() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = MailboxBus::new(pool);

    // Mix of kinds.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:scout",
        "agent:planner",
        "started",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-1".into(),
            workspace_id: "default".into(),
            goal: "g".into(),
        },
    )
    .await
    .unwrap();
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:planner",
        "agent:builder",
        "dispatched",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-1".into(),
            target: "agent:builder".into(),
            prompt: "build".into(),
            with_help_loop: true,
        },
    )
    .await
    .unwrap();
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:builder",
        "agent:planner",
        "result",
        None,
        MailboxEvent::AgentResult {
            job_id: "j-1".into(),
            agent_id: "builder".into(),
            assistant_text: "done".into(),
            total_cost_usd: 0.01,
            turn_count: 1,
        },
    )
    .await
    .unwrap();

    let dispatches =
        bus.list_typed(Some("task_dispatch"), None, None).await.unwrap();
    assert_eq!(dispatches.len(), 1);
    assert!(matches!(dispatches[0].event, MailboxEvent::TaskDispatch { .. }));

    let all = bus.list_typed(None, None, None).await.unwrap();
    assert_eq!(all.len(), 3);
    // Oldest-first ordering: job_started < task_dispatch < agent_result.
    assert!(matches!(all[0].event, MailboxEvent::JobStarted { .. }));
    assert!(matches!(all[2].event, MailboxEvent::AgentResult { .. }));
}

#[tokio::test]
async fn mailbox_list_typed_paginates_by_since_id() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = MailboxBus::new(pool);

    let env1 = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:a",
            "agent:b",
            "1",
            None,
            MailboxEvent::Note,
        )
        .await
        .unwrap();
    let _env2 = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:a",
            "agent:b",
            "2",
            None,
            MailboxEvent::Note,
        )
        .await
        .unwrap();

    let after_first =
        bus.list_typed(None, Some(env1.id), None).await.unwrap();
    assert_eq!(after_first.len(), 1);
    assert_eq!(after_first[0].summary, "2");
}

// -----------------------------------------------------------------
// 6. WP-W5-05 — workspace-busy guard for JobStarted
// -----------------------------------------------------------------

/// Test helper: seed one `swarm_jobs` row with the supplied
/// shape. Mirrors `swarm::projector::persist_job_init` minus
/// the duplicate-detect branch — tests want a deterministic
/// fixture, not idempotent insert behavior.
async fn seed_swarm_job(
    pool: &DbPool,
    id: &str,
    workspace_id: &str,
    state: &str,
    source: &str,
) {
    sqlx::query(
        "INSERT INTO swarm_jobs \
         (id, workspace_id, goal, created_at_ms, state, retry_count, last_error, finished_at_ms, last_verdict_json, source) \
         VALUES (?, ?, 'g', 0, ?, 0, NULL, NULL, NULL, ?)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(state)
    .bind(source)
    .execute(pool)
    .await
    .expect("seed swarm_jobs row");
}

/// Acceptance: `emit_typed(JobStarted)` against a workspace that
/// already has a brain-driven, non-terminal `swarm_jobs` row
/// surfaces `WorkspaceBusy` with the in-flight job's id —
/// without inserting a mailbox row.
#[tokio::test]
async fn emit_typed_rejects_concurrent_job_started_for_same_workspace() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_swarm_job(&pool, "j-existing", "ws-1", "scout", "brain").await;
    let bus = MailboxBus::new(pool.clone());

    let err = bus
        .emit_typed(
            app.handle(),
            "ws-1",
            "agent:user",
            "agent:coordinator",
            "second job",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-second".into(),
                workspace_id: "ws-1".into(),
                goal: "g2".into(),
            },
        )
        .await
        .expect_err("rejected");
    assert_eq!(err.kind(), "workspace_busy");
    match err {
        AppError::WorkspaceBusy {
            workspace_id,
            in_flight_job_id,
        } => {
            assert_eq!(workspace_id, "ws-1");
            assert_eq!(in_flight_job_id, "j-existing");
        }
        other => panic!("expected WorkspaceBusy; got {other:?}"),
    }

    // No mailbox row landed for the rejected JobStarted.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mailbox WHERE kind = 'job_started'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// Acceptance: once the previous brain job is terminal
/// (`done` / `failed`), the bus accepts a fresh JobStarted for
/// the same workspace.
#[tokio::test]
async fn emit_typed_allows_job_started_after_previous_finished() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    // Two prior jobs in the same workspace; both terminal.
    seed_swarm_job(&pool, "j-done", "ws-1", "done", "brain").await;
    seed_swarm_job(&pool, "j-failed", "ws-1", "failed", "brain").await;
    let bus = MailboxBus::new(pool);

    bus.emit_typed(
        app.handle(),
        "ws-1",
        "agent:user",
        "agent:coordinator",
        "fresh job",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-fresh".into(),
            workspace_id: "ws-1".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("JobStarted accepted after previous finished");
}

/// Acceptance: brain jobs in different workspaces run in
/// parallel — the guard scopes per workspace_id.
#[tokio::test]
async fn emit_typed_allows_concurrent_job_started_for_different_workspaces() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_swarm_job(&pool, "j-ws-a", "ws-a", "build", "brain").await;
    let bus = MailboxBus::new(pool);

    // Different workspace must be accepted even though ws-a is
    // busy.
    bus.emit_typed(
        app.handle(),
        "ws-b",
        "agent:user",
        "agent:coordinator",
        "ws-b job",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-ws-b".into(),
            workspace_id: "ws-b".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("ws-b JobStarted accepted while ws-a is busy");

    // Sanity: ws-a is still busy.
    let err = bus
        .emit_typed(
            app.handle(),
            "ws-a",
            "agent:user",
            "agent:coordinator",
            "ws-a second",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-ws-a-2".into(),
                workspace_id: "ws-a".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect_err("ws-a still busy");
    assert_eq!(err.kind(), "workspace_busy");
}

/// Acceptance: an `fsm`-source row in flight does NOT block a
/// brain JobStarted for the same workspace. The two paths
/// coexist until W5-06 deletes the FSM; the bus only gates on
/// brain jobs (W5-06 collapses both into the brain path).
#[tokio::test]
async fn emit_typed_ignores_fsm_source_jobs_for_busy_check() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_swarm_job(&pool, "j-fsm", "ws-1", "scout", "fsm").await;
    let bus = MailboxBus::new(pool);

    bus.emit_typed(
        app.handle(),
        "ws-1",
        "agent:user",
        "agent:coordinator",
        "brain job sharing workspace with fsm job",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-brain".into(),
            workspace_id: "ws-1".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("brain JobStarted accepted while fsm job in flight");
}

// -----------------------------------------------------------------
// 7. WP-W5-05 — shutdown cancel fan-out
// -----------------------------------------------------------------

/// Acceptance: `cancel_in_flight_brain_jobs` emits one
/// `MailboxEvent::JobCancel` for every brain-driven, non-terminal
/// `swarm_jobs` row — and skips terminal jobs and FSM-source
/// jobs. Mirrors the body of the `RunEvent::ExitRequested`
/// shutdown hook in `lib.rs::run` so the WP-W5-05 step-3
/// invariant is unit-testable without booting the runtime
/// closure.
#[tokio::test]
async fn shutdown_emits_job_cancel_for_each_in_flight_brain_job() {
    let (app, pool, _dir) = mock_app_with_pool().await;

    // Two in-flight brain jobs across two workspaces…
    seed_swarm_job(&pool, "j-brain-a", "ws-a", "scout", "brain").await;
    seed_swarm_job(&pool, "j-brain-b", "ws-b", "build", "brain").await;
    // …one terminal brain job (must be skipped)…
    seed_swarm_job(&pool, "j-brain-done", "ws-a", "done", "brain").await;
    // …one in-flight FSM-source job (must be skipped — the
    // brain path doesn't drive the FSM's cancel notify).
    seed_swarm_job(&pool, "j-fsm-live", "ws-c", "scout", "fsm").await;

    let bus = MailboxBus::new(pool.clone());

    // Subscribe to each workspace BEFORE the fan-out so the
    // broadcast lands on a live receiver (not strictly required
    // — the SQL log is the source of truth — but it lets us
    // assert per-workspace routing too).
    let mut rx_a = bus.subscribe("ws-a").await;
    let mut rx_b = bus.subscribe("ws-b").await;
    let mut rx_c = bus.subscribe("ws-c").await;

    let emitted =
        bus.cancel_in_flight_brain_jobs(app.handle()).await;
    assert_eq!(emitted, 2, "exactly two in-flight brain jobs cancel");

    // ws-a must receive a JobCancel for j-brain-a.
    let env_a = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rx_a.recv(),
    )
    .await
    .expect("ws-a recv within 1s")
    .expect("ws-a envelope");
    match env_a.event {
        MailboxEvent::JobCancel { job_id } => {
            assert_eq!(job_id, "j-brain-a");
        }
        other => panic!("ws-a expected JobCancel; got {other:?}"),
    }

    // ws-b must receive a JobCancel for j-brain-b.
    let env_b = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rx_b.recv(),
    )
    .await
    .expect("ws-b recv within 1s")
    .expect("ws-b envelope");
    match env_b.event {
        MailboxEvent::JobCancel { job_id } => {
            assert_eq!(job_id, "j-brain-b");
        }
        other => panic!("ws-b expected JobCancel; got {other:?}"),
    }

    // ws-c must NOT receive any cancel (FSM-source job skipped).
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        rx_c.recv(),
    )
    .await;
    assert!(
        result.is_err(),
        "ws-c (FSM-source) must not receive a JobCancel; got: {result:?}"
    );

    // Persisted rows: exactly two `kind='job_cancel'` rows in
    // mailbox.
    let cancel_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mailbox WHERE kind = 'job_cancel'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cancel_count, 2);
}
