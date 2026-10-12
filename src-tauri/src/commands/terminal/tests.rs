//! Tests for the `terminal:*` command namespace.
//!
//! Split from the inline `#[cfg(test)] mod tests` block in the original
//! monolithic `terminal.rs` (T3-01/T3-03/T3-23 precedent). `super` here
//! resolves to `commands::terminal` (the package `mod.rs`), so
//! `use super::*;` still imports the command fns under test.

use super::*;
use crate::test_support::{mock_app_with_pool_and_terminal_registry as mock_app_with_pool, seed_pane};
use tauri::Manager as _;

#[tokio::test]
async fn terminal_list_empty_returns_empty_vec() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let state = app.state::<crate::db::DbPool>();
    let out = terminal_list(state).await.expect("ok");
    assert!(out.is_empty());
}

#[tokio::test]
async fn terminal_list_returns_seeded_panes() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_pane(&pool, "p1").await;
    let state = app.state::<crate::db::DbPool>();
    let out = terminal_list(state).await.expect("ok");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, "p1");
    assert_eq!(out[0].agent_kind, "shell");
    // Mock-shape parity (③+④): the five carve-out fields default
    // to `None` for an idle, never-awaited pane.
    assert!(out[0].tokens_in.is_none());
    assert!(out[0].tokens_out.is_none());
    assert!(out[0].cost_usd.is_none());
    assert!(out[0].uptime.is_none());
    assert!(out[0].approval.is_none());
}

/// Acceptance (③+④): a pane in `awaiting_approval` with a JSON
/// blob in `last_approval_json` surfaces a fully-populated
/// `Pane.approval` to the frontend.
#[tokio::test]
async fn pane_with_awaiting_approval_and_blob_returns_banner() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query(
        "INSERT INTO panes (id, workspace, agent_kind, role, cwd, status, pid, last_approval_json) \
         VALUES ('p-await', 'personal', 'claude-code', 'builder', '/tmp', \
                 'awaiting_approval', NULL, \
                 '{\"tool\":\"write_file\",\"target\":\"src/components/Button.tsx\",\"added\":47,\"removed\":12}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = app.state::<crate::db::DbPool>();
    let out = terminal_list(state).await.expect("ok");
    assert_eq!(out.len(), 1);
    let banner = out[0].approval.as_ref().expect("banner present");
    assert_eq!(banner.tool, "write_file");
    assert_eq!(banner.target, "src/components/Button.tsx");
    assert_eq!(banner.added, 47);
    assert_eq!(banner.removed, 12);
}

/// Acceptance (③+④): a pane that is NOT `awaiting_approval`
/// surfaces `Pane.approval = None` even if a stale blob is still
/// stored in `last_approval_json` from a previous awaiting cycle.
#[tokio::test]
async fn pane_with_idle_status_returns_none_approval() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query(
        "INSERT INTO panes (id, workspace, agent_kind, role, cwd, status, pid, last_approval_json) \
         VALUES ('p-idle', 'personal', 'claude-code', NULL, '/tmp', 'running', NULL, \
                 '{\"tool\":\"write_file\",\"target\":\"x\",\"added\":1,\"removed\":0}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = app.state::<crate::db::DbPool>();
    let out = terminal_list(state).await.expect("ok");
    assert_eq!(out.len(), 1);
    assert!(
        out[0].approval.is_none(),
        "non-awaiting pane must not surface a banner"
    );
}

/// Acceptance (③+④): an `awaiting_approval` pane with a NULL
/// `last_approval_json` (e.g. legacy row predating migration 0003,
/// or a serialiser failure during the AWAITING transition)
/// returns `Pane.approval = None` — never panics, never mis-decodes.
#[tokio::test]
async fn pane_with_null_blob_returns_none() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query(
        "INSERT INTO panes (id, workspace, agent_kind, role, cwd, status, pid, last_approval_json) \
         VALUES ('p-null', 'personal', 'gemini', NULL, '/tmp', 'awaiting_approval', NULL, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = app.state::<crate::db::DbPool>();
    let out = terminal_list(state).await.expect("ok");
    assert_eq!(out.len(), 1);
    assert!(out[0].approval.is_none());
}

