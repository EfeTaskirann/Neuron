//! `mcp:*` namespace.
//!
//! - `mcp:list`        → `Server[]`
//! - `mcp:install`   `(id)` → `Server` — spawns the server, runs
//!                                       `tools/list`, persists the
//!                                       tools, flips `installed=1`.
//! - `mcp:uninstall` `(id)` → `Server` — drops tools + flips flag.
//! - `mcp:listTools` `(serverId)` → `Tool[]` — used by the agent
//!                                            runtime (WP-W2-04).
//! - `mcp:callTool`  `(serverId, name, args)` → `CallToolResult`
//!                  — re-spawns the server, calls one tool, returns
//!                  the MCP-spec response shape.
//!
//! ## Events
//!
//! Per ADR-0006, `mcp:install` emits `mcp:installed` and
//! `mcp:uninstall` emits `mcp:uninstalled` (logical names use `.` per
//! the ADR; Tauri 2.10 forbids `.` so the wire form swaps in `:`).
//! Frontend hooks invalidate the `['mcp']` cache key on either signal.
//!
//! ## Stubbed manifests
//!
//! Postgres / Browser / Slack / Vector DB are seeded as catalog rows
//! with no `spawn` recipe. Calling `mcp:install` on them surfaces
//! [`AppError::McpServerSpawnFailed`] so the frontend can render a
//! "Coming soon" CTA without the row mysteriously not toggling.

use tauri::{AppHandle, Emitter, Runtime, State};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::mcp::registry;
use crate::models::{CallToolResult, Server, Tool, ToolContent};

#[tauri::command]
#[specta::specta]
pub async fn mcp_list(pool: State<'_, DbPool>) -> Result<Vec<Server>, AppError> {
    let rows = sqlx::query_as::<_, Server>(
        "SELECT id, name, by, description, installs, rating, featured, installed \
         FROM servers ORDER BY featured DESC, installs DESC",
    )
    .fetch_all(pool.inner())
    .await?;
    Ok(rows)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_install<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    id: String,
) -> Result<Server, AppError> {
    let updated = registry::install(pool.inner(), &app, &id).await?;
    // Wire-name constant lives in `crate::events` (ADR-0006 § "Wire-
    // format substitution" — the logical `mcp.installed` is on-wire
    // `mcp:installed`).
    app.emit(events::MCP_INSTALLED, &updated)?;
    Ok(updated)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_uninstall<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    id: String,
) -> Result<Server, AppError> {
    let updated = registry::uninstall(pool.inner(), &id).await?;
    app.emit(events::MCP_UNINSTALLED, &updated)?;
    Ok(updated)
}

/// `mcp:listTools(serverId)` — return persisted tools for one server.
/// Empty list for uninstalled or unknown servers (the agent runtime
/// treats that as "no capabilities", which is the correct fallback).
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_list_tools(
    pool: State<'_, DbPool>,
    server_id: String,
) -> Result<Vec<Tool>, AppError> {
    registry::list_tools(pool.inner(), &server_id).await
}

/// `mcp:callTool(serverId, name, argsJson)` — execute one tool
/// against a freshly-spawned server. `argsJson` is the JSON-stringified
/// argument object; the server validates it against its `inputSchema`
/// and surfaces any schema violation as a structured `isError=true`
/// result.
///
/// Why a string instead of a typed object: the tool's argument shape
/// is whatever the MCP server declares, which the Rust side has no
/// compile-time knowledge of. Passing JSON as a `String` keeps the
/// `bindings.ts` signature clean (`argsJson: string`) — the caller is
/// expected to do `JSON.stringify(args)` at the boundary, and the
/// frontend `useMcpCallTool` hook (Week 3) will hide that cermony.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_call_tool<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    server_id: String,
    name: String,
    args_json: String,
) -> Result<CallToolResult, AppError> {
    let args: serde_json::Value = if args_json.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(&args_json).map_err(|e| {
            AppError::InvalidInput(format!("argsJson must be valid JSON: {e}"))
        })?
    };
    let out = registry::call_tool(pool.inner(), &app, &server_id, &name, args).await?;
    // Translate the client's `CallToolOutput` into the typed wire
    // shape. The two structs are isomorphic — keeping a separate
    // `models::*` form lets the frontend `bindings.ts` import the
    // shape without depending on `crate::mcp::*` internals.
    let content = out
        .content
        .into_iter()
        .map(|b| match b {
            crate::mcp::client::ContentBlock::Text { text } => ToolContent::Text { text },
            crate::mcp::client::ContentBlock::Other => ToolContent::Other,
        })
        .collect();
    Ok(CallToolResult {
        content,
        is_error: out.is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fresh_pool, mock_app_with_pool};
    // `app.state::<DbPool>()` and `app.handle()` come from `Manager`.
    use tauri::Manager as _;

    /// Seed all six manifest-derived servers (matches what
    /// `db::seed_mcp_servers` does at first run).
    async fn seed_all_manifest_rows(pool: &crate::db::DbPool) {
        let manifests = crate::mcp::manifests::load_all().expect("load manifests");
        for m in manifests {
            sqlx::query(
                "INSERT OR IGNORE INTO servers \
                 (id, name, by, description, installs, rating, featured, installed) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
            )
            .bind(&m.id)
            .bind(&m.name)
            .bind(&m.by)
            .bind(&m.description)
            .bind(m.installs)
            .bind(m.rating)
            .bind(m.featured as i64)
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
    async fn mcp_list_orders_featured_first() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        sqlx::query(
            "INSERT INTO servers (id, name, by, description, installs, rating, featured, installed) VALUES \
             ('s1','A','Anthropic','x',100,4.0,0,0), \
             ('s2','B','Anthropic','x',5000,4.5,1,1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = app.state::<crate::db::DbPool>();
        let out = mcp_list(state).await.expect("ok");
        assert_eq!(out.len(), 2);
        // Featured first.
        assert_eq!(out[0].id, "s2");
        assert!(out[0].featured);
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

    /// Stub manifest (browser) is seeded but install must surface a
    /// clear `mcp_server_spawn_failed` rather than silently flipping
    /// the flag.
    #[tokio::test]
    async fn mcp_install_stub_manifest_surface_spawn_failed() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        seed_all_manifest_rows(&pool).await;
        let handle = app.handle().clone();
        let state = app.state::<crate::db::DbPool>();
        let err = mcp_install(handle, state, "browser".into())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "mcp_server_spawn_failed");
        // The flag must remain 0 — no stub should land in installed=1
        // until WP-W3 wires its full pipeline.
        let installed: i64 =
            sqlx::query_scalar("SELECT installed FROM servers WHERE id='browser'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(installed, 0);
    }

    #[tokio::test]
    async fn mcp_uninstall_flips_installed_to_zero() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        sqlx::query(
            "INSERT INTO servers (id, name, by, description, installs, rating, featured, installed) \
             VALUES ('s1','A','Anthropic','x',1,4.0,0,1)",
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
}
