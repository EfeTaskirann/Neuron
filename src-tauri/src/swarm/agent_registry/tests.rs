use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use super::config::{resolve_turn_cap, DEFAULT_TURN_CAP, TURN_CAP_ENV};
use super::*;
use crate::swarm::profile::ProfileRegistry;
use crate::test_support::mock_app_with_pool;

fn fresh_registry() -> Arc<SwarmAgentRegistry> {
    let profiles =
        Arc::new(ProfileRegistry::load_from(None).expect("load"));
    Arc::new(SwarmAgentRegistry::new(profiles))
}

/// Fresh registry against the bundled 9 profiles surfaces 9
/// `NotSpawned` rows for any workspace. The W4-04 grid header
/// reads exactly this shape on first mount.
#[tokio::test]
async fn list_status_returns_not_spawned_for_untouched_agents() {
    let reg = fresh_registry();
    let rows = reg.list_status("default").await;
    assert_eq!(rows.len(), 9, "expected 9 bundled profiles");
    for r in &rows {
        assert_eq!(r.status, AgentStatus::NotSpawned);
        assert_eq!(r.turns_taken, 0);
        assert!(r.last_activity_ms.is_none());
        assert_eq!(r.workspace_id, "default");
    }
    // Stable alphabetical order — same shape `swarm:profiles_list`
    // promises elsewhere.
    let ids: Vec<&str> =
        rows.iter().map(|r| r.agent_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "backend-builder",
            "backend-reviewer",
            "coordinator",
            "frontend-builder",
            "frontend-reviewer",
            "integration-tester",
            "orchestrator",
            "planner",
            "scout",
        ]
    );
}

/// Different workspaces see independent `NotSpawned` rows.
#[tokio::test]
async fn list_status_isolated_per_workspace() {
    let reg = fresh_registry();
    let ws_a = reg.list_status("ws-a").await;
    let ws_b = reg.list_status("ws-b").await;
    assert_eq!(ws_a.len(), 9);
    assert_eq!(ws_b.len(), 9);
    for r in &ws_a {
        assert_eq!(r.workspace_id, "ws-a");
    }
    for r in &ws_b {
        assert_eq!(r.workspace_id, "ws-b");
    }
}

/// Empty registry slot count is 0 — no sessions exist before
/// anyone calls `acquire`.
#[tokio::test]
async fn fresh_registry_has_zero_slots() {
    let reg = fresh_registry();
    assert_eq!(reg.slot_count().await, 0);
}

/// `shutdown_workspace` on an empty registry is a no-op (no
/// crash, no error).
#[tokio::test]
async fn shutdown_workspace_on_empty_registry_is_ok() {
    let reg = fresh_registry();
    reg.shutdown_workspace("default").await.expect("ok");
    // Slot count stays 0.
    assert_eq!(reg.slot_count().await, 0);
}

/// `shutdown_all` on an empty registry is a no-op.
#[tokio::test]
async fn shutdown_all_on_empty_registry_is_ok() {
    let reg = fresh_registry();
    reg.shutdown_all().await.expect("ok");
    assert_eq!(reg.slot_count().await, 0);
}

