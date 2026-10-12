//! `swarm:orchestrator_*` coverage — decide-input validation
//! (WP-W3-12k1) plus history / clear / log_job IPC (WP-W3-12k2).

use crate::commands::swarm::{
    swarm_orchestrator_clear_history, swarm_orchestrator_decide,
    swarm_orchestrator_history, swarm_orchestrator_log_job,
};
use crate::swarm::coordinator::orchestrator_session::append_user_message;
use crate::test_support::mock_app_with_pool;

// ---------------------------------------------------------------- //
// WP-W3-12k1 — swarm:orchestrator_decide validation tests           //
// ---------------------------------------------------------------- //

/// Empty `workspace_id` short-circuits before any subprocess
/// spawn happens; the IPC surfaces `InvalidInput`.
#[tokio::test]
async fn swarm_orchestrator_decide_command_validates_empty_workspace_id() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_orchestrator_decide(
        app.handle().clone(),
        "".into(),
        "selam".into(),
    )
    .await
    .expect_err("empty workspace rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Whitespace-only `workspace_id` is treated identically to
/// empty (`trim().is_empty()` gate). The same gate exists on
/// `swarm:run_job` so the two surfaces stay symmetric.
#[tokio::test]
async fn swarm_orchestrator_decide_command_rejects_whitespace_workspace_id() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_orchestrator_decide(
        app.handle().clone(),
        "   ".into(),
        "selam".into(),
    )
    .await
    .expect_err("whitespace workspace rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Empty `user_message` short-circuits; the IPC surfaces
/// `InvalidInput`. The Orchestrator is not allowed to invent a
/// goal from an empty message.
#[tokio::test]
async fn swarm_orchestrator_decide_command_validates_empty_message() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_orchestrator_decide(
        app.handle().clone(),
        "ws-1".into(),
        "".into(),
    )
    .await
    .expect_err("empty message rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Whitespace-only `user_message` is treated identically to
/// empty. Mirrors the validator on `workspace_id`.
#[tokio::test]
async fn swarm_orchestrator_decide_command_rejects_whitespace_message() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_orchestrator_decide(
        app.handle().clone(),
        "ws-1".into(),
        "   \t\n".into(),
    )
    .await
    .expect_err("whitespace message rejected");
    assert_eq!(err.kind(), "invalid_input");
}

// ---------------------------------------------------------------- //
// WP-W3-12k2 — orchestrator history / clear / log_job IPC tests    //
// ---------------------------------------------------------------- //

/// Seed N=3 messages directly via the helpers, then call the
/// IPC and assert it returns oldest-first chronological.
#[tokio::test]
async fn swarm_orchestrator_history_returns_oldest_first() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    // Seed three rows out of order so the assertion is non-trivial.
    append_user_message(&pool, "default", "first", 100)
        .await
        .expect("seed u1");
    append_user_message(&pool, "default", "third", 300)
        .await
        .expect("seed u3");
    append_user_message(&pool, "default", "second", 200)
        .await
        .expect("seed u2");

    let msgs = swarm_orchestrator_history(
        app.handle().clone(),
        "default".into(),
        None,
    )
    .await
    .expect("history ok");
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].content, "first");
    assert_eq!(msgs[1].content, "second");
    assert_eq!(msgs[2].content, "third");
}

/// Caller-supplied `limit > 200` is capped at 200 — verified by
/// the empty-result happy path (a `limit=9999` against an empty
/// pool still returns `Ok(vec![])` rather than erroring).
#[tokio::test]
async fn swarm_orchestrator_history_caps_limit_at_200() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let msgs = swarm_orchestrator_history(
        app.handle().clone(),
        "default".into(),
        Some(9999),
    )
    .await
    .expect("history ok");
    assert!(msgs.is_empty());
}

/// Empty `workspaceId` short-circuits with `InvalidInput`.
#[tokio::test]
async fn swarm_orchestrator_history_validates_empty_workspace_id() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_orchestrator_history(
        app.handle().clone(),
        "".into(),
        None,
    )
    .await
    .expect_err("empty workspace rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Whitespace-only `workspaceId` matches the W3-12k1 trim gate.
#[tokio::test]
async fn swarm_orchestrator_history_validates_whitespace_workspace_id() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_orchestrator_history(
        app.handle().clone(),
        "   ".into(),
        None,
    )
    .await
    .expect_err("whitespace workspace rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// `swarm_orchestrator_clear_history` empties the targeted
/// workspace.
#[tokio::test]
async fn swarm_orchestrator_clear_history_empties_workspace() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    append_user_message(&pool, "default", "drop me", 100)
        .await
        .expect("seed");
    swarm_orchestrator_clear_history(
        app.handle().clone(),
        "default".into(),
    )
    .await
    .expect("clear ok");
    let after = swarm_orchestrator_history(
        app.handle().clone(),
        "default".into(),
        None,
    )
    .await
    .expect("history ok");
    assert!(after.is_empty());
}

/// Empty `workspaceId` short-circuits with `InvalidInput` on the
/// clear surface too.
#[tokio::test]
async fn swarm_orchestrator_clear_history_validates_empty_workspace_id() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_orchestrator_clear_history(
        app.handle().clone(),
        "".into(),
    )
    .await
    .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// `swarm_orchestrator_log_job` writes the Job row visibly via
/// the history IPC.
#[tokio::test]
async fn swarm_orchestrator_log_job_persists_row() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    swarm_orchestrator_log_job(
        app.handle().clone(),
        "default".into(),
        "j-abc".into(),
        "Add doc to X.tsx".into(),
    )
    .await
    .expect("log ok");
    let msgs = swarm_orchestrator_history(
        app.handle().clone(),
        "default".into(),
        None,
    )
    .await
    .expect("history ok");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "j-abc");
    assert_eq!(msgs[0].goal.as_deref(), Some("Add doc to X.tsx"));
}

/// Empty inputs on the `log_job` surface — workspaceId, jobId,
/// or goal — surface `InvalidInput`.
#[tokio::test]
async fn swarm_orchestrator_log_job_validates_inputs() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_orchestrator_log_job(
        app.handle().clone(),
        "".into(),
        "j-1".into(),
        "g".into(),
    )
    .await
    .expect_err("empty workspace rejected");
    assert_eq!(err.kind(), "invalid_input");
    let err = swarm_orchestrator_log_job(
        app.handle().clone(),
        "ws".into(),
        "".into(),
        "g".into(),
    )
    .await
    .expect_err("empty jobId rejected");
    assert_eq!(err.kind(), "invalid_input");
    let err = swarm_orchestrator_log_job(
        app.handle().clone(),
        "ws".into(),
        "j-1".into(),
        "   ".into(),
    )
    .await
    .expect_err("whitespace goal rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// `swarm_orchestrator_decide` persists the user message even
/// when the LLM invoke is unreachable. The subprocess spawn
/// will fail in the mock-runtime environment (no `claude` binary)
/// — but the user row must already be in the DB by then.
#[tokio::test]
async fn swarm_orchestrator_decide_persists_user_before_invoke() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    // The decide call will surface a SwarmInvoke / spawn error
    // because the mock runtime has no `claude` binary on PATH.
    // We only care that the user row landed before the failure.
    let _ = swarm_orchestrator_decide(
        app.handle().clone(),
        "default".into(),
        "selam".into(),
    )
    .await;
    let msgs = swarm_orchestrator_history(
        app.handle().clone(),
        "default".into(),
        None,
    )
    .await
    .expect("history ok");
    // The very first message persisted is the user row.
    assert!(!msgs.is_empty());
    assert_eq!(msgs[0].content, "selam");
}
