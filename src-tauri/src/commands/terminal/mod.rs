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
//!
//! ## Module layout
//!
//! Split from the original single-file `terminal.rs` into a package
//! (T3-01/T3-03/T3-23 precedent); behaviour is byte-for-byte preserved.
//! The command handlers live here in `mod`; the `#[cfg(test)]` block
//! moved verbatim to [`tests`]. The module path `commands::terminal`
//! is unchanged, so `bootstrap/specta.rs`'s `collect_commands!` list
//! and every `commands::terminal::terminal_*` reference stay identical.

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

/// Bulk-remove terminal-state panes (`closed` / `error` / `success`)
/// from the DB. `pane_lines` rows cascade via the FK. Returns the
/// number of pane rows actually deleted so the UI can surface a toast
/// like "removed 47 closed panes".
///
/// Safe to call any time: live panes (`running` / `idle` /
/// `awaiting_approval` / `starting`) are excluded by the status filter.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_purge_closed(pool: State<'_, DbPool>) -> Result<i64, AppError> {
    let res = sqlx::query(
        "DELETE FROM panes WHERE status IN ('closed', 'error', 'success')",
    )
    .execute(pool.inner())
    .await?;
    Ok(res.rows_affected() as i64)
}

/// Force-remove a single pane regardless of its current status. The
/// kill-then-delete sequence is what the UI's tab "✕" button needs:
/// `terminal:kill` only flips status to `closed` and leaves the row,
/// so without this command the tab strip never shrinks.
///
/// Live panes are killed first so the PTY + reader/waiter tasks
/// finalise cleanly (otherwise the DB row gets deleted out from under
/// the waiter, which would log a confused "pane vanished" warning).
/// 404 if the id was never seen by either the registry or the DB.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn terminal_delete(
    registry: State<'_, TerminalRegistry>,
    pool: State<'_, DbPool>,
    id: String,
) -> Result<(), AppError> {
    // Best-effort kill: ignore NotFound (already-dead pane) and let the
    // delete below speak the canonical "no such pane" error.
    match registry.inner().kill_pane(&id, pool.inner()).await {
        Ok(()) => {}
        Err(e) if e.kind() == "not_found" => {}
        Err(e) => return Err(e),
    }
    let res = sqlx::query("DELETE FROM panes WHERE id = ?")
        .bind(&id)
        .execute(pool.inner())
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Pane {id} not found")));
    }
    Ok(())
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
mod tests;
