use super::*;
use crate::test_support::{fresh_pool, mock_app_with_pool};
// `app.state::<DbPool>()` and `app.handle()` come from `Manager`.
use tauri::Manager as _;

/// Seed the manifest-derived servers (matches what
/// `db::seed_mcp_servers` does at first run).
async fn seed_all_manifest_rows(pool: &crate::db::DbPool) {
    let manifests = crate::mcp::manifests::load_all().expect("load manifests");
    for m in manifests {
        sqlx::query(
            "INSERT OR IGNORE INTO servers (id, name, by, description, installed) \
             VALUES (?, ?, ?, ?, 0)",
        )
        .bind(&m.id)
        .bind(&m.name)
        .bind(&m.by)
        .bind(&m.description)
        .execute(pool)
        .await
        .expect("seed");
    }
}

#[tokio::test]
async fn mcp_list_empty_returns_empty_vec() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let state = app.state::<crate::db::DbPool>();
    let out = mcp_list(state).await.expect("ok");
    assert!(out.is_empty());
}

#[tokio::test]
async fn mcp_list_orders_by_name() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query(
        "INSERT INTO servers (id, name, by, description, installed) VALUES \
         ('s2','B','x','x',1), \
         ('s1','A','x','x',0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = app.state::<crate::db::DbPool>();
    let out = mcp_list(state).await.expect("ok");
    let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["A", "B"]);
    assert!(out[1].installed);
}

/// Acceptance: install on an unknown id surfaces NotFound (same
/// behaviour as the WP-03 stub).
#[tokio::test]
async fn mcp_install_unknown_id_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let err = mcp_install(handle, state, "no-such".into())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

#[tokio::test]
async fn mcp_uninstall_flips_installed_to_zero() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query(
        "INSERT INTO servers (id, name, by, description, installed) \
         VALUES ('s1','A','x','x',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let res = mcp_uninstall(handle, state, "s1".into()).await.expect("ok");
    assert!(!res.installed);
}

#[tokio::test]
async fn mcp_uninstall_unknown_id_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let err = mcp_uninstall(handle, state, "no-such".into())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

/// Acceptance: list_tools returns persisted rows.
#[tokio::test]
async fn mcp_list_tools_returns_persisted_rows() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_all_manifest_rows(&pool).await;
    sqlx::query(
        "INSERT INTO server_tools (server_id, name, description, input_schema_json) \
         VALUES ('filesystem','read_file','x','{}'), \
                ('filesystem','write_file','y','{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = app.state::<crate::db::DbPool>();
    let tools = mcp_list_tools(state, "filesystem".into()).await.unwrap();
    assert_eq!(tools.len(), 2);
    // ORDER BY name → alphabetical.
    assert_eq!(tools[0].name, "read_file");
    assert_eq!(tools[1].name, "write_file");
}

/// Acceptance: list_tools for an uninstalled server returns an
/// empty list (not an error). The agent runtime treats that as
/// "no capabilities", which is the right fallback.
#[tokio::test]
async fn mcp_list_tools_uninstalled_returns_empty() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_all_manifest_rows(&pool).await;
    let state = app.state::<crate::db::DbPool>();
    let tools = mcp_list_tools(state, "filesystem".into()).await.unwrap();
    assert!(tools.is_empty());
}

/// `call_tool` against an uninstalled server is a controlled
/// `Conflict`, distinct from a missing row's `NotFound`.
#[tokio::test]
async fn mcp_call_tool_uninstalled_returns_conflict() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_all_manifest_rows(&pool).await;
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let err = mcp_call_tool(
        handle,
        state,
        "filesystem".into(),
        "read_file".into(),
        "{}".into(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), "conflict");
}

/// Acceptance smoke: state persists across "restarts" — re-init
/// of the pool against the same DB file shows `installed=1` still.
/// We simulate the restart via a second pool against the same
/// path, mirroring how `db::init` re-opens on launch.
#[tokio::test]
async fn install_state_persists_across_pool_reopen() {
    let (pool, dir) = fresh_pool().await;
    seed_all_manifest_rows(&pool).await;
    // Pretend the install pipeline ran (we test the spawn boundary
    // separately in registry::tests).
    sqlx::query("UPDATE servers SET installed=1 WHERE id='filesystem'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO server_tools (server_id, name, description, input_schema_json) \
         VALUES ('filesystem','read_file','x','{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    // Re-open against the same temp file — first-launch idempotent
    // open per `db::open_pool_at`.
    let path = dir.path().join("neuron-test.db");
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool2 = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(opts)
        .await
        .unwrap();
    let installed: bool =
        sqlx::query_scalar("SELECT installed FROM servers WHERE id='filesystem'")
            .fetch_one(&pool2)
            .await
            .unwrap();
    assert!(installed);
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM server_tools WHERE server_id='filesystem'",
    )
    .fetch_one(&pool2)
    .await
    .unwrap();
    assert_eq!(count, 1);
}
