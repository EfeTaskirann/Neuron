//! Waiter task — observes child exit, finalises DB state.

use std::sync::{Arc, Mutex};

use portable_pty::Child;
use tauri::{AppHandle, Runtime};

use crate::db::DbPool;
use crate::error::AppError;
use crate::tuning::WAIT_POLL;

use super::super::{status, PaneState, RingLine, TerminalRegistry};
use super::emit::{emit_data, emit_line, LineEventPayload};

pub(in crate::sidecar::terminal) fn run_waiter<R: Runtime>(
    mut child: Box<dyn Child + Send + Sync>,
    state: Arc<Mutex<PaneState>>,
    app: AppHandle<R>,
    pool: DbPool,
    pane_id: String,
    registry: TerminalRegistry,
) {
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                // Once we observe a successful poll without exit, flip
                // STARTING → RUNNING so the frontend's "spawning" pill
                // resolves promptly. Idempotent.
                {
                    let mut guard = match state.lock() {
                        Ok(g) => g,
                        Err(_) => break portable_pty::ExitStatus::with_exit_code(1),
                    };
                    if guard.status == status::STARTING {
                        guard.status = status::RUNNING;
                        let pool = pool.clone();
                        let pane_id = pane_id.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = sqlx::query("UPDATE panes SET status = 'running' WHERE id = ?")
                                .bind(&pane_id)
                                .execute(&pool)
                                .await;
                        });
                    }
                }
                std::thread::sleep(WAIT_POLL);
            }
            Err(e) => {
                tracing::error!(pane_id = %pane_id, error = %e, "pane try_wait error");
                break portable_pty::ExitStatus::with_exit_code(1);
            }
        }
    };

    // Drop the writer + master so the reader task gets EOF. On
    // Windows ConPTY especially, the reader does not unblock until
    // every master/slave/writer reference is dropped — without this,
    // the `spawn_blocking` reader sits in `read()` indefinitely after
    // the child has already exited, leaking a thread and preventing
    // the registry from cleaning up the pane.
    {
        if let Ok(mut guard) = state.lock() {
            guard.writer.take();
            guard.master.take();
            guard.status = if exit_status.success() {
                status::SUCCESS
            } else {
                status::ERROR
            };
        }
    }

    // Synthesize a final `sys` line so the UI gets a definitive end
    // marker even if the shell printed nothing on its way out.
    let exit_text = format!("[exit {}]", exit_status.exit_code());
    let payload = LineEventPayload {
        k: "sys",
        text: exit_text.clone(),
        seq: 0,
    };
    emit_line(&app, &state, &pane_id, payload, &exit_text, "sys");
    // Mirror the end marker onto the raw interactive stream — the live
    // xterm view subscribes to `panes:{id}:data`, not the line channel,
    // so without this the exit line would only appear after a remount
    // (snapshot rehydration). Wrap in CRLF so it lands on its own row
    // regardless of whether the child's last output ended in a newline.
    emit_data(&app, &pane_id, &format!("\r\n{exit_text}\r\n"));

    let final_status = if exit_status.success() {
        status::SUCCESS
    } else {
        status::ERROR
    };

    // Flush ring → pane_lines and finalise panes.status/closed_at.
    let snapshot = {
        if let Ok(guard) = state.lock() {
            guard.ring.iter().cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    };
    let pane_id_async = pane_id.clone();
    let pool_async = pool.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = flush_ring_to_db(&pool_async, &pane_id_async, &snapshot).await {
            tracing::error!(
                pane_id = %pane_id_async,
                error = %e,
                "terminal ring flush failed"
            );
        }
        let res = sqlx::query(
            "UPDATE panes SET status = ?, closed_at = strftime('%s','now') \
             WHERE id = ? AND closed_at IS NULL",
        )
        .bind(final_status)
        .bind(&pane_id_async)
        .execute(&pool_async)
        .await;
        if let Err(e) = res {
            tracing::error!(
                pane_id = %pane_id_async,
                error = %e,
                "pane finalise failed"
            );
        }
    });

    // Drop the registry slot so subsequent `pane_lines` reads fall
    // through to the DB.
    let registry_clone = registry.clone();
    let pane_id_for_drop = pane_id.clone();
    tauri::async_runtime::spawn(async move {
        let mut panes = registry_clone.inner.panes.lock().await;
        panes.remove(&pane_id_for_drop);
    });
}

pub(in crate::sidecar::terminal) async fn flush_ring_to_db(
    pool: &DbPool,
    pane_id: &str,
    lines: &[RingLine],
) -> Result<(), AppError> {
    if lines.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for line in lines {
        // OR IGNORE + the UNIQUE(pane_id, seq) index (migration 0012)
        // make the flush idempotent — the waiter finalise and app-exit
        // shutdown_all can both snapshot the same ring on a close race.
        sqlx::query(
            "INSERT OR IGNORE INTO pane_lines (pane_id, seq, k, text) VALUES (?, ?, ?, ?)",
        )
        .bind(pane_id)
        .bind(line.seq)
        .bind(line.kind)
        .bind(&line.text)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
