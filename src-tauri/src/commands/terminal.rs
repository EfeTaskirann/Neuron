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

use sqlx::Row;
use tauri::{AppHandle, Runtime, State};

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{ApprovalBanner, Pane, PaneLine, PaneSpawnInput};
use crate::sidecar::terminal::TerminalRegistry;

/// Materialise every pane with mock-shape parity. The five derived
/// fields (`tokens_in/out/cost_usd/uptime/approval`) are filled here
/// rather than via `sqlx::FromRow` because `approval` carries a
/// JSON-on-disk blob: the SQL column is `last_approval_json TEXT`
/// (migration 0003), which is decoded into `ApprovalBanner` only when
/// the pane is currently `awaiting_approval`. Idle / running / closed
/// panes get `Pane.approval = None` even if a stale blob lingers in
/// the column from a previous awaiting cycle.
///
/// `tokens_in/out/cost_usd/uptime` always project as SQL `NULL` — the
/// Charter Constraint #1 *display-derived carve-out* puts these in
/// the frontend hook (Week 3 will source `tokens_*` and `cost_usd`
/// from `runs_spans`; `uptime` stays display-derived).
#[tauri::command]
#[specta::specta]
pub async fn terminal_list(pool: State<'_, DbPool>) -> Result<Vec<Pane>, AppError> {
    let rows = sqlx::query(
        "SELECT id, workspace, agent_kind, role, cwd, status, pid, \
                started_at, closed_at, last_approval_json \
         FROM panes ORDER BY started_at DESC",
    )
    .fetch_all(pool.inner())
    .await?;

    let panes = rows
        .into_iter()
        .map(|row| {
            let status: String = row.get("status");
            let last_blob: Option<String> = row.get("last_approval_json");
            let approval = if status == "awaiting_approval" {
                last_blob
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<ApprovalBanner>(s).ok())
            } else {
                None
            };
            Pane {
                id: row.get("id"),
                workspace: row.get("workspace"),
                agent_kind: row.get("agent_kind"),
                role: row.get("role"),
                cwd: row.get("cwd"),
                status,
                pid: row.get("pid"),
                started_at: row.get("started_at"),
                closed_at: row.get("closed_at"),
                tokens_in: None,
                tokens_out: None,
                cost_usd: None,
                uptime: None,
                approval,
            }
        })
        .collect();
    Ok(panes)
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
