//! `swarm:agents:list_status` / `swarm:agents:shutdown_workspace`
//! coverage (WP-W4-02).

use tauri::Manager;

use crate::commands::swarm::{
    swarm_agents_list_status, swarm_agents_shutdown_workspace,
};
use crate::swarm::ProfileRegistry;
use crate::test_support::mock_app_with_pool;

/// Empty `workspace_id` short-circuits with `InvalidInput` before
/// touching the registry.
#[tokio::test]
async fn swarm_agents_list_status_validates_empty_workspace_id() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = std::sync::Arc::new(
        crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
            ProfileRegistry::load_from(None).expect("load"),
        )),
    );
    app.manage(registry);
    let err =
        swarm_agents_list_status(app.handle().clone(), "".into())
            .await
            .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Whitespace-only `workspace_id` rejected — same gate as the
/// other swarm IPCs.
#[tokio::test]
async fn swarm_agents_list_status_rejects_whitespace_workspace_id() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = std::sync::Arc::new(
        crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
            ProfileRegistry::load_from(None).expect("load"),
        )),
    );
    app.manage(registry);
    let err =
        swarm_agents_list_status(app.handle().clone(), "   ".into())
            .await
            .expect_err("whitespace rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Missing registry state surfaces `Internal` — defensive path.
#[tokio::test]
async fn swarm_agents_list_status_without_registry_returns_internal() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    // Intentionally do NOT manage(SwarmAgentRegistry).
    let err = swarm_agents_list_status(
        app.handle().clone(),
        "default".into(),
    )
    .await
    .expect_err("missing registry");
    assert_eq!(err.kind(), "internal");
}

/// Happy path — fresh registry returns 9 `NotSpawned` rows
/// alphabetically. Same shape `swarm:profiles_list` promises.
#[tokio::test]
async fn swarm_agents_list_status_returns_not_spawned_for_fresh_workspace() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = std::sync::Arc::new(
        crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
            ProfileRegistry::load_from(None).expect("load"),
        )),
    );
    app.manage(registry);
    let rows = swarm_agents_list_status(
        app.handle().clone(),
        "default".into(),
    )
    .await
    .expect("ok");
    assert_eq!(rows.len(), 9);
    for r in &rows {
        assert_eq!(
            r.status,
            crate::swarm::AgentStatus::NotSpawned
        );
        assert_eq!(r.turns_taken, 0);
        assert!(r.last_activity_ms.is_none());
    }
}

/// `shutdown_workspace` empty workspaceId rejected.
#[tokio::test]
async fn swarm_agents_shutdown_workspace_validates_empty_workspace_id() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = std::sync::Arc::new(
        crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
            ProfileRegistry::load_from(None).expect("load"),
        )),
    );
    app.manage(registry);
    let err = swarm_agents_shutdown_workspace(
        app.handle().clone(),
        "".into(),
    )
    .await
    .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// `shutdown_workspace` is idempotent — calling on an empty
/// workspace returns `Ok(())`.
#[tokio::test]
async fn swarm_agents_shutdown_workspace_idempotent_on_empty_registry() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = std::sync::Arc::new(
        crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
            ProfileRegistry::load_from(None).expect("load"),
        )),
    );
    app.manage(registry);
    swarm_agents_shutdown_workspace(
        app.handle().clone(),
        "default".into(),
    )
    .await
    .expect("ok");
}
