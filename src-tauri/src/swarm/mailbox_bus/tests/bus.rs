//! `MailboxBus` runtime surface: per-workspace subscribe / channel
//! lifecycle, `emit_typed` (persist + broadcast + legacy
//! `mailbox:new` fan-out + input validation), and `list_typed`
//! filtering / pagination.

use std::sync::{Arc, Mutex};

use tauri::Listener;

use crate::models::MailboxEntry;
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEvent};
use crate::test_support::mock_app_with_pool;

// -----------------------------------------------------------------
// MailboxBus subscribe / channel lifecycle
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
// emit_typed end-to-end
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
// list_typed
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