#[tokio::test]
async fn terminal_spawn_rejects_empty_cwd() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let pool_state = app.state::<crate::db::DbPool>();
    let registry_state = app.state::<TerminalRegistry>();
    let err = terminal_spawn(
        app.handle().clone(),
        registry_state,
        pool_state,
        PaneSpawnInput {
            cwd: "".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");
}

#[tokio::test]
async fn terminal_kill_flips_status_to_closed_for_stub_rows() {
    // Stub rows from WP-W2-03 (no PTY ever attached) still need
    // `terminal:kill` to succeed and flip the status.
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_pane(&pool, "p1").await;
    let pool_state = app.state::<crate::db::DbPool>();
    let registry_state = app.state::<TerminalRegistry>();
    terminal_kill(registry_state, pool_state, "p1".into())
        .await
        .expect("ok");
    let (status, closed_at): (String, Option<i64>) =
        sqlx::query_as("SELECT status, closed_at FROM panes WHERE id = 'p1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "closed");
    assert!(closed_at.is_some());
}

#[tokio::test]
async fn terminal_kill_unknown_id_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let pool_state = app.state::<crate::db::DbPool>();
    let registry_state = app.state::<TerminalRegistry>();
    let err = terminal_kill(registry_state, pool_state, "no-such".into())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

#[tokio::test]
async fn terminal_write_unknown_pane_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry_state = app.state::<TerminalRegistry>();
    let err = terminal_write(registry_state, "p-missing".into(), "echo\n".into())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

#[tokio::test]
async fn terminal_resize_rejects_zero_dimensions() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry_state = app.state::<TerminalRegistry>();
    let err = terminal_resize(registry_state, "p-x".into(), 0, 24)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");
}

#[tokio::test]
async fn terminal_delete_removes_stub_row() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_pane(&pool, "p-gone").await;
    let pool_state = app.state::<crate::db::DbPool>();
    let registry_state = app.state::<TerminalRegistry>();
    terminal_delete(registry_state, pool_state, "p-gone".into())
        .await
        .expect("ok");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM panes WHERE id = 'p-gone'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn terminal_delete_unknown_id_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let pool_state = app.state::<crate::db::DbPool>();
    let registry_state = app.state::<TerminalRegistry>();
    let err = terminal_delete(registry_state, pool_state, "nope".into())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

#[tokio::test]
async fn terminal_purge_closed_drops_only_terminal_state_rows() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query(
        "INSERT INTO panes (id, workspace, agent_kind, role, cwd, status, pid) VALUES \
         ('p-live',   'personal', 'shell', NULL, '/tmp', 'running',           NULL), \
         ('p-await',  'personal', 'shell', NULL, '/tmp', 'awaiting_approval', NULL), \
         ('p-closed', 'personal', 'shell', NULL, '/tmp', 'closed',            NULL), \
         ('p-err',    'personal', 'shell', NULL, '/tmp', 'error',             NULL), \
         ('p-ok',     'personal', 'shell', NULL, '/tmp', 'success',           NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let pool_state = app.state::<crate::db::DbPool>();
    let removed = terminal_purge_closed(pool_state).await.expect("ok");
    assert_eq!(removed, 3, "should drop closed + error + success");

    let remaining: Vec<(String,)> = sqlx::query_as("SELECT id FROM panes ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let ids: Vec<&str> = remaining.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(ids, vec!["p-await", "p-live"]);
}

#[tokio::test]
async fn terminal_lines_for_unknown_pane_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let pool_state = app.state::<crate::db::DbPool>();
    let registry_state = app.state::<TerminalRegistry>();
    let err = terminal_lines(registry_state, pool_state, "p-missing".into(), None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}