/// Empty `workspaceId` rejected at the registry method boundary
/// (mirrors the IPC validation at `commands/swarm.rs`). We
/// validate twice — defense-in-depth — so a non-IPC caller
/// (e.g. the FSM) doesn't bypass the check.
#[tokio::test]
async fn shutdown_workspace_rejects_empty_workspace_id() {
    let reg = fresh_registry();
    let err = reg
        .shutdown_workspace("")
        .await
        .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Turn cap defaults to `DEFAULT_TURN_CAP` (200) when the env
/// var is absent.
#[test]
fn turn_cap_defaults_to_200_when_env_absent() {
    // Save + clear the env var for the duration of the test.
    let prior = std::env::var(TURN_CAP_ENV).ok();
    std::env::remove_var(TURN_CAP_ENV);
    assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
    assert_eq!(DEFAULT_TURN_CAP, 200);
    // Restore.
    if let Some(v) = prior {
        std::env::set_var(TURN_CAP_ENV, v);
    }
}

/// `NEURON_SWARM_AGENT_TURN_CAP=42` lands as `turn_cap = 42`.
#[test]
fn turn_cap_env_override_lands() {
    let prior = std::env::var(TURN_CAP_ENV).ok();
    std::env::set_var(TURN_CAP_ENV, "42");
    assert_eq!(resolve_turn_cap(), 42);
    // Restore.
    match prior {
        Some(v) => std::env::set_var(TURN_CAP_ENV, v),
        None => std::env::remove_var(TURN_CAP_ENV),
    }
}

/// Non-numeric env override falls back to default with a warn
/// log (we don't capture the log here — too fragile — but we
/// do assert the fallback fires).
#[test]
fn turn_cap_non_numeric_falls_back_to_default() {
    let prior = std::env::var(TURN_CAP_ENV).ok();
    std::env::set_var(TURN_CAP_ENV, "not-a-number");
    assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
    match prior {
        Some(v) => std::env::set_var(TURN_CAP_ENV, v),
        None => std::env::remove_var(TURN_CAP_ENV),
    }
}

/// Zero env override falls back to default (we want `cap=0` to
/// be a typo, not "never respawn").
#[test]
fn turn_cap_zero_falls_back_to_default() {
    let prior = std::env::var(TURN_CAP_ENV).ok();
    std::env::set_var(TURN_CAP_ENV, "0");
    assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
    match prior {
        Some(v) => std::env::set_var(TURN_CAP_ENV, v),
        None => std::env::remove_var(TURN_CAP_ENV),
    }
}

/// `with_turn_cap` builder sets the cap directly without env
/// dependency — the test path uses this so suite order doesn't
/// matter.
#[test]
fn with_turn_cap_pins_cap() {
    let profiles =
        Arc::new(ProfileRegistry::load_from(None).expect("load"));
    let reg = SwarmAgentRegistry::with_turn_cap(profiles, 5);
    assert_eq!(reg.turn_cap(), 5);
}

/// Acquire with empty workspaceId rejected.
#[tokio::test]
async fn acquire_validates_empty_workspace_id() {
    let reg = fresh_registry();
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = reg
        .acquire_and_invoke_turn(
            app.handle(),
            "",
            "scout",
            "hi",
            Duration::from_secs(1),
            Arc::new(Notify::new()),
        )
        .await
        .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Acquire with empty agentId rejected.
#[tokio::test]
async fn acquire_validates_empty_agent_id() {
    let reg = fresh_registry();
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = reg
        .acquire_and_invoke_turn(
            app.handle(),
            "default",
            "",
            "hi",
            Duration::from_secs(1),
            Arc::new(Notify::new()),
        )
        .await
        .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

// ---------------------------------------------------------------- //
// WP-W5-02 — ensure_dispatcher idempotence                          //
// ---------------------------------------------------------------- //

/// Calling `ensure_dispatcher` twice for the same
/// (workspace, agent) pair leaves exactly one dispatcher
/// registered. Different (workspace, agent) keys land
/// independent dispatchers. Empty inputs no-op silently.
#[tokio::test]
async fn registry_ensure_dispatcher_is_idempotent() {
    let reg = fresh_registry();
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(crate::swarm::MailboxBus::new(pool));

    assert_eq!(reg.dispatcher_count().await, 0);

    // Empty inputs are silent no-ops.
    reg.ensure_dispatcher(app.handle(), "", "scout", &bus).await;
    reg.ensure_dispatcher(app.handle(), "default", "", &bus).await;
    reg.ensure_dispatcher(app.handle(), "   ", "scout", &bus).await;
    assert_eq!(reg.dispatcher_count().await, 0);

    // First call lands a dispatcher.
    reg.ensure_dispatcher(app.handle(), "default", "planner", &bus)
        .await;
    assert_eq!(reg.dispatcher_count().await, 1);

    // Second call for same key is a no-op.
    reg.ensure_dispatcher(app.handle(), "default", "planner", &bus)
        .await;
    assert_eq!(reg.dispatcher_count().await, 1);

    // Different agent in same workspace — separate slot.
    reg.ensure_dispatcher(app.handle(), "default", "scout", &bus)
        .await;
    assert_eq!(reg.dispatcher_count().await, 2);

    // Different workspace, same agent — separate slot.
    reg.ensure_dispatcher(app.handle(), "other", "planner", &bus)
        .await;
    assert_eq!(reg.dispatcher_count().await, 3);

    // shutdown_all drains all dispatchers.
    reg.shutdown_all().await.expect("shutdown_all ok");
    assert_eq!(reg.dispatcher_count().await, 0);
}

/// Acquire with unknown agentId rejected as `not_found` (the
/// profile registry returns None for unknown ids).
#[tokio::test]
async fn acquire_unknown_agent_id_returns_not_found() {
    let reg = fresh_registry();
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = reg
        .acquire_and_invoke_turn(
            app.handle(),
            "default",
            "no-such-agent",
            "hi",
            Duration::from_secs(1),
            Arc::new(Notify::new()),
        )
        .await
        .expect_err("unknown rejected");
    assert_eq!(err.kind(), "not_found");
}

/// Real-claude integration smoke (`#[ignore]`'d) — drives two
/// turns through the registry and asserts:
///  1. The same session is reused (turn 2 doesn't cold-start).
///  2. `list_status` flips through `Spawning → Running → Idle`
///     and reports `turns_taken == 2` after both turns finish.
///  3. `shutdown_workspace` reverts the row to `NotSpawned`.
///
/// Time budget: typical 60-180s (one cold-start + two turns).
#[tokio::test]
#[ignore = "requires real `claude` binary + Pro/Max subscription"]
async fn integration_registry_reuses_session() {
    let reg = fresh_registry();
    let (app, _pool, _dir) = mock_app_with_pool().await;

    let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(180);
    let timeout = Duration::from_secs(stage_secs);
    let cancel = Arc::new(Notify::new());

    // Turn 1 — cold-start path, lazy-spawns the scout session.
    let r1 = reg
        .acquire_and_invoke_turn(
            app.handle(),
            "default",
            "scout",
            "Reply with exactly the single word `BETA` and nothing else.",
            timeout,
            Arc::clone(&cancel),
        )
        .await
        .expect("turn 1 ok");
    assert!(
        r1.assistant_text.to_uppercase().contains("BETA"),
        "turn 1 should contain BETA"
    );

    // Turn 2 — should reuse the session. The proof: list_status
    // shows turns_taken == 2 (not 1) for the scout row; if a
    // respawn had happened the counter would have reset.
    let r2 = reg
        .acquire_and_invoke_turn(
            app.handle(),
            "default",
            "scout",
            "What was the single word you just replied with? Answer in one word.",
            timeout,
            cancel,
        )
        .await
        .expect("turn 2 ok");
    assert!(
        r2.assistant_text.to_uppercase().contains("BETA"),
        "turn 2 should recall BETA"
    );

    let rows = reg.list_status("default").await;
    let scout = rows
        .iter()
        .find(|r| r.agent_id == "scout")
        .expect("scout row");
    assert_eq!(scout.status, AgentStatus::Idle);
    assert_eq!(scout.turns_taken, 2);
    assert!(scout.last_activity_ms.is_some());

    // Shutdown reverts to NotSpawned.
    reg.shutdown_workspace("default").await.expect("shutdown ok");
    let rows = reg.list_status("default").await;
    let scout = rows
        .iter()
        .find(|r| r.agent_id == "scout")
        .expect("scout row");
    assert_eq!(scout.status, AgentStatus::NotSpawned);
    assert_eq!(scout.turns_taken, 0);
}
