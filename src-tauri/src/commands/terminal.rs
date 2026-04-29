//! `terminal:*` namespace.
//!
//! WP-W2-03 shipped DB-only stubs (`terminal:list`, `terminal:spawn`,
//! `terminal:kill`). WP-W2-06 replaces them with real PTY supervision
//! via [`crate::sidecar::terminal::TerminalRegistry`] and adds three
//! new commands that the frontend uses to drive a live shell:
//!
//! - `terminal:list`           → unchanged DB read
//! - `terminal:spawn(input)`   → fork a real PTY, return the row
//! - `terminal:write(paneId,d)`→ write bytes to the PTY stdin
//! - `terminal:resize(paneId,c,r)` → SIGWINCH equivalent
//! - `terminal:kill(id)`       → send SIGTERM (waiter task finalises)
//! - `terminal:lines(paneId,sinceSeq?)` → ring-buffer / DB scrollback
//!
//! All write-side commands take the registry through Tauri `State` and
//! the DB pool through Tauri `State`. Tests instantiate the registry
//! directly and use the underlying methods on `TerminalRegistry`.

use tauri::{AppHandle, Runtime, State};

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Pane, PaneLine, PaneSpawnInput};
use crate::sidecar::terminal::TerminalRegistry;

#[tauri::command]
#[specta::specta]
pub async fn terminal_list(pool: State<'_, DbPool>) -> Result<Vec<Pane>, AppError> {
    let rows = sqlx::query_as::<_, Pane>(
        "SELECT id, workspace, agent_kind, role, cwd, status, pid, started_at, closed_at \
         FROM panes ORDER BY started_at DESC",
    )
    .fetch_all(pool.inner())
    .await?;
    Ok(rows)
}

/// Fork a PTY, spawn the platform default shell (or `input.cmd` if
/// supplied), and return the freshly-inserted `Pane` row. The reader
/// and waiter tasks run on the tokio runtime; output streams via
/// `panes:{id}:line` Tauri events.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_spawn<R: Runtime>(
    app: AppHandle<R>,
    registry: State<'_, TerminalRegistry>,
    pool: State<'_, DbPool>,
    input: PaneSpawnInput,
) -> Result<Pane, AppError> {
    if input.cwd.trim().is_empty() {
        return Err(AppError::InvalidInput("cwd must not be empty".into()));
    }
    registry
        .inner()
        .clone()
        .spawn_pane(input, app, pool.inner().clone())
        .await
}

/// Send `data` (raw bytes — typically keystrokes) to the pane's PTY
/// stdin. Bytes are written verbatim; the shell handles line editing
/// and echoing.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_write(
    registry: State<'_, TerminalRegistry>,
    pane_id: String,
    data: String,
) -> Result<(), AppError> {
    registry
        .inner()
        .write_to_pane(&pane_id, data.as_bytes())
        .await
}

/// Resize the PTY (SIGWINCH on Unix, SetConsoleScreenBufferInfoEx on
/// Windows). Frontend callers must throttle to ≤10/sec per the
/// WP-W2-06 risk register; we apply the resize as-is.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_resize(
    registry: State<'_, TerminalRegistry>,
    pane_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), AppError> {
    registry.inner().resize_pane(&pane_id, cols, rows).await
}

/// Terminate the pane's child process. Idempotent — kill on an already-
/// closed pane updates `panes` row state without erroring (so the UI
/// can fire-and-forget on close). 404 if the pane id was never seen.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_kill(
    registry: State<'_, TerminalRegistry>,
    pool: State<'_, DbPool>,
    id: String,
) -> Result<(), AppError> {
    registry.inner().kill_pane(&id, pool.inner()).await
}

/// Return the most recent scrollback for a pane. Live panes read from
/// the in-memory ring; closed panes read from `pane_lines` (persisted
/// at pane close). `sinceSeq` (exclusive) lets the UI hydrate
/// incrementally on mount.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_lines(
    registry: State<'_, TerminalRegistry>,
    pool: State<'_, DbPool>,
    pane_id: String,
    since_seq: Option<i64>,
) -> Result<Vec<PaneLine>, AppError> {
    registry
        .inner()
        .pane_lines(&pane_id, since_seq, pool.inner())
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fresh_pool, seed_pane};
    use tauri::Manager as _;

    async fn mock_app_with_pool() -> (
        tauri::App<tauri::test::MockRuntime>,
        crate::db::DbPool,
        tempfile::TempDir,
    ) {
        let (pool, dir) = fresh_pool().await;
        let app = tauri::test::mock_builder()
            .manage(pool.clone())
            .manage(TerminalRegistry::new())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        (app, pool, dir)
    }

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
    async fn terminal_lines_for_unknown_pane_is_not_found() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let pool_state = app.state::<crate::db::DbPool>();
        let registry_state = app.state::<TerminalRegistry>();
        let err = terminal_lines(registry_state, pool_state, "p-missing".into(), None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }
}
