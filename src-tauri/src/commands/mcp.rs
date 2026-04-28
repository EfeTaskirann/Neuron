//! `mcp:*` namespace.
//!
//! - `mcp:list`      → `Server[]`
//! - `mcp:install`   `(id)` → `Server` // STUB — flips `installed=1` only
//! - `mcp:uninstall` `(id)` → `Server`
//!
//! ## Stubs
//!
//! WP-W2-03 § "Stubs in this WP":
//!
//!   `mcp:install` only flips `installed=1`. WP-05 adds tool registration.
//!
//! Real install also populates `server_tools`; this WP does not.
//!
//! ## Events
//!
//! Per ADR-0006, `mcp:install` emits `mcp:installed` and
//! `mcp:uninstall` emits `mcp:uninstalled` (logical names use `.` per
//! the ADR; Tauri 2.10 forbids `.` so the wire form swaps in `:`).
//! Frontend hooks invalidate the `['mcp']` cache key on either signal.

use tauri::{AppHandle, Emitter, Runtime, State};

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::Server;

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
    let updated = sqlx::query_as::<_, Server>(
        "UPDATE servers SET installed = 1 WHERE id = ? \
         RETURNING id, name, by, description, installs, rating, featured, installed",
    )
    .bind(&id)
    .fetch_optional(pool.inner())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Server {id} not found")))?;

    // Tauri 2.10 rejects `.` in event names; use the colon-separated
    // shape that the rest of the command surface follows. ADR-0006's
    // `mcp.installed` is `mcp:installed` on the wire.
    app.emit("mcp:installed", &updated)?;
    Ok(updated)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_uninstall<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    id: String,
) -> Result<Server, AppError> {
    let updated = sqlx::query_as::<_, Server>(
        "UPDATE servers SET installed = 0 WHERE id = ? \
         RETURNING id, name, by, description, installs, rating, featured, installed",
    )
    .bind(&id)
    .fetch_optional(pool.inner())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Server {id} not found")))?;

    app.emit("mcp:uninstalled", &updated)?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fresh_pool, seed_server_uninstalled};
    // `app.state::<DbPool>()` and `app.handle()` come from `Manager`.
    use tauri::Manager as _;

    async fn mock_app_with_pool() -> (
        tauri::App<tauri::test::MockRuntime>,
        crate::db::DbPool,
        tempfile::TempDir,
    ) {
        let (pool, dir) = fresh_pool().await;
        let app = tauri::test::mock_builder()
            .manage(pool.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        (app, pool, dir)
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

    #[tokio::test]
    async fn mcp_install_flips_installed_flag() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        seed_server_uninstalled(&pool).await;
        let handle = app.handle().clone();
        let state = app.state::<crate::db::DbPool>();

        let before: i64 = sqlx::query_scalar("SELECT installed FROM servers WHERE id='s3'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before, 0);

        let res = mcp_install(handle, state, "s3".into()).await.expect("ok");
        assert!(res.installed);

        let after: i64 = sqlx::query_scalar("SELECT installed FROM servers WHERE id='s3'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(after, 1);

        // STUB assertion: no server_tools rows registered.
        let tool_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM server_tools")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(tool_count, 0, "WP-05 will register tools; WP-03 must not");
    }

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
}
