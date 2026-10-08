//! `swarm:agents:dispatch_to_agent` coverage (WP-W5-02).

use tauri::Manager;

use crate::commands::swarm::swarm_agents_dispatch_to_agent;
use crate::swarm::{ProfileRegistry, SwarmAgentRegistry};

/// Build a mock app with both `MailboxBus` and `SwarmAgentRegistry`
/// in state so the W5-02 IPC tests don't repeat the wiring three
/// times.
async fn mock_app_with_w5_state() -> (
    tauri::App<tauri::test::MockRuntime>,
    std::sync::Arc<crate::swarm::MailboxBus>,
    std::sync::Arc<SwarmAgentRegistry>,
    crate::db::DbPool,
    tempfile::TempDir,
) {
    let (pool, dir) = crate::test_support::fresh_pool().await;
    let bus = std::sync::Arc::new(
        crate::swarm::MailboxBus::new(pool.clone()),
    );
    let registry = std::sync::Arc::new(SwarmAgentRegistry::new(
        std::sync::Arc::new(
            ProfileRegistry::load_from(None).expect("load"),
        ),
    ));
    let app = tauri::test::mock_builder()
        .manage(pool.clone())
        .manage(bus.clone())
        .manage(registry.clone())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app");
    (app, bus, registry, pool, dir)
}

/// Acceptance: empty inputs surface `InvalidInput` BEFORE
/// touching state. Mirrors the validation pattern of every
/// other swarm IPC.
#[tokio::test]
async fn swarm_agents_dispatch_to_agent_validates_inputs() {
    let (app, _bus, _reg, _pool, _dir) = mock_app_with_w5_state().await;
    let bus_state = app.state::<std::sync::Arc<crate::swarm::MailboxBus>>();
    let registry_state =
        app.state::<std::sync::Arc<SwarmAgentRegistry>>();

    // Empty workspace_id.
    let err = swarm_agents_dispatch_to_agent(
        app.handle().clone(),
        bus_state.clone(),
        registry_state.clone(),
        "".into(),
        "scout".into(),
        "do something".into(),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");

    // Empty agent_id.
    let err = swarm_agents_dispatch_to_agent(
        app.handle().clone(),
        bus_state.clone(),
        registry_state.clone(),
        "default".into(),
        "".into(),
        "do something".into(),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");

    // Whitespace-only agent_id.
    let err = swarm_agents_dispatch_to_agent(
        app.handle().clone(),
        bus_state.clone(),
        registry_state.clone(),
        "default".into(),
        "   ".into(),
        "do something".into(),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");

    // Empty prompt.
    let err = swarm_agents_dispatch_to_agent(
        app.handle().clone(),
        bus_state,
        registry_state,
        "default".into(),
        "scout".into(),
        "".into(),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");
}

/// Acceptance: a successful call lands a `task_dispatch` row in
/// the mailbox + ensures a dispatcher exists in the registry.
///
/// We dispatch to a *non-bundled* agent id so the dispatcher's
/// downstream `acquire_and_invoke_turn` returns `NotFound`
/// quickly (no real `claude` spawn) and the dispatcher's error
/// path emits an `error:` agent_result. That way the test
/// fully exercises the IPC + emit surface without a 60s claude
/// spawn timing out.
#[tokio::test]
async fn swarm_agents_dispatch_to_agent_emits_dispatch_event() {
    let (app, bus, registry, _pool, _dir) =
        mock_app_with_w5_state().await;
    let bus_state = app.state::<std::sync::Arc<crate::swarm::MailboxBus>>();
    let registry_state =
        app.state::<std::sync::Arc<SwarmAgentRegistry>>();

    // Pre-state: no dispatchers, no dispatch rows.
    assert_eq!(registry.dispatcher_count().await, 0);
    let pre =
        bus.list_typed(Some("task_dispatch"), None, None).await.unwrap();
    assert!(pre.is_empty());

    let id = swarm_agents_dispatch_to_agent(
        app.handle().clone(),
        bus_state,
        registry_state,
        "default".into(),
        "test-not-bundled".into(),
        "Investigate auth.rs callsites".into(),
        Some("j-test-1".into()),
        Some(true),
    )
    .await
    .expect("dispatch ok");

    // 1. Dispatcher landed for (default, test-not-bundled).
    assert_eq!(registry.dispatcher_count().await, 1);

    // 2. Mailbox has the task_dispatch row.
    let rows =
        bus.list_typed(Some("task_dispatch"), None, None).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.id, id);
    assert_eq!(row.from_pane, "agent:coordinator");
    assert_eq!(row.to_pane, "agent:test-not-bundled");
    match &row.event {
        crate::swarm::MailboxEvent::TaskDispatch {
            job_id,
            target,
            prompt,
            with_help_loop,
        } => {
            assert_eq!(job_id, "j-test-1");
            assert_eq!(target, "agent:test-not-bundled");
            assert_eq!(prompt, "Investigate auth.rs callsites");
            assert!(*with_help_loop);
        }
        _ => panic!("unexpected event kind"),
    }

    // 3. The dispatcher's invoke task fails fast with NotFound
    //    (the agent isn't in the bundled profile registry) and
    //    emits an error AgentResult with parent_id chained
    //    back to the dispatch row. This proves the error path
    //    end-to-end without needing a real `claude` subprocess.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(5);
    let result = loop {
        let rows = bus
            .list_typed(Some("agent_result"), None, None)
            .await
            .unwrap();
        if let Some(row) = rows.into_iter().find(|r| r.parent_id == Some(id)) {
            break row;
        }
        if std::time::Instant::now() > deadline {
            panic!("error AgentResult never arrived");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20))
            .await;
    };
    match &result.event {
        crate::swarm::MailboxEvent::AgentResult {
            assistant_text,
            ..
        } => {
            assert!(
                assistant_text.starts_with("error:"),
                "expected error result for unknown agent: {assistant_text}"
            );
        }
        _ => panic!("unexpected event kind"),
    }

    // Cleanup — drain the dispatcher so the test exits without
    // leaving its background task in flight.
    registry.shutdown_all().await.expect("shutdown ok");
}
