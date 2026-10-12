//! `swarm:profiles_list` + `swarm:test_invoke` coverage (WP-W3-11).

use crate::commands::swarm::{swarm_profiles_list, swarm_test_invoke};
use crate::test_support::mock_app_with_pool;

/// Acceptance: on a fresh install (no `<app_data_dir>/agents/`),
/// `swarm:profiles_list` returns exactly the nine bundled
/// profiles (W3-12d added reviewer + integration-tester; W3-12f
/// added the coordinator brain; W3-12g renamed `reviewer` to
/// `backend-reviewer` and added `frontend-builder` +
/// `frontend-reviewer`; W3-12k1 added the orchestrator brain
/// inserted alphabetically between `integration-tester` and
/// `planner`) in deterministic alphabetical order.
#[tokio::test]
async fn profiles_list_returns_nine_bundled() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let summaries = swarm_profiles_list(app.handle().clone())
        .await
        .expect("ok");
    let ids: Vec<&str> =
        summaries.iter().map(|s| s.id.as_str()).collect();
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
    for s in &summaries {
        assert_eq!(
            s.source, "bundled",
            "fresh install: every profile must be bundled"
        );
    }
}

/// `swarm:test_invoke` rejects unknown profile ids before
/// spawning anything.
#[tokio::test]
async fn test_invoke_unknown_profile_returns_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_test_invoke(
        app.handle().clone(),
        "no-such-profile".into(),
        "hello".into(),
    )
    .await
    .expect_err("unknown profile rejected");
    assert_eq!(err.kind(), "not_found");
}

/// Empty profile id is `invalid_input`, not `not_found`.
#[tokio::test]
async fn test_invoke_empty_profile_id_rejected() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_test_invoke(
        app.handle().clone(),
        "".into(),
        "hello".into(),
    )
    .await
    .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Empty user message is `invalid_input`.
#[tokio::test]
async fn test_invoke_empty_message_rejected() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_test_invoke(
        app.handle().clone(),
        "scout".into(),
        "".into(),
    )
    .await
    .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}
