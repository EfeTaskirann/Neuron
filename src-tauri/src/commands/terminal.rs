//! `terminal:*` namespace.
//!
//! - `terminal:list`  → `Pane[]`
//! - `terminal:spawn` `(input)` → `Pane`         // STUB — inserts row, no PTY
//! - `terminal:kill`  `(id)` → `void`
//!
//! ## Stubs
//!
//! WP-W2-03 § "Stubs in this WP":
//!
//!   `terminal:spawn` inserts a pane row with `status='idle'`. WP-06
//!   adds the PTY.
//!
//! `terminal:kill` flips `status` to `closed` and stamps `closed_at`.
//! No real process is reaped — the supervisor lands in WP-06.

use tauri::State;
use ulid::Ulid;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Pane, PaneSpawnInput};

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

/// **STUB.** Inserts a row in `panes` with `status='idle'` and no PTY.
/// Real PTY supervision lands in WP-06.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_spawn(
    pool: State<'_, DbPool>,
    input: PaneSpawnInput,
) -> Result<Pane, AppError> {
    if input.agent_kind.trim().is_empty() {
        return Err(AppError::InvalidInput("agentKind must not be empty".into()));
    }
    if input.cwd.trim().is_empty() {
        return Err(AppError::InvalidInput("cwd must not be empty".into()));
    }

    let id = format!("p-{}", Ulid::new());
    let workspace = input.workspace.unwrap_or_else(|| "personal".into());

    let pane = sqlx::query_as::<_, Pane>(
        "INSERT INTO panes (id, workspace, agent_kind, role, cwd, status, pid) \
         VALUES (?, ?, ?, ?, ?, 'idle', NULL) \
         RETURNING id, workspace, agent_kind, role, cwd, status, pid, started_at, closed_at",
    )
    .bind(&id)
    .bind(&workspace)
    .bind(&input.agent_kind)
    .bind(&input.role)
    .bind(&input.cwd)
    .fetch_one(pool.inner())
    .await?;

    Ok(pane)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_kill(pool: State<'_, DbPool>, id: String) -> Result<(), AppError> {
    let res = sqlx::query(
        "UPDATE panes SET status = 'closed', closed_at = strftime('%s','now') \
         WHERE id = ? AND closed_at IS NULL",
    )
    .bind(&id)
    .execute(pool.inner())
    .await?;
    if res.rows_affected() == 0 {
        // Row may not exist or already be closed. Distinguish for the
        // frontend so it can show "already closed" vs "no such pane".
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM panes WHERE id = ?")
            .bind(&id)
            .fetch_optional(pool.inner())
            .await?;
        return Err(if exists.is_some() {
            AppError::Conflict(format!("Pane {id} already closed"))
        } else {
            AppError::NotFound(format!("Pane {id} not found"))
        });
    }
    Ok(())
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
    async fn terminal_spawn_inserts_idle_row_with_no_pty() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        let pane = terminal_spawn(
            state,
            PaneSpawnInput {
                agent_kind: "claude-code".into(),
                cwd: "/tmp/work".into(),
                role: Some("builder".into()),
                workspace: None,
            },
        )
        .await
        .expect("ok");

        // STUB assertions: status idle, no pid (no real PTY), DB row
        // exists, no pane_lines emitted.
        assert_eq!(pane.status, "idle");
        assert!(pane.pid.is_none());
        assert_eq!(pane.workspace, "personal");
        assert!(pane.id.starts_with("p-"));

        let count_panes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM panes")
            .fetch_one(&pool)
            .await
            .unwrap();
        let count_lines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pane_lines")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count_panes, 1);
        assert_eq!(count_lines, 0, "WP-06 emits PTY lines; WP-03 must not");
    }

    #[tokio::test]
    async fn terminal_spawn_rejects_empty_cwd() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        let err = terminal_spawn(
            state,
            PaneSpawnInput {
                agent_kind: "shell".into(),
                cwd: "".into(),
                role: None,
                workspace: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    #[tokio::test]
    async fn terminal_kill_flips_status_to_closed() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        seed_pane(&pool, "p1").await;
        let state = app.state::<crate::db::DbPool>();
        terminal_kill(state, "p1".into()).await.expect("ok");
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
        let state = app.state::<crate::db::DbPool>();
        let err = terminal_kill(state, "no-such".into()).await.unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }
}
